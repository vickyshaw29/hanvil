//! The one chain state. Everything the three servers read or write lives here, behind one lock.
//! Nothing in this module is async; handlers take the lock, call one method, render, release.

pub mod accounts;
pub mod blocks;
pub mod hapi;
pub mod time;

use std::collections::{BTreeMap, HashMap, HashSet};

use alloy_primitives::{Address, B256, Bloom, Bytes, U256, keccak256};
use revm::DatabaseCommit as _;
use revm::context::TxEnv;
use revm::context_interface::result::{ExecutionResult, Output};
use revm::database::{AccountState, CacheDB, DbAccount};
use revm::database_interface::EmptyDB;
use revm::primitives::TxKind;
use revm::state::{AccountInfo, Bytecode};
use serde::{Deserialize, Serialize};

pub use accounts::{Account, EntityId, Key};
pub use blocks::{
    Block, CallRequest, LogFilter, Receipt, Slot, StoredLog, TxBody, TxRecord, UnsignedTx,
};
pub use hapi::{
    AccountRef, Body, Digest384, Record, Status, Topic, TopicMessage, Transaction, Transfer, TxId,
};
pub use time::{Clock, Timestamp};

use crate::evm::units::{Tinybar, long_zero_address};
use crate::evm::{self, BlockInput, Mode, Rejected};
use crate::keys;

/// Parameters that shape the initial state.
#[derive(Debug, Clone)]
pub struct Genesis {
    /// EVM chain id reported by `eth_chainId`.
    pub chain_id: u64,
    /// Predefined accounts per key type (1..=10).
    pub accounts_per_type: u8,
    /// Balance given to every predefined account.
    pub balance: Tinybar,
    /// Network gas price in tinybar per gas.
    pub gas_price: Tinybar,
    /// Wall-clock time at boot; the genesis block carries it.
    pub now: Timestamp,
}

/// Errors while building or mutating state.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A predefined key could not be parsed or derived.
    #[error(transparent)]
    Key(#[from] keys::Error),
    /// Raw transaction bytes were not usable.
    #[error(transparent)]
    Decode(#[from] evm::DecodeError),
    /// Refused before execution.
    #[error(transparent)]
    Rejected(#[from] Rejected),
    /// `eth_sendTransaction` from an address Hanvil holds no key for.
    #[error(
        "{0} is neither a predefined account nor impersonated; call anvil_impersonateAccount first"
    )]
    NotImpersonated(Address),
    /// A timestamp that would move the chain backwards.
    #[error("timestamp {requested} is not after the latest block's {latest}")]
    TimestampNotAfterLatest {
        /// What was asked for.
        requested: u64,
        /// What the head block has.
        latest: u64,
    },
    /// `eth_estimateGas` could not find a passing gas limit.
    #[error("execution fails at every gas limit up to {0}")]
    NoGasEstimate(u64),
    /// A state file could not be read or written.
    #[error("chain state: {0}")]
    State(String),
}

/// First entity id handed out to user accounts. Matches hiero-local-node.
pub const FIRST_USER_ID: u64 = 1002;
/// Treasury; the JSON-RPC relay's operator on hiero-local-node.
pub const TREASURY: EntityId = EntityId(2);
/// The single consensus node.
pub const NODE: EntityId = EntityId(3);
/// Fee collection account; EVM fees land here.
pub const FEE_COLLECTOR: EntityId = EntityId(98);
/// Hedera's HTS system contract, `0.0.359` — long-zero address `0x…0167`.
pub const HTS_SYSTEM_CONTRACT: EntityId = EntityId(359);
/// What a call to [`HTS_SYSTEM_CONTRACT`] reverts with.
pub const HTS_NOT_EMULATED: &str =
    "hanvil: HTS system contract not emulated; see README#system-contracts";
/// Hedera's exchange rate system contract, `0.0.360` — long-zero address `0x…0168`, the address
/// the mirror node's own manual test calls `tinycentsToTinybars(uint256)` on
/// (`hiero-mirror-node/docs/web3/README.md:59,69`).
pub const EXCHANGE_RATE_SYSTEM_CONTRACT: EntityId = EntityId(360);
/// What a call to [`EXCHANGE_RATE_SYSTEM_CONTRACT`] reverts with.
pub const EXCHANGE_RATE_NOT_EMULATED: &str =
    "hanvil: exchange rate system contract not emulated; see README#system-contracts";
/// Hedera's pseudorandom number generator, `0.0.361` — long-zero address `0x…0169`. HIP-351
/// (Final) puts it there: "the solidity precompiled contract is to reside at address `0x169`".
pub const PRNG_SYSTEM_CONTRACT: EntityId = EntityId(361);
/// What a call to [`PRNG_SYSTEM_CONTRACT`] reverts with.
pub const PRNG_NOT_EMULATED: &str =
    "hanvil: PRNG system contract not emulated; see README#system-contracts";
/// Every Hedera system contract Hanvil does not emulate, with the reason a call to it reverts
/// with. Genesis etches reverting bytecode at each: an address with no code is not an error in
/// the EVM, so a call to one succeeds and returns nothing, and a caller would read a token
/// operation, an exchange rate or a random seed as having worked.
const UNEMULATED_SYSTEM_CONTRACTS: [(EntityId, &str); 3] = [
    (HTS_SYSTEM_CONTRACT, HTS_NOT_EMULATED),
    (EXCHANGE_RATE_SYSTEM_CONTRACT, EXCHANGE_RATE_NOT_EMULATED),
    (PRNG_SYSTEM_CONTRACT, PRNG_NOT_EMULATED),
];
/// Most gas one transaction may ask for, and what a block reports as its limit. This is the
/// relay's `MAX_TRANSACTION_GAS_LIMIT` default (relay `docs/configuration.md:82`), which refuses
/// `eth_sendRawTransaction` above it and caps an `eth_call` asking for more; its rejection calls
/// this number the block gas limit (relay `docs/design/batch-request.md:157`), so blocks report
/// it too rather than advertising headroom the network will not accept.
pub const BLOCK_GAS_LIMIT: u64 = 15_000_000;
/// Fee charged for every HAPI transaction, whatever the body. Hanvil does not emulate Hedera's
/// fee schedule; this is one flat number, listed in the README under what is not emulated.
pub const HAPI_FEE: Tinybar = Tinybar(10_000);
/// Total supply, 50 billion HBAR, minted to the treasury.
const TOTAL_SUPPLY: Tinybar = Tinybar::from_hbar(50_000_000_000);
/// Seconds an entity lives before it must be renewed. Hedera's default, and what the mirror and
/// HAPI both report for an entity that set none. Hanvil never expires anything; the field exists
/// so a client that reads it sees a time in the future rather than the creation instant.
pub const AUTO_RENEW_PERIOD_SECS: u64 = 7_776_000;
/// The fixed rate `/api/v1/network/exchangerate` and every HAPI receipt report: 30,000 ℏ per
/// 360,000 ¢, or 1 ℏ = 12 ¢. Hanvil has no price feed and never expires the rate.
pub const HBAR_EQUIVALENT: u32 = 30_000;
/// Cent side of [`HBAR_EQUIVALENT`].
pub const CENT_EQUIVALENT: u32 = 360_000;
/// How far ahead of the head block the reported rate claims to be valid.
pub const EXCHANGE_RATE_VALID_SECS: u64 = 86_400;

/// Metadata for a contract entity; code and storage live in the EVM database. Read by the mirror
/// REST (`/contracts/{id}`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Contract {
    /// EVM address.
    pub address: Address,
    /// Block in which it was created.
    pub created_block: u64,
}

/// In-memory chain. Cloning it is how snapshots work, and `--state` / `--dump-state` are the
/// same fields written to a file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chain {
    chain_id: u64,
    gas_price: Tinybar,
    accounts: BTreeMap<EntityId, Account>,
    by_evm: HashMap<Address, EntityId>,
    contracts: BTreeMap<EntityId, Contract>,
    contract_by_evm: HashMap<Address, EntityId>,
    next_id: u64,
    db: CacheDB<EmptyDB>,
    blocks: Vec<Block>,
    txs: HashMap<B256, TxRecord>,
    time_offset: i64,
    next_timestamp: Option<u64>,
    last_consensus: Timestamp,
    impersonated: HashSet<Address>,
    topics: BTreeMap<EntityId, Topic>,
    hapi_records: Vec<Record>,
    hapi_by_id: HashMap<TxId, usize>,
    snapshots: BTreeMap<u64, Chain>,
    next_snapshot: u64,
}

impl Chain {
    /// Build genesis: system accounts, the predefined funded user accounts, block 0.
    pub fn genesis(genesis: &Genesis) -> Result<Self, Error> {
        let mut chain = Self {
            chain_id: genesis.chain_id,
            gas_price: genesis.gas_price,
            accounts: BTreeMap::new(),
            by_evm: HashMap::new(),
            contracts: BTreeMap::new(),
            contract_by_evm: HashMap::new(),
            next_id: FIRST_USER_ID,
            db: CacheDB::default(),
            blocks: Vec::new(),
            txs: HashMap::new(),
            time_offset: 0,
            next_timestamp: None,
            last_consensus: genesis.now,
            impersonated: HashSet::new(),
            topics: BTreeMap::new(),
            hapi_records: Vec::new(),
            hapi_by_id: HashMap::new(),
            snapshots: BTreeMap::new(),
            next_snapshot: 0,
        };
        let users =
            keys::predefined::accounts(genesis.accounts_per_type, genesis.balance, genesis.now)?;
        let funded = Tinybar(genesis.balance.0.saturating_mul(users.len() as u64));
        chain.insert_account(system_account(
            TREASURY,
            Some(keys::predefined::treasury_key()?),
            Tinybar(TOTAL_SUPPLY.0.saturating_sub(funded.0)),
            genesis.now,
        ));
        chain.insert_account(system_account(NODE, None, Tinybar(0), genesis.now));
        chain.insert_account(system_account(FEE_COLLECTOR, None, Tinybar(0), genesis.now));
        for account in users {
            chain.insert_account(account);
        }
        chain.next_id = chain.next_id.max(FIRST_USER_ID);
        for (entity, reason) in UNEMULATED_SYSTEM_CONTRACTS {
            chain.etch(long_zero_address(entity), evm::revert_stub(reason));
        }
        let genesis_hash = keccak256(format!("hanvil genesis chain {}", genesis.chain_id));
        chain.blocks.push(Block {
            number: 0,
            hash: genesis_hash,
            parent_hash: B256::ZERO,
            timestamp: genesis.now.secs,
            consensus_timestamp: genesis.now,
            gas_limit: BLOCK_GAS_LIMIT,
            gas_used: 0,
            base_fee: genesis.gas_price.0,
            transactions: Vec::new(),
            logs_bloom: Bloom::ZERO,
        });
        Ok(chain)
    }

    /// Read a chain written by [`Chain::to_json`]. The file decides the chain id and every
    /// account, so the genesis flags are not consulted.
    pub fn from_json(json: &str) -> Result<Self, Error> {
        serde_json::from_str(json).map_err(|e| Error::State(e.to_string()))
    }

    /// The whole chain as JSON, snapshots included, so `--state` restores what `evm_revert`
    /// could still reach.
    pub fn to_json(&self) -> Result<String, Error> {
        serde_json::to_string(self).map_err(|e| Error::State(e.to_string()))
    }

    fn insert_account(&mut self, account: Account) {
        self.by_evm.insert(account.evm_address(), account.id);
        self.by_evm
            .insert(long_zero_address(account.id), account.id);
        if account.id.0 >= FIRST_USER_ID {
            self.next_id = self.next_id.max(account.id.0 + 1);
        }
        self.accounts.insert(account.id, account);
    }

    fn allocate_id(&mut self) -> EntityId {
        let id = EntityId(self.next_id);
        self.next_id += 1;
        id
    }

    // ---- reads -------------------------------------------------------------------------------

    /// EVM chain id.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Network gas price in tinybar per gas.
    pub fn gas_price(&self) -> Tinybar {
        self.gas_price
    }

    /// Latest block number.
    pub fn block_number(&self) -> u64 {
        self.blocks.len() as u64 - 1
    }

    /// The head block.
    pub fn latest_block(&self) -> &Block {
        &self.blocks[self.blocks.len() - 1]
    }

    /// Block by height.
    pub fn block_by_number(&self, number: u64) -> Option<&Block> {
        self.blocks.get(usize::try_from(number).ok()?)
    }

    /// Block by hash. Linear; blocks are few.
    pub fn block_by_hash(&self, hash: &B256) -> Option<&Block> {
        self.blocks.iter().find(|b| &b.hash == hash)
    }

    /// A mined transaction.
    pub fn transaction(&self, hash: &B256) -> Option<&TxRecord> {
        self.txs.get(hash)
    }

    /// Every block, oldest first.
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }

    /// The genesis block's consensus timestamp: when this chain came into being.
    pub fn genesis_timestamp(&self) -> Timestamp {
        self.blocks[0].consensus_timestamp
    }

    /// Every mined transaction in consensus order.
    pub fn transactions(&self) -> impl Iterator<Item = &TxRecord> {
        self.blocks
            .iter()
            .flat_map(|block| block.transactions.iter())
            .filter_map(|hash| self.txs.get(hash))
    }

    /// Account by entity id.
    pub fn account(&self, id: EntityId) -> Option<&Account> {
        self.accounts.get(&id)
    }

    /// Entity id behind an EVM address, account or contract.
    pub fn entity_by_evm(&self, address: &Address) -> Option<EntityId> {
        self.by_evm
            .get(address)
            .or_else(|| self.contract_by_evm.get(address))
            .copied()
    }

    /// Contract metadata by entity id.
    pub fn contract(&self, id: EntityId) -> Option<&Contract> {
        self.contracts.get(&id)
    }

    /// Contract entity id for an EVM address.
    pub fn contract_id_by_evm(&self, address: &Address) -> Option<EntityId> {
        self.contract_by_evm.get(address).copied()
    }

    /// Account by alias or long-zero address.
    pub fn account_by_evm(&self, address: &Address) -> Option<&Account> {
        self.by_evm
            .get(address)
            .and_then(|id| self.accounts.get(id))
    }

    /// Balance in tinybar: from the account when one exists, else from the EVM database (contracts
    /// and addresses that only ever held value inside the EVM). Zero when unknown.
    pub fn balance_by_evm(&self, address: &Address) -> Tinybar {
        if let Some(account) = self.account_by_evm(address) {
            return account.balance;
        }
        self.db
            .cache
            .accounts
            .get(address)
            .map(|a| Tinybar::from_evm(a.info.balance))
            .unwrap_or_default()
    }

    /// Ethereum nonce.
    pub fn nonce_by_evm(&self, address: &Address) -> u64 {
        if let Some(account) = self.account_by_evm(address) {
            return account.nonce;
        }
        self.db
            .cache
            .accounts
            .get(address)
            .map(|a| a.info.nonce)
            .unwrap_or_default()
    }

    /// Deployed bytecode; empty when none.
    pub fn code_by_evm(&self, address: &Address) -> Bytes {
        self.db
            .cache
            .accounts
            .get(address)
            .and_then(|a| a.info.code.as_ref())
            .map(|c| c.original_bytes())
            .unwrap_or_default()
    }

    /// One storage slot.
    pub fn storage_at(&self, address: &Address, slot: Slot) -> U256 {
        self.db
            .cache
            .accounts
            .get(address)
            .and_then(|a| a.storage.get(&slot).copied())
            .unwrap_or_default()
    }

    /// Whether `anvil_impersonateAccount` was called for this address.
    pub fn is_impersonated(&self, address: &Address) -> bool {
        self.impersonated.contains(address)
    }

    /// Whether Hanvil holds this address's private key (a predefined dev account).
    pub fn holds_key_for(&self, address: &Address) -> bool {
        self.account_by_evm(address)
            .is_some_and(|a| a.private_key_hex.is_some())
    }

    /// Logs in `[from_block, to_block]` matching the filter, in block and log order.
    pub fn logs(&self, filter: &LogFilter) -> Vec<&StoredLog> {
        let to = filter.to_block.min(self.block_number());
        (filter.from_block..=to)
            .filter_map(|n| self.block_by_number(n))
            .flat_map(|b| b.transactions.iter())
            .filter_map(|h| self.txs.get(h))
            .flat_map(|tx| tx.receipt.logs.iter())
            .filter(|log| filter.matches(log))
            .collect()
    }

    /// Accounts grouped for the boot banner, in id order within each group. Only the predefined
    /// accounts, whose keys are public by design.
    pub fn accounts_by_group(&self) -> Vec<(&'static str, Vec<&Account>)> {
        let mut ecdsa = Vec::new();
        let mut alias = Vec::new();
        let mut ed = Vec::new();
        for account in self.accounts.values() {
            if account.private_key_hex.is_none() {
                continue;
            }
            match (&account.key, account.alias) {
                (Some(Key::EcdsaSecp256k1(_)), Some(_)) => alias.push(account),
                (Some(Key::EcdsaSecp256k1(_)), None) => ecdsa.push(account),
                (Some(Key::Ed25519(_)), _) => ed.push(account),
                (None, _) => {}
            }
        }
        vec![
            ("Accounts (ECDSA, long-zero address)", ecdsa),
            ("Accounts (ECDSA with EVM alias)", alias),
            ("Accounts (ED25519)", ed),
        ]
    }

    // ---- time --------------------------------------------------------------------------------

    /// Timestamp the next block will carry: the pending `evm_setNextBlockTimestamp` if any, else
    /// wall clock plus the `evm_increaseTime` offset; never earlier than the head block.
    fn next_block_timestamp(&mut self, now: Timestamp) -> u64 {
        let latest = self.latest_block().timestamp;
        let ts = match self.next_timestamp.take() {
            Some(pinned) => {
                // Later blocks continue from the pinned time, as Anvil does.
                self.time_offset = pinned as i64 - now.secs as i64;
                pinned
            }
            None => now.secs.saturating_add_signed(self.time_offset),
        };
        ts.max(latest)
    }

    /// `evm_increaseTime`: shift the clock by `seconds`; returns the total offset.
    pub fn increase_time(&mut self, seconds: u64) -> i64 {
        self.time_offset = self
            .time_offset
            .saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX));
        self.time_offset
    }

    /// `evm_setNextBlockTimestamp`.
    pub fn set_next_timestamp(&mut self, timestamp: u64) -> Result<(), Error> {
        let latest = self.latest_block().timestamp;
        if timestamp <= latest {
            return Err(Error::TimestampNotAfterLatest {
                requested: timestamp,
                latest,
            });
        }
        self.next_timestamp = Some(timestamp);
        Ok(())
    }

    // ---- execution ---------------------------------------------------------------------------

    fn block_input(&self, timestamp: u64) -> BlockInput {
        BlockInput {
            number: self.block_number() + 1,
            timestamp,
            gas_limit: BLOCK_GAS_LIMIT,
            base_fee: self.gas_price.0,
            beneficiary: long_zero_address(FEE_COLLECTOR),
        }
    }

    /// The database entry for an address, created if absent. `CacheDB::load_account` marks a fresh
    /// entry `NotExisting`, and revm then reads it as `None` whatever its fields say; flipping the
    /// state makes the balance and nonce written afterwards visible.
    fn db_account(&mut self, address: Address) -> &mut DbAccount {
        let Ok(slot) = self.db.load_account(address);
        if slot.account_state == AccountState::NotExisting {
            slot.account_state = AccountState::Touched;
        }
        slot
    }

    /// Write every account's authoritative balance and nonce into the EVM database.
    fn sync_accounts_into_db(&mut self) {
        let balances: Vec<(Address, Tinybar, u64)> = self
            .accounts
            .values()
            .map(|a| (a.evm_address(), a.balance, a.nonce))
            .collect();
        for (address, balance, nonce) in balances {
            let slot = self.db_account(address);
            slot.info.balance = balance.to_evm();
            slot.info.nonce = nonce;
        }
    }

    /// Read touched balances and nonces back from an execution, allocate ids for new contracts,
    /// and create hollow accounts for fresh addresses that received value.
    fn sync_db_into_accounts(&mut self, state: &revm::state::EvmState, created_at: Timestamp) {
        let block = self.block_number() + 1;
        let mut new_contracts = Vec::new();
        let mut hollow = Vec::new();
        for (address, touched) in state {
            if !touched.is_touched() {
                continue;
            }
            let has_code = touched.info.code.as_ref().is_some_and(|c| !c.is_empty());
            if let Some(id) = self.by_evm.get(address).copied() {
                if let Some(account) = self.accounts.get_mut(&id) {
                    account.balance = Tinybar::from_evm(touched.info.balance);
                    account.nonce = touched.info.nonce;
                }
            } else if self.contract_by_evm.contains_key(address) {
                // Contract balances are read from the database on demand.
            } else if has_code {
                new_contracts.push(*address);
            } else if touched.info.balance > U256::ZERO {
                hollow.push((*address, Tinybar::from_evm(touched.info.balance)));
            }
        }
        for address in new_contracts {
            let id = self.allocate_id();
            self.contracts.insert(
                id,
                Contract {
                    address,
                    created_block: block,
                },
            );
            self.contract_by_evm.insert(address, id);
        }
        for (address, balance) in hollow {
            let id = self.allocate_id();
            self.insert_account(Account {
                id,
                key: None,
                alias: Some(address),
                balance,
                nonce: 0,
                deleted: false,
                memo: String::new(),
                created_at,
                private_key_hex: None,
            });
        }
    }

    /// revm burns the base fee (EIP-1559); Hedera does not. Move it to the fee collector so the
    /// supply is conserved.
    fn credit_burned_base_fee(&mut self, gas_used: u64) {
        let burned = Tinybar(gas_used.saturating_mul(self.gas_price.0));
        if let Some(collector) = self.accounts.get_mut(&FEE_COLLECTOR) {
            collector.balance = Tinybar(collector.balance.0.saturating_add(burned.0));
        }
    }

    /// `eth_sendRawTransaction`: decode, execute, mine one block, store the receipt.
    pub fn send_raw(&mut self, raw: Bytes, now: Timestamp) -> Result<B256, Error> {
        let decoded = evm::decode_signed(&raw)?;
        let hash = *decoded.envelope.tx_hash();
        let from = decoded.from;
        let effective_gas_price = decoded.gas_price.0.min(
            self.gas_price
                .0
                .saturating_add(priority_fee(&decoded.env).unwrap_or(0)),
        );
        self.execute_and_mine(
            hash,
            TxBody::Signed(raw),
            from,
            decoded.env,
            effective_gas_price,
            now,
        )
    }

    /// `eth_sendTransaction` for predefined and impersonated senders.
    pub fn send_unsigned(&mut self, tx: UnsignedTx, now: Timestamp) -> Result<B256, Error> {
        if !self.holds_key_for(&tx.from) && !self.is_impersonated(&tx.from) {
            return Err(Error::NotImpersonated(tx.from));
        }
        let mut preimage = Vec::with_capacity(64);
        preimage.extend_from_slice(b"hanvil unsigned");
        preimage.extend_from_slice(tx.from.as_slice());
        preimage.extend_from_slice(&tx.nonce.to_be_bytes());
        preimage.extend_from_slice(&self.chain_id.to_be_bytes());
        let hash = keccak256(preimage);
        let env = TxEnv::builder()
            .tx_type(Some(0))
            .caller(tx.from)
            .nonce(tx.nonce)
            .gas_limit(tx.gas_limit)
            .gas_price(u128::from(tx.gas_price))
            .kind(match tx.to {
                Some(to) => TxKind::Call(to),
                None => TxKind::Create,
            })
            .value(U256::from(tx.value))
            .data(tx.input.clone())
            .chain_id(Some(self.chain_id))
            .build()
            .map_err(|e| Error::Rejected(Rejected::Other(format!("{e:?}"))))?;
        self.execute_and_mine(
            hash,
            TxBody::Unsigned(tx.clone()),
            tx.from,
            env,
            tx.gas_price,
            now,
        )
    }

    fn execute_and_mine(
        &mut self,
        hash: B256,
        body: TxBody,
        from: Address,
        env: TxEnv,
        effective_gas_price: u64,
        now: Timestamp,
    ) -> Result<B256, Error> {
        if env.gas_limit > BLOCK_GAS_LIMIT {
            return Err(Error::Rejected(Rejected::GasLimitTooHigh {
                tx: env.gas_limit,
                max: BLOCK_GAS_LIMIT,
            }));
        }
        let timestamp = self.next_block_timestamp(now);
        let consensus_timestamp = self.next_consensus(Timestamp {
            secs: timestamp,
            nanos: now.nanos,
        });
        let block = self.block_input(timestamp);
        self.sync_accounts_into_db();
        let executed = evm::execute(&mut self.db, self.chain_id, &block, Mode::Transaction, env)?;
        self.sync_db_into_accounts(&executed.state, consensus_timestamp);
        self.db.commit(executed.state);

        let gas_used = executed.result.tx_gas_used();
        self.credit_burned_base_fee(gas_used);
        let receipt = receipt_from(&executed.result, effective_gas_price);
        tracing::info!(
            kind = match &body {
                TxBody::Signed(_) => "eth_sendRawTransaction",
                TxBody::Unsigned(_) => "eth_sendTransaction",
            },
            hash = %hash,
            from = %from,
            gas = gas_used,
            status = if receipt.success { "success" } else { "reverted" },
            block = block.number,
            "transaction"
        );
        let record = TxRecord {
            hash,
            body,
            from,
            block_number: block.number,
            block_hash: B256::ZERO,
            index: 0,
            consensus_timestamp,
            receipt,
        };
        self.seal_block(timestamp, consensus_timestamp, vec![record]);
        Ok(hash)
    }

    /// Append a block holding `records`, filling in block-relative fields.
    fn seal_block(&mut self, timestamp: u64, consensus: Timestamp, mut records: Vec<TxRecord>) {
        let number = self.block_number() + 1;
        let parent_hash = self.latest_block().hash;
        let hashes: Vec<B256> = records.iter().map(|r| r.hash).collect();
        let hash = Block::compute_hash(number, &parent_hash, timestamp, &hashes);
        let mut gas_used = 0;
        let mut bloom = Bloom::ZERO;
        let mut log_index = 0;
        for (index, record) in records.iter_mut().enumerate() {
            record.block_number = number;
            record.block_hash = hash;
            record.index = index as u64;
            gas_used += record.receipt.gas_used;
            bloom.accrue_bloom(&record.receipt.logs_bloom);
            for log in &mut record.receipt.logs {
                log.log_index = log_index;
                log.tx_hash = record.hash;
                log.tx_index = index as u64;
                log.block_number = number;
                log.block_hash = hash;
                log_index += 1;
            }
        }
        self.blocks.push(Block {
            number,
            hash,
            parent_hash,
            timestamp,
            consensus_timestamp: consensus,
            gas_limit: BLOCK_GAS_LIMIT,
            gas_used,
            base_fee: self.gas_price.0,
            transactions: hashes,
            logs_bloom: bloom,
        });
        for record in records {
            self.txs.insert(record.hash, record);
        }
    }

    /// `evm_mine`: an empty block.
    pub fn mine_empty(&mut self, now: Timestamp) -> &Block {
        let timestamp = self.next_block_timestamp(now);
        let consensus = self.next_consensus(Timestamp {
            secs: timestamp,
            nanos: now.nanos,
        });
        self.seal_block(timestamp, consensus, Vec::new());
        self.latest_block()
    }

    /// Consensus timestamps identify a transaction on Hedera, so they are unique and increasing.
    /// A fixed clock, or two transactions inside one nanosecond, would otherwise produce two
    /// records with the same id.
    fn next_consensus(&mut self, at: Timestamp) -> Timestamp {
        let next = if at > self.last_consensus {
            at
        } else {
            self.last_consensus.next_nano()
        };
        self.last_consensus = next;
        next
    }

    fn call_env(&self, request: &CallRequest) -> TxEnv {
        TxEnv::builder()
            .tx_type(Some(0))
            .caller(request.from.unwrap_or_default())
            .nonce(self.nonce_by_evm(&request.from.unwrap_or_default()))
            .gas_limit(request.gas.unwrap_or(BLOCK_GAS_LIMIT).min(BLOCK_GAS_LIMIT))
            .gas_price(u128::from(request.gas_price.unwrap_or(0)))
            .kind(match request.to {
                Some(to) => TxKind::Call(to),
                None => TxKind::Create,
            })
            .value(U256::from(request.value))
            .data(request.input.clone())
            .chain_id(Some(self.chain_id))
            .build_fill()
    }

    /// `eth_call`: execute against the head state without committing.
    pub fn call(
        &mut self,
        request: &CallRequest,
        now: Timestamp,
    ) -> Result<ExecutionResult, Error> {
        let timestamp = self.latest_block().timestamp.max(now.secs);
        let block = self.block_input(timestamp);
        self.sync_accounts_into_db();
        let env = self.call_env(request);
        Ok(evm::execute(&mut self.db, self.chain_id, &block, Mode::Call, env)?.result)
    }

    /// `eth_estimateGas`: the smallest gas limit at which the call succeeds, found by bisection
    /// between the gas the unconstrained run used and the block limit (the 63/64 rule makes the
    /// first number insufficient for calls that make calls).
    pub fn estimate_gas(&mut self, request: &CallRequest, now: Timestamp) -> Result<u64, Error> {
        let cap = request.gas.unwrap_or(BLOCK_GAS_LIMIT).min(BLOCK_GAS_LIMIT);
        let unconstrained = self.call(
            &CallRequest {
                gas: Some(cap),
                ..request.clone()
            },
            now,
        )?;
        if !unconstrained.is_success() {
            return Ok(cap);
        }
        let mut lo = unconstrained.tx_gas_used();
        let mut hi = cap;
        let succeeds = |chain: &mut Self, gas: u64| -> Result<bool, Error> {
            let run = chain.call(
                &CallRequest {
                    gas: Some(gas),
                    ..request.clone()
                },
                now,
            )?;
            Ok(run.is_success())
        };
        if succeeds(self, lo)? {
            return Ok(lo);
        }
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if succeeds(self, mid)? {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        if succeeds(self, hi)? {
            Ok(hi)
        } else {
            Err(Error::NoGasEstimate(cap))
        }
    }

    // ---- HAPI --------------------------------------------------------------------------------

    /// A topic by id.
    pub fn topic(&self, id: EntityId) -> Option<&Topic> {
        self.topics.get(&id)
    }

    /// The record left by a HAPI transaction id.
    pub fn hapi_record(&self, id: &TxId) -> Option<&Record> {
        self.hapi_by_id
            .get(id)
            .and_then(|i| self.hapi_records.get(*i))
    }

    /// Every HAPI record, oldest first.
    pub fn hapi_records(&self) -> impl Iterator<Item = &Record> {
        self.hapi_records.iter()
    }

    /// Whether this transaction id already reached consensus (`DUPLICATE_TRANSACTION`).
    pub fn has_transaction_id(&self, id: &TxId) -> bool {
        self.hapi_by_id.contains_key(id)
    }

    /// Apply one decoded HAPI transaction. The fee is charged first and stays charged even when
    /// the body fails, as on Hedera; every body validates completely before it mutates, so a
    /// failure leaves nothing half-applied. Always produces a record — prechecks that would stop
    /// a transaction reaching consensus run in `hapi/`, before this is called.
    pub fn apply_hapi(&mut self, tx: Transaction, now: Timestamp) -> Record {
        // The EVM path allocates its own consensus timestamp when it mines. One transaction has
        // one timestamp, so an `ethereumTransaction` takes the EVM's rather than a second one.
        let consensus = match tx.body {
            Body::Ethereum { .. } => now,
            _ => self.next_consensus(now),
        };
        let mut record = Record {
            id: tx.id,
            kind: tx.body.kind(),
            consensus_timestamp: consensus,
            status: Status::Success,
            charged_fee: HAPI_FEE,
            max_fee: tx.max_fee,
            memo: tx.memo,
            hash: tx.hash,
            valid_duration_seconds: tx.valid_duration_seconds,
            transfers: Vec::new(),
            created_account: None,
            created_alias: None,
            created_topic: None,
            topic_sequence_number: 0,
            topic_running_hash: Vec::new(),
            ethereum_hash: Vec::new(),
        };
        self.move_tinybar(tx.id.payer, FEE_COLLECTOR, HAPI_FEE, &mut record.transfers);

        if let Err(status) = self.apply_body(tx.body, &mut record, consensus) {
            record.status = status;
        }
        record.transfers = aggregate(record.transfers);

        self.hapi_by_id.insert(record.id, self.hapi_records.len());
        self.hapi_records.push(record.clone());
        record
    }

    /// The body half of [`Chain::apply_hapi`]. Validates, then mutates.
    fn apply_body(
        &mut self,
        body: Body,
        record: &mut Record,
        consensus: Timestamp,
    ) -> Result<(), Status> {
        match body {
            Body::CreateAccount {
                key,
                initial_balance,
                alias,
                memo,
            } => {
                if alias.is_some_and(|alias| self.by_evm.contains_key(&alias)) {
                    return Err(Status::AliasAlreadyAssigned);
                }
                if self.balance_of(record.id.payer) < initial_balance {
                    return Err(Status::InsufficientPayerBalance);
                }
                let id = self.allocate_id();
                self.insert_account(Account {
                    id,
                    key: Some(key),
                    alias,
                    balance: Tinybar(0),
                    nonce: 0,
                    deleted: false,
                    memo,
                    created_at: consensus,
                    private_key_hex: None,
                });
                self.move_tinybar(record.id.payer, id, initial_balance, &mut record.transfers);
                record.created_account = Some(id);
                record.created_alias = alias;
                Ok(())
            }

            Body::Transfer { amounts } => self.apply_transfer(amounts, record, consensus),

            Body::Delete {
                account,
                transfer_to,
            } => {
                if account == transfer_to {
                    return Err(Status::InvalidTransferAccountId);
                }
                if !self.accounts.contains_key(&account) {
                    return Err(Status::InvalidAccountId);
                }
                if !self.is_live(account) {
                    return Err(Status::AccountDeleted);
                }
                if !self.is_live(transfer_to) {
                    return Err(Status::InvalidTransferAccountId);
                }
                let balance = self.balance_of(account);
                self.move_tinybar(account, transfer_to, balance, &mut record.transfers);
                if let Some(target) = self.accounts.get_mut(&account) {
                    target.deleted = true;
                }
                Ok(())
            }

            Body::CreateTopic {
                memo,
                admin_key,
                submit_key,
                auto_renew_period,
                auto_renew_account,
            } => {
                let id = self.allocate_id();
                self.topics.insert(
                    id,
                    Topic {
                        id,
                        memo,
                        admin_key,
                        submit_key,
                        sequence_number: 0,
                        running_hash: Digest384::default(),
                        created_at: consensus,
                        auto_renew_period,
                        auto_renew_account,
                        messages: Vec::new(),
                    },
                );
                record.created_topic = Some(id);
                Ok(())
            }

            Body::SubmitMessage { topic, message } => {
                if message.is_empty() {
                    return Err(Status::InvalidTopicMessage);
                }
                let payer = record.id.payer;
                let Some(target) = self.topics.get_mut(&topic) else {
                    return Err(Status::InvalidTopicId);
                };
                let sequence_number = target.sequence_number + 1;
                let running_hash = hapi::running_hash_v3(
                    &target.running_hash,
                    payer,
                    topic,
                    consensus,
                    sequence_number,
                    &message,
                );
                target.sequence_number = sequence_number;
                target.running_hash = running_hash;
                target.messages.push(TopicMessage {
                    sequence_number,
                    message,
                    running_hash,
                    consensus_timestamp: consensus,
                    payer,
                });
                record.topic_sequence_number = sequence_number;
                record.topic_running_hash = running_hash.as_bytes().to_vec();
                Ok(())
            }

            Body::Ethereum { rlp } => match self.send_raw(Bytes::from(rlp), consensus) {
                Ok(hash) => {
                    record.ethereum_hash = hash.to_vec();
                    let mined = self.transaction(&hash);
                    let succeeded = mined.is_some_and(|tx| tx.receipt.success);
                    if let Some(at) = mined.map(|tx| tx.consensus_timestamp) {
                        record.consensus_timestamp = at;
                    }
                    if succeeded {
                        Ok(())
                    } else {
                        // The mirror reports a reverting EVM call this way; the record still
                        // exists and the hash still resolves.
                        Err(Status::ContractRevertExecuted)
                    }
                }
                Err(_) => {
                    // Nothing was mined, so nothing allocated a timestamp; this record still
                    // needs one that identifies it.
                    record.consensus_timestamp = self.next_consensus(consensus);
                    Err(Status::InvalidTransactionBody)
                }
            },
        }
    }

    /// `cryptoTransfer`'s HBAR list: sums to zero, no account twice, every debit funded.
    fn apply_transfer(
        &mut self,
        amounts: Vec<(AccountRef, i64)>,
        record: &mut Record,
        consensus: Timestamp,
    ) -> Result<(), Status> {
        if amounts
            .iter()
            .map(|(_, amount)| *amount as i128)
            .sum::<i128>()
            != 0
        {
            return Err(Status::InvalidAccountAmounts);
        }

        // Resolve every reference first: a credit to an unknown alias creates a hollow account,
        // which is how a fresh EVM key gets funded, but only once the whole list is valid.
        let mut resolved: Vec<(EntityId, i64)> = Vec::with_capacity(amounts.len());
        let mut created: Vec<(Address, i64)> = Vec::new();
        for (account, amount) in &amounts {
            match account {
                AccountRef::Id(id) => {
                    if !self.is_live(*id) {
                        return Err(if self.accounts.contains_key(id) {
                            Status::AccountDeleted
                        } else {
                            Status::InvalidAccountId
                        });
                    }
                    resolved.push((*id, *amount));
                }
                AccountRef::Alias(address) => match self.by_evm.get(address).copied() {
                    Some(id) => {
                        if !self.is_live(id) {
                            return Err(Status::AccountDeleted);
                        }
                        resolved.push((id, *amount));
                    }
                    // A credit to an unknown alias creates one hollow account, so naming it
                    // twice is the same repeat the resolved list rejects below.
                    None if *amount > 0 => {
                        if created.iter().any(|(seen, _)| seen == address) {
                            return Err(Status::AccountRepeatedInAccountAmounts);
                        }
                        created.push((*address, *amount));
                    }
                    None => return Err(Status::InvalidAccountId),
                },
            }
        }

        let mut seen: Vec<EntityId> = resolved.iter().map(|(id, _)| *id).collect();
        seen.sort_unstable();
        if seen.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Status::AccountRepeatedInAccountAmounts);
        }
        for (id, amount) in &resolved {
            if *amount < 0 && self.balance_of(*id).0 < amount.unsigned_abs() {
                return Err(Status::InsufficientAccountBalance);
            }
        }

        for (id, amount) in resolved {
            self.adjust(id, amount);
            record.transfers.push(Transfer {
                account: id,
                amount,
            });
        }
        for (address, amount) in created {
            let id = self.allocate_id();
            self.insert_account(Account {
                id,
                key: None,
                alias: Some(address),
                balance: Tinybar(amount.unsigned_abs()),
                nonce: 0,
                deleted: false,
                memo: String::new(),
                created_at: consensus,
                private_key_hex: None,
            });
            record.transfers.push(Transfer {
                account: id,
                amount,
            });
        }
        Ok(())
    }

    /// Balance in tinybar; zero for an id with no account.
    fn balance_of(&self, id: EntityId) -> Tinybar {
        self.accounts.get(&id).map_or(Tinybar(0), |a| a.balance)
    }

    /// Whether the id names an account that exists and was not deleted.
    fn is_live(&self, id: EntityId) -> bool {
        self.accounts.get(&id).is_some_and(|a| !a.deleted)
    }

    /// Add a signed tinybar amount to an account that is known to exist.
    fn adjust(&mut self, id: EntityId, amount: i64) {
        if let Some(account) = self.accounts.get_mut(&id) {
            account.balance = if amount < 0 {
                Tinybar(account.balance.0.saturating_sub(amount.unsigned_abs()))
            } else {
                Tinybar(account.balance.0.saturating_add(amount.unsigned_abs()))
            };
        }
    }

    /// Move tinybar between two existing accounts and append both legs to a transfer list.
    fn move_tinybar(
        &mut self,
        from: EntityId,
        to: EntityId,
        amount: Tinybar,
        transfers: &mut Vec<Transfer>,
    ) {
        if amount.0 == 0 {
            return;
        }
        let signed = amount.0 as i64;
        self.adjust(from, -signed);
        self.adjust(to, signed);
        transfers.push(Transfer {
            account: from,
            amount: -signed,
        });
        transfers.push(Transfer {
            account: to,
            amount: signed,
        });
    }

    // ---- cheats ------------------------------------------------------------------------------

    /// `evm_snapshot`: clone the whole chain under a fresh id.
    pub fn snapshot(&mut self) -> u64 {
        let id = self.next_snapshot;
        self.next_snapshot += 1;
        let snapshots = std::mem::take(&mut self.snapshots);
        let copy = self.clone();
        self.snapshots = snapshots;
        self.snapshots.insert(id, copy);
        id
    }

    /// `evm_revert`: restore snapshot `id` and forget it and every later one. `false` if unknown.
    pub fn revert(&mut self, id: u64) -> bool {
        let mut snapshots = std::mem::take(&mut self.snapshots);
        let Some(restored) = snapshots.remove(&id) else {
            self.snapshots = snapshots;
            return false;
        };
        snapshots.retain(|k, _| *k < id);
        let next_snapshot = self.next_snapshot;
        *self = restored;
        self.snapshots = snapshots;
        self.next_snapshot = next_snapshot;
        true
    }

    /// `anvil_setBalance`. Creates a hollow account for an unknown address.
    pub fn set_balance(&mut self, address: Address, balance: Tinybar, now: Timestamp) {
        if let Some(id) = self.by_evm.get(&address).copied() {
            if let Some(account) = self.accounts.get_mut(&id) {
                account.balance = balance;
            }
            return;
        }
        if self.contract_by_evm.contains_key(&address) {
            self.db_account(address).info.balance = balance.to_evm();
            return;
        }
        let id = self.allocate_id();
        self.insert_account(Account {
            id,
            key: None,
            alias: Some(address),
            balance,
            nonce: 0,
            deleted: false,
            memo: String::new(),
            created_at: now,
            private_key_hex: None,
        });
    }

    /// Write bytecode at an address without registering a contract entity. Used for the system
    /// contracts, which already have entity ids of their own.
    fn etch(&mut self, address: Address, code: Bytes) {
        let bytecode = Bytecode::new_raw(code);
        let slot = self.db_account(address);
        slot.info = AccountInfo {
            balance: slot.info.balance,
            nonce: slot.info.nonce,
            ..AccountInfo::default()
        }
        .with_code(bytecode);
    }

    /// `anvil_setCode`. Registers a contract entity when the address had no code.
    pub fn set_code(&mut self, address: Address, code: Bytes) {
        let bytecode = Bytecode::new_raw(code);
        let slot = self.db_account(address);
        let info = AccountInfo {
            balance: slot.info.balance,
            nonce: slot.info.nonce,
            ..AccountInfo::default()
        }
        .with_code(bytecode);
        slot.info = info;
        if !self.contract_by_evm.contains_key(&address) && !self.by_evm.contains_key(&address) {
            let id = self.allocate_id();
            self.contracts.insert(
                id,
                Contract {
                    address,
                    created_block: self.block_number(),
                },
            );
            self.contract_by_evm.insert(address, id);
        }
    }

    /// `anvil_setNonce`.
    pub fn set_nonce(&mut self, address: Address, nonce: u64) {
        if let Some(account) = self
            .by_evm
            .get(&address)
            .copied()
            .and_then(|id| self.accounts.get_mut(&id))
        {
            account.nonce = nonce;
        }
        self.db_account(address).info.nonce = nonce;
    }

    /// `anvil_setStorageAt`.
    pub fn set_storage(&mut self, address: Address, slot: Slot, value: U256) {
        self.db_account(address).storage.insert(slot, value);
    }

    /// `anvil_impersonateAccount`.
    pub fn impersonate(&mut self, address: Address) {
        self.impersonated.insert(address);
    }

    /// `anvil_stopImpersonatingAccount`.
    pub fn stop_impersonating(&mut self, address: Address) {
        self.impersonated.remove(&address);
    }
}

/// One entry per account, in id order: the mirror lists a transaction's transfers that way, and
/// the fee leg and a body leg can name the same account.
fn aggregate(transfers: Vec<Transfer>) -> Vec<Transfer> {
    let mut totals: BTreeMap<EntityId, i64> = BTreeMap::new();
    for transfer in transfers {
        *totals.entry(transfer.account).or_default() += transfer.amount;
    }
    totals
        .into_iter()
        .map(|(account, amount)| Transfer { account, amount })
        .collect()
}

fn system_account(
    id: EntityId,
    key: Option<Key>,
    balance: Tinybar,
    created_at: Timestamp,
) -> Account {
    Account {
        id,
        key,
        alias: None,
        balance,
        nonce: 0,
        deleted: false,
        memo: String::new(),
        created_at,
        private_key_hex: None,
    }
}

fn priority_fee(env: &TxEnv) -> Option<u64> {
    env.gas_priority_fee.and_then(|p| u64::try_from(p).ok())
}

fn receipt_from(result: &ExecutionResult, effective_gas_price: u64) -> Receipt {
    let logs: Vec<StoredLog> = result
        .logs()
        .iter()
        .map(|log| StoredLog {
            address: log.address,
            topics: log.data.topics().to_vec(),
            data: log.data.data.clone(),
            log_index: 0,
            tx_hash: B256::ZERO,
            tx_index: 0,
            block_number: 0,
            block_hash: B256::ZERO,
        })
        .collect();
    let mut bloom = Bloom::ZERO;
    for log in result.logs() {
        bloom.accrue_log(log);
    }
    let (success, contract_address, output, halt_reason) = match result {
        ExecutionResult::Success { output, .. } => (
            true,
            match output {
                Output::Create(_, address) => *address,
                Output::Call(_) => None,
            },
            output.data().clone(),
            None,
        ),
        ExecutionResult::Revert { output, .. } => (false, None, output.clone(), None),
        ExecutionResult::Halt { reason, .. } => {
            (false, None, Bytes::new(), Some(format!("{reason:?}")))
        }
    };
    Receipt {
        success,
        gas_used: result.tx_gas_used(),
        effective_gas_price,
        contract_address,
        logs,
        logs_bloom: bloom,
        output,
        halt_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain() -> Chain {
        Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 2,
            balance: Tinybar::from_hbar(100),
            gas_price: Tinybar(71),
            now: Timestamp::from_secs(1_700_000_000),
        })
        .expect("genesis")
    }

    /// A call to an unemulated system contract must fail loudly. Before genesis etched stubs
    /// there, each was an empty address, and the EVM answers a call to one with success and no
    /// return data — a token operation, a price lookup or a seed would read as having worked.
    #[test]
    fn a_call_to_an_unemulated_system_contract_reverts_with_a_reason() {
        for (entity, reason) in UNEMULATED_SYSTEM_CONTRACTS {
            let mut c = chain();
            // Any selector; the stub reverts before reading calldata.
            let request = CallRequest {
                to: Some(long_zero_address(entity)),
                input: Bytes::from_static(&[0x27, 0x8e, 0x0e, 0x45]),
                gas: Some(100_000),
                ..CallRequest::default()
            };
            let result = c
                .call(&request, Timestamp::from_secs(1_700_000_000))
                .expect("the call executes");
            let ExecutionResult::Revert { output, .. } = result else {
                panic!("expected a revert from {entity}, got {result:?}");
            };
            assert_eq!(
                evm::revert_reason(&output).as_deref(),
                Some(reason),
                "the reason has to decode as Error(string) so viem and ethers show it"
            );
        }
    }

    /// The stubs are chain state, not a special case in the executor, so a snapshot taken before
    /// one is overwritten puts it back.
    #[test]
    fn a_system_contract_stub_survives_snapshot_and_revert() {
        let mut c = chain();
        let address = long_zero_address(EXCHANGE_RATE_SYSTEM_CONTRACT);
        let stub = c.code_by_evm(&address);
        assert!(!stub.is_empty(), "genesis etches the exchange rate stub");
        let id = c.snapshot();
        c.set_code(address, Bytes::from_static(&[0x00]));
        assert!(c.revert(id), "the snapshot is there");
        assert_eq!(c.code_by_evm(&address), stub, "revert restores the stub");
    }

    #[test]
    fn genesis_has_block_zero_and_system_accounts() {
        let c = chain();
        assert_eq!(c.block_number(), 0);
        assert_eq!(c.latest_block().timestamp, 1_700_000_000);
        assert!(c.account_by_evm(&long_zero_address(TREASURY)).is_some());
        assert_eq!(c.balance_by_evm(&long_zero_address(NODE)), Tinybar(0));
        assert_eq!(c.next_id, 1008);
    }

    #[test]
    fn snapshot_and_revert_restore_blocks_and_time() {
        let mut c = chain();
        let id = c.snapshot();
        c.increase_time(3600);
        c.mine_empty(Timestamp::from_secs(1_700_000_010));
        assert_eq!(c.block_number(), 1);
        assert_eq!(c.latest_block().timestamp, 1_700_003_610);
        assert!(c.revert(id));
        assert_eq!(c.block_number(), 0);
        assert_eq!(c.time_offset, 0);
        assert!(!c.revert(id), "a reverted snapshot is consumed");
        assert_eq!(c.snapshot(), 1, "ids keep increasing after a revert");
    }

    #[test]
    fn revert_drops_later_snapshots() {
        let mut c = chain();
        let first = c.snapshot();
        c.mine_empty(Timestamp::from_secs(1_700_000_001));
        let second = c.snapshot();
        c.mine_empty(Timestamp::from_secs(1_700_000_002));
        assert!(c.revert(first));
        assert!(!c.revert(second));
    }

    #[test]
    fn next_block_timestamp_never_goes_backwards() {
        let mut c = chain();
        c.mine_empty(Timestamp::from_secs(1_699_000_000));
        assert_eq!(c.latest_block().timestamp, 1_700_000_000);
    }

    #[test]
    fn pinned_timestamp_shifts_the_offset() {
        let mut c = chain();
        c.set_next_timestamp(1_800_000_000).expect("in the future");
        c.mine_empty(Timestamp::from_secs(1_700_000_000));
        assert_eq!(c.latest_block().timestamp, 1_800_000_000);
        c.mine_empty(Timestamp::from_secs(1_700_000_005));
        assert_eq!(c.latest_block().timestamp, 1_800_000_005);
        assert!(c.set_next_timestamp(1_700_000_000).is_err());
    }

    #[test]
    fn set_balance_creates_hollow_account() {
        let mut c = chain();
        let addr = Address::repeat_byte(0xaa);
        c.set_balance(
            addr,
            Tinybar::from_hbar(5),
            Timestamp::from_secs(1_700_000_001),
        );
        let account = c.account_by_evm(&addr).expect("hollow account");
        assert_eq!(account.key, None);
        assert_eq!(account.alias, Some(addr));
        assert_eq!(account.created_at, Timestamp::from_secs(1_700_000_001));
        assert_eq!(c.balance_by_evm(&addr), Tinybar::from_hbar(5));
    }

    #[test]
    fn unsigned_transfer_moves_tinybar_and_charges_fee() {
        let mut c = chain();
        let from = c.accounts[&EntityId(1004)].evm_address();
        let to = Address::repeat_byte(0xbb);
        let hash = c
            .send_unsigned(
                UnsignedTx {
                    from,
                    to: Some(to),
                    nonce: 0,
                    gas_limit: 21_000,
                    gas_price: 71,
                    value: Tinybar::from_hbar(1).0,
                    input: Bytes::new(),
                },
                Timestamp::from_secs(1_700_000_001),
            )
            .expect("transfer");
        let tx = c.transaction(&hash).expect("recorded");
        assert!(tx.receipt.success);
        assert_eq!(tx.receipt.gas_used, 21_000);
        assert_eq!(c.balance_by_evm(&to), Tinybar::from_hbar(1));
        assert_eq!(
            c.balance_by_evm(&from),
            Tinybar(Tinybar::from_hbar(99).0 - 21_000 * 71)
        );
        assert_eq!(
            c.balance_by_evm(&long_zero_address(FEE_COLLECTOR)),
            Tinybar(21_000 * 71),
            "the fee is collected, not burned"
        );
        assert_eq!(c.nonce_by_evm(&from), 1);
        assert_eq!(c.block_number(), 1);
        let recipient = c
            .account_by_evm(&to)
            .expect("recipient became a hollow account");
        assert_eq!(
            recipient.created_at, tx.consensus_timestamp,
            "created when the transfer reached consensus, not at genesis"
        );
        assert_ne!(recipient.created_at, c.accounts[&EntityId(1004)].created_at);
    }

    #[test]
    fn consensus_timestamps_are_unique_under_a_fixed_clock() {
        // The mirror builds a transaction id from the payer and this timestamp, so two
        // transactions in the same nanosecond would otherwise share one id.
        let mut c = chain();
        let from = c.accounts[&EntityId(1004)].evm_address();
        let now = Timestamp::from_secs(1_700_000_001);
        let mut sent = Vec::new();
        for nonce in 0..3 {
            let hash = c
                .send_unsigned(
                    UnsignedTx {
                        from,
                        to: Some(Address::repeat_byte(0xcc)),
                        nonce,
                        gas_limit: 21_000,
                        gas_price: 71,
                        value: 1,
                        input: Bytes::new(),
                    },
                    now,
                )
                .expect("transfer");
            sent.push(c.transaction(&hash).expect("recorded").consensus_timestamp);
        }
        assert!(
            sent.windows(2).all(|pair| pair[1] > pair[0]),
            "consensus timestamps increase: {sent:?}"
        );
        c.mine_empty(now);
        assert!(c.latest_block().consensus_timestamp > sent[2]);
    }

    #[test]
    fn raw_legacy_transfer_from_cast_executes() {
        // The transaction under `evm::tests::LEGACY`: 0.0.1012 sends one tinybar to 0.0.1002.
        let raw = hex::decode("f86b8085a54f4c3c008252089400000000000000000000000000000000000003ea8502540be40080820278a05cdae3a91a661323014df84e1dd7237d54c08b90231f8b9bd0cf1fc3bd542afaa04b4264f936be6811e7a19868ed2eae39480fcd84297d0dc87c37aea6d3b6e96e").unwrap();
        let mut c = Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 10,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: Timestamp::from_secs(1_700_000_000),
        })
        .expect("genesis");
        let sender: Address = "0x67d8d32e9bf1a9968a5ff53b87d777aa8ebbee69"
            .parse()
            .unwrap();
        assert_eq!(c.balance_by_evm(&sender), Tinybar::from_hbar(10_000));
        let hash = c
            .send_raw(Bytes::from(raw), Timestamp::from_secs(1_700_000_001))
            .expect("transfer executes");
        let tx = c.transaction(&hash).expect("recorded");
        assert!(tx.receipt.success);
        assert_eq!(
            c.balance_by_evm(&long_zero_address(EntityId(1002))).0,
            Tinybar::from_hbar(10_000).0 + 1
        );
    }

    /// One HAPI transaction has one consensus timestamp. `ethereumTransaction` used to take one
    /// here and let the EVM allocate a second when it mined, so the mirror reported the record
    /// and the contract result under two different instants.
    #[test]
    fn an_ethereum_body_and_its_evm_record_share_one_consensus_timestamp() {
        let raw = hex::decode("f86b8085a54f4c3c008252089400000000000000000000000000000000000003ea8502540be40080820278a05cdae3a91a661323014df84e1dd7237d54c08b90231f8b9bd0cf1fc3bd542afaa04b4264f936be6811e7a19868ed2eae39480fcd84297d0dc87c37aea6d3b6e96e").unwrap();
        let mut c = Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 10,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: Timestamp::from_secs(1_700_000_000),
        })
        .expect("genesis");

        let record = c.apply_hapi(
            Transaction {
                id: TxId {
                    payer: EntityId(1012),
                    valid_start: Timestamp::from_secs(1_700_000_001),
                    nonce: 0,
                    scheduled: false,
                },
                memo: String::new(),
                max_fee: Tinybar(0),
                hash: Digest384::default(),
                valid_duration_seconds: 120,
                body: Body::Ethereum { rlp: raw },
            },
            Timestamp::from_secs(1_700_000_001),
        );

        assert_eq!(record.status, Status::Success);
        let hash = B256::from_slice(&record.ethereum_hash);
        let mined = c.transaction(&hash).expect("the EVM mined it");
        assert_eq!(record.consensus_timestamp, mined.consensus_timestamp);
    }

    /// `cryptoDelete` of an id that never existed is INVALID_ACCOUNT_ID (15); ACCOUNT_DELETED (72)
    /// is for one that did (`response_code.proto:108,398`).
    #[test]
    fn deleting_a_missing_account_and_a_deleted_one_answer_differently() {
        let mut c = chain();
        let delete = |account: EntityId, at: u64| Transaction {
            id: TxId {
                payer: EntityId(1002),
                valid_start: Timestamp::from_secs(at),
                nonce: 0,
                scheduled: false,
            },
            memo: String::new(),
            max_fee: Tinybar(0),
            hash: Digest384::default(),
            valid_duration_seconds: 120,
            body: Body::Delete {
                account,
                transfer_to: EntityId(1002),
            },
        };

        let missing = c.apply_hapi(delete(EntityId(9_999), 1), Timestamp::from_secs(1));
        assert_eq!(missing.status, Status::InvalidAccountId);

        let first = c.apply_hapi(delete(EntityId(1003), 2), Timestamp::from_secs(2));
        assert_eq!(first.status, Status::Success);
        let again = c.apply_hapi(delete(EntityId(1003), 3), Timestamp::from_secs(3));
        assert_eq!(again.status, Status::AccountDeleted);
    }

    /// Crediting the same unknown alias twice used to allocate two accounts and leave the first
    /// one's balance unreachable, because the second overwrote the alias index.
    #[test]
    fn one_alias_credited_twice_in_a_list_is_a_repeat() {
        let mut c = chain();
        let alias = Address::repeat_byte(0x42);
        let record = c.apply_hapi(
            Transaction {
                id: TxId {
                    payer: EntityId(1002),
                    valid_start: Timestamp::from_secs(1),
                    nonce: 0,
                    scheduled: false,
                },
                memo: String::new(),
                max_fee: Tinybar(0),
                hash: Digest384::default(),
                valid_duration_seconds: 120,
                body: Body::Transfer {
                    amounts: vec![
                        (AccountRef::Id(EntityId(1002)), -200),
                        (AccountRef::Alias(alias), 100),
                        (AccountRef::Alias(alias), 100),
                    ],
                },
            },
            Timestamp::from_secs(1),
        );

        assert_eq!(record.status, Status::AccountRepeatedInAccountAmounts);
        assert!(c.account_by_evm(&alias).is_none());
    }

    #[test]
    fn unsigned_from_unknown_sender_is_refused() {
        let mut c = chain();
        let err = c
            .send_unsigned(
                UnsignedTx {
                    from: Address::repeat_byte(0x01),
                    to: None,
                    nonce: 0,
                    gas_limit: 100_000,
                    gas_price: 71,
                    value: 0,
                    input: Bytes::new(),
                },
                Timestamp::from_secs(1),
            )
            .expect_err("no key, not impersonated");
        assert!(matches!(err, Error::NotImpersonated(_)));
    }
}
