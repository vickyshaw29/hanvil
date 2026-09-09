//! Blocks, transactions, receipts and logs as the chain stores them. Rendering to the JSON-RPC
//! or mirror shapes happens in the servers; nothing here is wire format.

use alloy_primitives::{Address, B256, Bloom, Bytes, U256, keccak256};
use serde::{Deserialize, Serialize};

use super::time::Timestamp;

/// One block. Hanvil mines one block per transaction (automine) or on `evm_mine`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Block {
    /// Height, genesis is 0.
    pub number: u64,
    /// keccak256 of number, parent hash, timestamp and transaction hashes. Not an Ethereum
    /// header hash; nothing on Hedera is either.
    pub hash: B256,
    /// Hash of block `number - 1`; zero for genesis.
    pub parent_hash: B256,
    /// Seconds since the epoch. Never earlier than the parent's.
    pub timestamp: u64,
    /// Consensus timestamp of the first transaction, or of the block itself when empty.
    pub consensus_timestamp: Timestamp,
    /// Block gas limit reported to clients.
    pub gas_limit: u64,
    /// Sum of `gas_used` over the transactions.
    pub gas_used: u64,
    /// Network gas price in tinybar; reported as `baseFeePerGas` after conversion.
    pub base_fee: u64,
    /// Transaction hashes in execution order.
    pub transactions: Vec<B256>,
    /// Union of the receipts' blooms.
    pub logs_bloom: Bloom,
}

impl Block {
    /// Deterministic hash over the fields that identify a block.
    pub fn compute_hash(number: u64, parent_hash: &B256, timestamp: u64, txs: &[B256]) -> B256 {
        let mut preimage = Vec::with_capacity(48 + 32 * txs.len());
        preimage.extend_from_slice(&number.to_be_bytes());
        preimage.extend_from_slice(parent_hash.as_slice());
        preimage.extend_from_slice(&timestamp.to_be_bytes());
        for tx in txs {
            preimage.extend_from_slice(tx.as_slice());
        }
        keccak256(preimage)
    }
}

/// A transaction executed without a signature: `eth_sendTransaction` from a predefined or an
/// impersonated account. Only the fields the EVM needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UnsignedTx {
    /// Sender.
    pub from: Address,
    /// Callee, or `None` to create a contract.
    pub to: Option<Address>,
    /// Sender nonce at execution.
    pub nonce: u64,
    /// Gas limit.
    pub gas_limit: u64,
    /// Gas price in tinybar.
    pub gas_price: u64,
    /// Value in tinybar.
    pub value: u64,
    /// Calldata or init code.
    pub input: Bytes,
}

/// What was submitted.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TxBody {
    /// The EIP-2718 bytes exactly as `eth_sendRawTransaction` received them.
    Signed(Bytes),
    /// An unsigned transaction accepted through `eth_sendTransaction`.
    Unsigned(UnsignedTx),
}

/// A mined transaction with its receipt.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxRecord {
    /// Transaction hash.
    pub hash: B256,
    /// The submitted form.
    pub body: TxBody,
    /// Sender, recovered from the signature or taken from the unsigned body.
    pub from: Address,
    /// Block that includes it.
    pub block_number: u64,
    /// Hash of that block.
    pub block_hash: B256,
    /// Position within the block.
    pub index: u64,
    /// Consensus timestamp assigned at execution.
    pub consensus_timestamp: Timestamp,
    /// Outcome.
    pub receipt: Receipt,
}

/// Execution outcome of one transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Receipt {
    /// `true` when the EVM returned normally.
    pub success: bool,
    /// Gas charged.
    pub gas_used: u64,
    /// Price paid per gas, in tinybar.
    pub effective_gas_price: u64,
    /// Address of a contract created by the top-level call.
    pub contract_address: Option<Address>,
    /// Logs in emission order, with block-level indexes filled in.
    pub logs: Vec<StoredLog>,
    /// Bloom over `logs`.
    pub logs_bloom: Bloom,
    /// Return data on success, revert data on revert, empty on halt.
    pub output: Bytes,
    /// Set when the EVM halted (out of gas, invalid opcode, …).
    pub halt_reason: Option<String>,
}

/// One log entry with its position.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredLog {
    /// Emitting contract.
    pub address: Address,
    /// Indexed topics, at most four.
    pub topics: Vec<B256>,
    /// Unindexed data.
    pub data: Bytes,
    /// Index within the block.
    pub log_index: u64,
    /// Transaction that emitted it.
    pub tx_hash: B256,
    /// Position of that transaction in the block.
    pub tx_index: u64,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: B256,
}

/// `eth_getLogs` filter after parsing.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LogFilter {
    /// First block, inclusive.
    pub from_block: u64,
    /// Last block, inclusive.
    pub to_block: u64,
    /// Restrict to these emitters; empty means any.
    pub addresses: Vec<Address>,
    /// Per position: `None` matches anything, `Some(list)` matches any listed topic.
    pub topics: Vec<Option<Vec<B256>>>,
}

impl LogFilter {
    /// Whether a stored log satisfies the address and topic clauses.
    pub fn matches(&self, log: &StoredLog) -> bool {
        if !self.addresses.is_empty() && !self.addresses.contains(&log.address) {
            return false;
        }
        self.topics
            .iter()
            .enumerate()
            .all(|(i, clause)| match clause {
                None => true,
                Some(any_of) => log.topics.get(i).is_some_and(|t| any_of.contains(t)),
            })
    }
}

/// Parameters of a read-only execution (`eth_call`, `eth_estimateGas`), already in tinybar.
#[derive(Clone, Debug, Default)]
pub struct CallRequest {
    /// Caller; zero when omitted, as geth does.
    pub from: Option<Address>,
    /// Callee; `None` simulates a contract creation.
    pub to: Option<Address>,
    /// Gas limit; the block limit when omitted.
    pub gas: Option<u64>,
    /// Gas price in tinybar; zero when omitted.
    pub gas_price: Option<u64>,
    /// Value in tinybar.
    pub value: u64,
    /// Calldata.
    pub input: Bytes,
}

/// Storage slot key, kept as the 256-bit value clients send.
pub type Slot = U256;

/// An installed filter, polled with `eth_getFilterChanges`. The relay serves the filter family
/// over plain HTTP (`openrpc.json:885-1005`), which is what `ethers`' `contract.on(...)` and
/// `viem`'s `createEventFilter` use when there is no WebSocket; each filter carries the block its
/// last poll stopped at, because a poll returns only what arrived since.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Filter {
    /// `eth_newFilter`: logs matching a query.
    Logs {
        /// What to match. Its `to_block` bounds the watch; `from_block` seeds the first poll.
        query: LogFilter,
        /// First block the next poll reads.
        next_block: u64,
    },
    /// `eth_newBlockFilter`: the hash of every block mined since the last poll.
    Blocks {
        /// First block the next poll reads.
        next_block: u64,
    },
}

/// What one poll of a filter found. A log filter and a block filter answer different shapes on
/// the same method, which is why `eth_getFilterChanges` cannot simply return an array of one type.
#[derive(Debug)]
pub enum FilterChanges {
    /// From a filter installed by `eth_newFilter`.
    Logs(Vec<StoredLog>),
    /// From a filter installed by `eth_newBlockFilter`.
    Blocks(Vec<B256>),
}
