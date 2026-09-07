//! The transaction a `TxRecord` holds, decoded once for the renderers that need its fields.
//!
//! The chain stores what was submitted: EIP-2718 bytes for a signed transaction, the fields
//! themselves for an unsigned one. The mirror reports both in tinybar (`hbar=true`, the default,
//! `openapi.yml:2384`), so the weibar amounts a signed envelope carries are converted here.
//! JSON-RPC renders the same records in weibar and reads the envelope itself.

use alloy_consensus::{Transaction as _, TxEnvelope};
use alloy_eips::eip2718::Decodable2718 as _;
use alloy_primitives::{Address, Bytes, Signature, U256};

use crate::evm::units::Tinybar;
use crate::state::{TxBody, TxRecord};

/// One transaction as it was submitted, in the chain's own units.
pub struct Submitted {
    /// EIP-2718 type: 0 legacy, 1 access-list, 2 dynamic-fee.
    pub tx_type: u8,
    /// Sender nonce.
    pub nonce: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas price in tinybar. For a 1559 transaction, the max fee.
    pub gas_price: Tinybar,
    /// Priority fee in tinybar, when the envelope carried one.
    pub priority_fee: Option<Tinybar>,
    /// Value in tinybar.
    pub value: Tinybar,
    /// Callee; `None` for a contract creation.
    pub to: Option<Address>,
    /// Calldata or init code.
    pub input: Bytes,
    /// Chain id, absent on a pre-EIP-155 legacy transaction.
    pub chain_id: Option<u64>,
    /// The signature, absent for `eth_sendTransaction`.
    pub signature: Option<Signature>,
}

/// Decode `tx` into its fields. Only transactions this chain already accepted are stored, so the
/// weibar amounts here have been through `Tinybar::from_weibar_{exact,floor}` once already at
/// submission (`evm::decode_signed`); a conversion that failed there never reached a `TxRecord`.
pub fn decode(tx: &TxRecord) -> Submitted {
    match &tx.body {
        TxBody::Signed(raw) => match TxEnvelope::decode_2718(&mut &raw[..]) {
            Ok(envelope) => Submitted {
                tx_type: envelope.tx_type() as u8,
                nonce: envelope.nonce(),
                gas_limit: envelope.gas_limit(),
                gas_price: tinybar(U256::from(envelope.max_fee_per_gas())),
                priority_fee: envelope
                    .max_priority_fee_per_gas()
                    .map(|fee| tinybar(U256::from(fee))),
                value: tinybar(envelope.value()),
                to: envelope.to(),
                input: envelope.input().clone(),
                chain_id: envelope.chain_id(),
                signature: Some(*envelope.signature()),
            },
            Err(_) => empty(),
        },
        TxBody::Unsigned(tx) => Submitted {
            tx_type: 0,
            nonce: tx.nonce,
            gas_limit: tx.gas_limit,
            gas_price: Tinybar(tx.gas_price),
            priority_fee: None,
            value: Tinybar(tx.value),
            to: tx.to,
            input: tx.input.clone(),
            chain_id: None,
            signature: None,
        },
    }
}

/// Weibar to tinybar. See the invariant on [`decode`]: the range check already passed once.
fn tinybar(weibar: U256) -> Tinybar {
    Tinybar::from_weibar_floor(weibar).unwrap_or_default()
}

/// What a renderer sees for bytes that no longer decode. Unreachable: the chain stores only
/// transactions it executed.
fn empty() -> Submitted {
    Submitted {
        tx_type: 0,
        nonce: 0,
        gas_limit: 0,
        gas_price: Tinybar(0),
        priority_fee: None,
        value: Tinybar(0),
        to: None,
        input: Bytes::new(),
        chain_id: None,
        signature: None,
    }
}
