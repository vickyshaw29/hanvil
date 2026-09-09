//! EVM execution on revm. The EVM's native unit is the tinybar, as on Hedera; `units.rs` is the
//! only place that knows about weibar. Nothing here is async and nothing here touches `Chain`.

pub mod units;

use std::convert::Infallible;

use alloy_consensus::{Transaction as _, TxEnvelope, transaction::SignerRecoverable as _};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{Address, U256};
use revm::context::{Context, TxEnv};
use revm::context_interface::result::{EVMError, ExecutionResult, InvalidTransaction};
use revm::database::CacheDB;
use revm::database_interface::EmptyDB;
use revm::primitives::hardfork::SpecId;
use revm::state::EvmState;
use revm::{ExecuteEvm, MainBuilder, MainContext};

use units::{Tinybar, UnitError};

/// Hedera's EVM is Cancun-level: PUSH0, transient storage, no blobs.
pub const SPEC: SpecId = SpecId::CANCUN;

/// Block-level inputs for one execution.
#[derive(Clone, Copy, Debug)]
pub struct BlockInput {
    /// Height the transaction executes in.
    pub number: u64,
    /// Block timestamp in seconds.
    pub timestamp: u64,
    /// Block gas limit.
    pub gas_limit: u64,
    /// Network gas price in tinybar; every transaction pays at least this.
    pub base_fee: u64,
    /// Where fees go: the long-zero address of 0.0.98.
    pub beneficiary: Address,
}

/// A mined transaction is validated in full; a read-only call skips balance, nonce and fee checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// `eth_sendRawTransaction`, `eth_sendTransaction`.
    Transaction,
    /// `eth_call`, `eth_estimateGas`.
    Call,
}

/// Why a transaction was refused before any code ran.
#[derive(Debug, thiserror::Error)]
pub enum Rejected {
    /// Nonce below the account's.
    #[error("nonce too low: transaction has {tx}, account is at {state}")]
    NonceTooLow {
        /// Nonce in the transaction.
        tx: u64,
        /// Nonce the account expects.
        state: u64,
    },
    /// Nonce above the account's.
    #[error("nonce too high: transaction has {tx}, account is at {state}")]
    NonceTooHigh {
        /// Nonce in the transaction.
        tx: u64,
        /// Nonce the account expects.
        state: u64,
    },
    /// Balance cannot cover `gas_limit * gas_price + value`.
    #[error("insufficient funds: gas * price + value needs {need} tinybar, balance is {have}")]
    InsufficientFunds {
        /// Tinybar required.
        need: U256,
        /// Tinybar available.
        have: U256,
    },
    /// Offered gas price is below the network gas price.
    #[error("gas price below the network gas price of {base_fee} tinybar")]
    GasPriceTooLow {
        /// The network price.
        base_fee: u64,
    },
    /// Transaction signed for another chain.
    #[error("chain id mismatch: transaction says {tx:?}, this network is {expected}")]
    ChainId {
        /// Chain id in the transaction.
        tx: Option<u64>,
        /// Chain id of this network.
        expected: u64,
    },
    /// Gas limit above what the network accepts for one transaction. Wording and quoting are the
    /// relay's (`docs/design/batch-request.md:157`): the request in hex, the maximum in decimal.
    #[error("Transaction gas limit '{tx:#x}' exceeds block gas limit '{max}'")]
    GasLimitTooHigh {
        /// Gas the transaction asked for.
        tx: u64,
        /// Most the network accepts.
        max: u64,
    },
    /// Anything else revm refuses.
    #[error("{0}")]
    Other(String),
}

/// Why raw transaction bytes could not become an executable transaction.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    /// Not a valid EIP-2718 envelope.
    #[error("transaction bytes could not be decoded: {0}")]
    Envelope(String),
    /// Signature does not recover to an address.
    #[error("signature could not be recovered: {0}")]
    Signature(String),
    /// Blob and set-code transactions are not part of Hedera's EVM.
    #[error("transaction type {0} is not supported; use legacy, EIP-2930 or EIP-1559")]
    TxType(u8),
    /// Value or gas price not representable in tinybar.
    #[error(transparent)]
    Units(#[from] UnitError),
}

/// The result of running a transaction together with the state it would commit.
pub struct Executed {
    /// revm's verdict.
    pub result: ExecutionResult,
    /// Accounts touched, ready for `DatabaseCommit`.
    pub state: EvmState,
}

/// A decoded, sender-recovered transaction in EVM units.
pub struct Decoded {
    /// The envelope, kept for rendering.
    pub envelope: TxEnvelope,
    /// Recovered sender.
    pub from: Address,
    /// Ready to execute.
    pub env: TxEnv,
    /// Max fee per gas in tinybar.
    pub gas_price: Tinybar,
}

/// Decode EIP-2718 bytes, recover the signer and convert weibar to tinybar.
pub fn decode_signed(raw: &[u8]) -> Result<Decoded, DecodeError> {
    let envelope =
        TxEnvelope::decode_2718(&mut &raw[..]).map_err(|e| DecodeError::Envelope(e.to_string()))?;
    let tx_type = envelope.tx_type() as u8;
    if tx_type > 2 {
        return Err(DecodeError::TxType(tx_type));
    }
    let from = envelope
        .recover_signer()
        .map_err(|e| DecodeError::Signature(e.to_string()))?;
    let value = Tinybar::from_weibar_exact(envelope.value())?;
    let gas_price = Tinybar::from_weibar_floor(U256::from(envelope.max_fee_per_gas()))?;
    let priority = envelope
        .max_priority_fee_per_gas()
        .map(|p| Tinybar::from_weibar_floor(U256::from(p)))
        .transpose()?;

    let mut builder = TxEnv::builder()
        .tx_type(Some(tx_type))
        .caller(from)
        .nonce(envelope.nonce())
        .gas_limit(envelope.gas_limit())
        .gas_price(u128::from(gas_price.0))
        .kind(envelope.kind())
        .value(value.to_evm())
        .data(envelope.input().clone())
        .chain_id(envelope.chain_id());
    if let Some(list) = envelope.access_list() {
        builder = builder.access_list(list.clone());
    }
    if let Some(tip) = priority {
        builder = builder.gas_priority_fee(Some(u128::from(tip.0)));
    }
    let env = builder
        .build()
        .map_err(|e| DecodeError::Envelope(format!("{e:?}")))?;
    Ok(Decoded {
        envelope,
        from,
        env,
        gas_price,
    })
}

/// Run one transaction against `db`. Nothing is committed; the caller decides.
pub fn execute(
    db: &mut CacheDB<EmptyDB>,
    chain_id: u64,
    block: &BlockInput,
    mode: Mode,
    tx: TxEnv,
) -> Result<Executed, Rejected> {
    let owned = std::mem::take(db);
    let ctx = Context::mainnet()
        .with_db(owned)
        .modify_cfg_chained(|cfg| {
            cfg.chain_id = chain_id;
            cfg.spec = SPEC;
            if mode == Mode::Call {
                cfg.disable_balance_check = true;
                cfg.disable_nonce_check = true;
                cfg.disable_base_fee = true;
                cfg.disable_block_gas_limit = true;
            }
        })
        .modify_block_chained(|b| {
            b.number = U256::from(block.number);
            b.timestamp = U256::from(block.timestamp);
            b.gas_limit = block.gas_limit;
            b.basefee = block.base_fee;
            b.beneficiary = block.beneficiary;
        });
    let mut evm = ctx.build_mainnet();
    let gas_limit = tx.gas_limit;
    let outcome = evm.transact(tx);
    *db = evm.ctx.journaled_state.database;
    match outcome {
        Ok(run) => Ok(Executed {
            result: run.result,
            state: run.state,
        }),
        Err(e) => Err(rejection(e, chain_id, block, gas_limit)),
    }
}

fn rejection(
    error: EVMError<Infallible>,
    expected_chain: u64,
    block: &BlockInput,
    gas_limit: u64,
) -> Rejected {
    match error {
        EVMError::Transaction(invalid) => match invalid {
            InvalidTransaction::NonceTooLow { tx, state } => Rejected::NonceTooLow { tx, state },
            InvalidTransaction::NonceTooHigh { tx, state } => Rejected::NonceTooHigh { tx, state },
            InvalidTransaction::LackOfFundForMaxFee { fee, balance } => {
                Rejected::InsufficientFunds {
                    need: *fee,
                    have: *balance,
                }
            }
            InvalidTransaction::GasPriceLessThanBasefee => Rejected::GasPriceTooLow {
                base_fee: block.base_fee,
            },
            InvalidTransaction::InvalidChainId => Rejected::ChainId {
                tx: None,
                expected: expected_chain,
            },
            InvalidTransaction::CallerGasLimitMoreThanBlock => Rejected::GasLimitTooHigh {
                tx: gas_limit,
                max: block.gas_limit,
            },
            other => Rejected::Other(other.to_string()),
        },
        other => Rejected::Other(other.to_string()),
    }
}

/// Runtime bytecode that always reverts with `Error(string)` carrying `reason`.
///
/// Etched at an address a caller expects to be a contract, so the call fails loudly and every
/// client decodes the reason — viem and ethers read `Error(string)`, and so does
/// [`revert_reason`]. Hedera's system contracts are not emulated, and returning success with
/// empty data (what an empty address does) would let a caller believe an HTS call worked.
pub fn revert_stub(reason: &str) -> alloy_primitives::Bytes {
    // Error(string): selector ‖ offset ‖ length ‖ utf-8 padded to a 32-byte boundary.
    let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
    data.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
    data.extend_from_slice(&U256::from(reason.len()).to_be_bytes::<32>());
    data.extend_from_slice(reason.as_bytes());
    while (data.len() - 4) % 32 != 0 {
        data.push(0);
    }

    // CODECOPY(dest=0, offset=DATA_OFFSET, len) then REVERT(0, len). Both pop their arguments
    // top-first, so each is pushed in reverse.
    const DATA_OFFSET: u16 = 15;
    let len = data.len() as u16;
    let mut code = Vec::with_capacity(DATA_OFFSET as usize + data.len());
    code.push(0x61); // PUSH2 len
    code.extend_from_slice(&len.to_be_bytes());
    code.push(0x61); // PUSH2 DATA_OFFSET
    code.extend_from_slice(&DATA_OFFSET.to_be_bytes());
    code.extend_from_slice(&[0x60, 0x00]); // PUSH1 0
    code.push(0x39); // CODECOPY
    code.push(0x61); // PUSH2 len
    code.extend_from_slice(&len.to_be_bytes());
    code.extend_from_slice(&[0x60, 0x00]); // PUSH1 0
    code.push(0xfd); // REVERT
    debug_assert_eq!(code.len(), DATA_OFFSET as usize);
    code.extend_from_slice(&data);
    code.into()
}

/// Human-readable reason from revert data: `Error(string)` and `Panic(uint256)`.
pub fn revert_reason(data: &[u8]) -> Option<String> {
    const ERROR_SELECTOR: [u8; 4] = [0x08, 0xc3, 0x79, 0xa0];
    const PANIC_SELECTOR: [u8; 4] = [0x4e, 0x48, 0x7b, 0x71];
    if data.len() >= 4 + 32 + 32 && data[..4] == ERROR_SELECTOR {
        let len = U256::from_be_slice(&data[36..68]).to::<usize>();
        let text = data.get(68..68 + len)?;
        return Some(String::from_utf8_lossy(text).into_owned());
    }
    if data.len() == 36 && data[..4] == PANIC_SELECTOR {
        let code = U256::from_be_slice(&data[4..36]);
        return Some(format!("panic code {code:#x}"));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cast mktx --private-key 0x105d05… --chain 298 --legacy --nonce 0 --gas-price 710000000000
    /// --gas-limit 21000 --value 10000000000 0x…03ea` (foundry 1.8.1).
    const LEGACY: &str = "0xf86b8085a54f4c3c008252089400000000000000000000000000000000000003ea8502540be40080820278a05cdae3a91a661323014df84e1dd7237d54c08b90231f8b9bd0cf1fc3bd542afaa04b4264f936be6811e7a19868ed2eae39480fcd84297d0dc87c37aea6d3b6e96e";
    /// Same transaction as EIP-1559 with a zero priority fee.
    const EIP1559: &str = "0x02f86e82012a808085a54f4c3c008252089400000000000000000000000000000000000003ea8502540be40080c080a0026f9dfcd7f8bf87a1000f902e2ba80a07eab91f171796a1f72136a1fc13360aa044fb18398fbde3aeafe721cc55ea4db0ea033d56e805bd344dff57fcaae1d8ab";
    const SIGNER: &str = "0x67d8d32e9bf1a9968a5ff53b87d777aa8ebbee69";

    fn raw(hex_text: &str) -> Vec<u8> {
        hex::decode(&hex_text[2..]).unwrap()
    }

    #[test]
    fn legacy_transaction_recovers_the_cast_signer() {
        let decoded = decode_signed(&raw(LEGACY)).unwrap();
        assert_eq!(format!("{:#x}", decoded.from), SIGNER);
        assert_eq!(decoded.env.nonce, 0);
        assert_eq!(
            decoded.env.value,
            U256::from(1),
            "10^10 weibar is one tinybar"
        );
        assert_eq!(decoded.gas_price, Tinybar(71));
        assert_eq!(decoded.env.gas_price, 71);
        assert_eq!(decoded.env.chain_id, Some(298));
        assert_eq!(
            format!("{:#x}", decoded.envelope.tx_hash()),
            "0x014c237608269cb95c7254a6312bc295da1843c0b3d07e8cfa185f0372532cf6"
        );
    }

    #[test]
    fn eip1559_transaction_recovers_the_cast_signer() {
        let decoded = decode_signed(&raw(EIP1559)).unwrap();
        assert_eq!(format!("{:#x}", decoded.from), SIGNER);
        assert_eq!(decoded.env.gas_price, 71);
        assert_eq!(decoded.env.gas_priority_fee, Some(0));
        assert_eq!(decoded.env.tx_type, 2);
    }

    #[test]
    fn fractional_tinybar_value_is_refused() {
        // Same legacy transaction with value 0x2540be401 would need re-signing; instead check the
        // conversion rule directly.
        assert!(Tinybar::from_weibar_exact(U256::from(10_000_000_001u64)).is_err());
    }

    /// The rejection has to name the price the network actually charges: it is the number the
    /// caller has to raise their gas price to, and it used to be reported as zero.
    #[test]
    fn a_gas_price_under_the_base_fee_names_the_network_price() {
        let block = BlockInput {
            number: 1,
            timestamp: 1_700_000_000,
            gas_limit: 30_000_000,
            base_fee: 71,
            beneficiary: Address::ZERO,
        };
        let tx = TxEnv::builder()
            .tx_type(Some(0))
            .caller(SIGNER.parse().unwrap())
            .nonce(0)
            .gas_limit(21_000)
            .gas_price(70)
            .kind(revm::primitives::TxKind::Call(Address::ZERO))
            .chain_id(Some(298))
            .build()
            .unwrap();

        let mut db = CacheDB::default();
        let Err(rejected) = execute(&mut db, 298, &block, Mode::Transaction, tx) else {
            panic!("70 tinybar is under the 71 the block charges");
        };
        assert!(
            matches!(rejected, Rejected::GasPriceTooLow { base_fee: 71 }),
            "{rejected:?}"
        );
        assert_eq!(
            rejected.to_string(),
            "gas price below the network gas price of 71 tinybar"
        );
    }

    #[test]
    fn revert_reason_decodes_error_string() {
        let mut data = vec![0x08, 0xc3, 0x79, 0xa0];
        data.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
        data.extend_from_slice(&U256::from(5).to_be_bytes::<32>());
        let mut text = b"hello".to_vec();
        text.resize(32, 0);
        data.extend_from_slice(&text);
        assert_eq!(revert_reason(&data).as_deref(), Some("hello"));
        assert_eq!(revert_reason(&[0x01, 0x02]), None);
    }
}
