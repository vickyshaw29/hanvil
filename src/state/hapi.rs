//! What HAPI transactions leave behind: transaction ids, topics, and the records the mirror and
//! the receipt queries read back. Nothing here is wire format — `hapi/` decodes protobuf into
//! these types and renders these types back out.

use std::fmt;

use alloy_primitives::Address;
use serde::{Deserialize, Serialize};

use super::accounts::{EntityId, Key};
use super::time::Timestamp;
use crate::evm::units::Tinybar;

/// A 48-byte SHA-384 digest: a topic's running hash, or a transaction's hash. Serialised as hex
/// because serde's array impls stop at 32 bytes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Digest384(pub [u8; 48]);

impl Default for Digest384 {
    fn default() -> Self {
        Self([0u8; 48])
    }
}

impl fmt::Debug for Digest384 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&hex::encode(self.0))
    }
}

impl Digest384 {
    /// The bytes, as every wire format wants them.
    pub fn as_bytes(&self) -> &[u8; 48] {
        &self.0
    }
}

impl Serialize for Digest384 {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}

impl<'de> Deserialize<'de> for Digest384 {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let text = String::deserialize(d)?;
        let bytes = hex::decode(text).map_err(serde::de::Error::custom)?;
        bytes
            .try_into()
            .map(Self)
            .map_err(|_| serde::de::Error::custom("digest must be 48 bytes"))
    }
}

/// A `ResponseCodeEnum` value (`services/response_code.proto`). Only the codes Hanvil can
/// produce are listed; the number and the name are both the protocol's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    /// 0 — precheck passed; the node accepted the transaction for consensus.
    Ok,
    /// 1 — the envelope carried nothing usable.
    InvalidTransaction,
    /// 2 — no account with the payer's id.
    PayerAccountNotFound,
    /// 3 — the body names a node this network does not run.
    InvalidNodeAccount,
    /// 4 — `validStart + validDuration` is already past.
    TransactionExpired,
    /// 5 — `validStart` is in the future.
    InvalidTransactionStart,
    /// 6 — `validDuration` outside 1..=180 seconds.
    InvalidTransactionDuration,
    /// 7 — a required signature is missing or does not verify.
    InvalidSignature,
    /// 10 — the payer cannot cover the fee.
    InsufficientPayerBalance,
    /// 11 — this transaction id already reached consensus.
    DuplicateTransaction,
    /// 13 — a body Hanvil does not emulate.
    NotSupported,
    /// 15 — an account id in the body names no account.
    InvalidAccountId,
    /// 16 — a contract id that names no contract.
    InvalidContractId,
    /// 17 — the body carries no usable `transactionID`.
    InvalidTransactionId,
    /// 18 — no receipt under that transaction id.
    ReceiptNotFound,
    /// 19 — no record under that transaction id.
    RecordNotFound,
    /// 22 — the transaction reached consensus and did what it said.
    Success,
    /// 26 — `cryptoCreateAccount` without a key.
    KeyRequired,
    /// 27 — a key or alias whose bytes are not a key of that type.
    BadEncoding,
    /// 28 — an account in a transfer list cannot cover its debit.
    InsufficientAccountBalance,
    /// 33 — the EVM reverted; the record and the fee stand.
    ContractRevertExecuted,
    /// 34 — the EVM halted (out of gas, invalid opcode).
    ContractExecutionException,
    /// 48 — the HBAR transfer list does not sum to zero.
    InvalidAccountAmounts,
    /// 50 — `bodyBytes` did not parse as a `TransactionBody`.
    InvalidTransactionBody,
    /// 72 — the account was deleted by an earlier `cryptoDelete`.
    AccountDeleted,
    /// 74 — one account appears twice in a transfer list.
    AccountRepeatedInAccountAmounts,
    /// 150 — no topic with that id.
    InvalidTopicId,
    /// 158 — an empty message.
    InvalidTopicMessage,
    /// 256 — the payer was deleted by an earlier `cryptoDelete`.
    PayerAccountDeleted,
    /// 285 — `cryptoDelete` without a live account to send the balance to.
    InvalidTransferAccountId,
    /// 332 — the alias is already held by another account.
    AliasAlreadyAssigned,
}

impl Status {
    /// The protobuf number (`response_code.proto`).
    pub fn code(self) -> i32 {
        match self {
            Self::Ok => 0,
            Self::InvalidTransaction => 1,
            Self::PayerAccountNotFound => 2,
            Self::InvalidNodeAccount => 3,
            Self::TransactionExpired => 4,
            Self::InvalidTransactionStart => 5,
            Self::InvalidTransactionDuration => 6,
            Self::InvalidSignature => 7,
            Self::InsufficientPayerBalance => 10,
            Self::DuplicateTransaction => 11,
            Self::NotSupported => 13,
            Self::InvalidAccountId => 15,
            Self::InvalidContractId => 16,
            Self::InvalidTransactionId => 17,
            Self::ReceiptNotFound => 18,
            Self::RecordNotFound => 19,
            Self::Success => 22,
            Self::KeyRequired => 26,
            Self::BadEncoding => 27,
            Self::InsufficientAccountBalance => 28,
            Self::ContractRevertExecuted => 33,
            Self::ContractExecutionException => 34,
            Self::InvalidAccountAmounts => 48,
            Self::InvalidTransactionBody => 50,
            Self::AccountDeleted => 72,
            Self::AccountRepeatedInAccountAmounts => 74,
            Self::InvalidTopicId => 150,
            Self::InvalidTopicMessage => 158,
            Self::PayerAccountDeleted => 256,
            Self::InvalidTransferAccountId => 285,
            Self::AliasAlreadyAssigned => 332,
        }
    }

    /// The protobuf name, which is what the mirror reports as a transaction's `result`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::InvalidTransaction => "INVALID_TRANSACTION",
            Self::PayerAccountNotFound => "PAYER_ACCOUNT_NOT_FOUND",
            Self::InvalidNodeAccount => "INVALID_NODE_ACCOUNT",
            Self::TransactionExpired => "TRANSACTION_EXPIRED",
            Self::InvalidTransactionStart => "INVALID_TRANSACTION_START",
            Self::InvalidTransactionDuration => "INVALID_TRANSACTION_DURATION",
            Self::InvalidSignature => "INVALID_SIGNATURE",
            Self::InsufficientPayerBalance => "INSUFFICIENT_PAYER_BALANCE",
            Self::DuplicateTransaction => "DUPLICATE_TRANSACTION",
            Self::NotSupported => "NOT_SUPPORTED",
            Self::InvalidAccountId => "INVALID_ACCOUNT_ID",
            Self::InvalidContractId => "INVALID_CONTRACT_ID",
            Self::InvalidTransactionId => "INVALID_TRANSACTION_ID",
            Self::ReceiptNotFound => "RECEIPT_NOT_FOUND",
            Self::RecordNotFound => "RECORD_NOT_FOUND",
            Self::Success => "SUCCESS",
            Self::KeyRequired => "KEY_REQUIRED",
            Self::BadEncoding => "BAD_ENCODING",
            Self::InsufficientAccountBalance => "INSUFFICIENT_ACCOUNT_BALANCE",
            Self::ContractRevertExecuted => "CONTRACT_REVERT_EXECUTED",
            Self::ContractExecutionException => "CONTRACT_EXECUTION_EXCEPTION",
            Self::InvalidAccountAmounts => "INVALID_ACCOUNT_AMOUNTS",
            Self::InvalidTransactionBody => "INVALID_TRANSACTION_BODY",
            Self::AccountDeleted => "ACCOUNT_DELETED",
            Self::AccountRepeatedInAccountAmounts => "ACCOUNT_REPEATED_IN_ACCOUNT_AMOUNTS",
            Self::InvalidTopicId => "INVALID_TOPIC_ID",
            Self::InvalidTopicMessage => "INVALID_TOPIC_MESSAGE",
            Self::PayerAccountDeleted => "PAYER_ACCOUNT_DELETED",
            Self::InvalidTransferAccountId => "INVALID_TRANSFER_ACCOUNT_ID",
            Self::AliasAlreadyAssigned => "ALIAS_ALREADY_ASSIGNED",
        }
    }
}

/// A Hedera transaction id: the payer and the instant the client says the transaction starts.
/// Two transactions with the same id are the same transaction (`DUPLICATE_TRANSACTION`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct TxId {
    /// Paying account.
    pub payer: EntityId,
    /// `transactionValidStart`.
    pub valid_start: Timestamp,
    /// Child-transaction nonce; 0 for a user transaction.
    pub nonce: i32,
    /// Set when the transaction was executed from a schedule. Hanvil never sets it.
    pub scheduled: bool,
}

/// The mirror's URL form, `0.0.x-sss-nnn` (`openapi.yml` TransactionId).
impl fmt::Display for TxId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}-{}-{:09}",
            self.payer, self.valid_start.secs, self.valid_start.nanos
        )
    }
}

/// The transaction bodies Hanvil applies. The name is the mirror's `TransactionTypes` spelling
/// (`openapi.yml:4063`), which is also the protobuf field name upper-cased.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BodyKind {
    /// `cryptoCreateAccount`.
    CryptoCreateAccount,
    /// `cryptoTransfer`.
    CryptoTransfer,
    /// `cryptoDelete`.
    CryptoDelete,
    /// `consensusCreateTopic`.
    ConsensusCreateTopic,
    /// `consensusSubmitMessage`.
    ConsensusSubmitMessage,
    /// `ethereumTransaction`, the HAPI wrapper around an EVM transaction.
    EthereumTransaction,
}

impl BodyKind {
    /// The mirror's transaction type name.
    pub fn name(self) -> &'static str {
        match self {
            Self::CryptoCreateAccount => "CRYPTOCREATEACCOUNT",
            Self::CryptoTransfer => "CRYPTOTRANSFER",
            Self::CryptoDelete => "CRYPTODELETE",
            Self::ConsensusCreateTopic => "CONSENSUSCREATETOPIC",
            Self::ConsensusSubmitMessage => "CONSENSUSSUBMITMESSAGE",
            Self::EthereumTransaction => "ETHEREUMTRANSACTION",
        }
    }
}

/// One HBAR movement inside a transaction's transfer list.
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Transfer {
    /// Account whose balance moved.
    pub account: EntityId,
    /// Signed tinybar; debits are negative.
    pub amount: i64,
}

/// An account named by id, or by an EVM alias that may not exist yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountRef {
    /// `0.0.N`.
    Id(EntityId),
    /// A 20-byte EVM address used as an alias. A credit to an unknown one creates a hollow
    /// account, which is how the relay funds a fresh EVM key.
    Alias(Address),
}

/// A consensus topic.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Topic {
    /// Entity id.
    pub id: EntityId,
    /// Topic memo.
    pub memo: String,
    /// Key that may update or delete the topic.
    pub admin_key: Option<Key>,
    /// Key a message must be signed with, when the topic has one.
    pub submit_key: Option<Key>,
    /// Messages accepted so far.
    pub sequence_number: u64,
    /// SHA-384 running hash; zero before the first message.
    pub running_hash: Digest384,
    /// Consensus timestamp of the `consensusCreateTopic`.
    pub created_at: Timestamp,
    /// Seconds; echoed back by `getTopicInfo`, never acted on — Hanvil does not expire entities.
    pub auto_renew_period: u64,
    /// Account that would pay the renewal.
    pub auto_renew_account: Option<EntityId>,
    /// Every message, oldest first.
    pub messages: Vec<TopicMessage>,
}

/// One message on a topic.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TopicMessage {
    /// 1 for the first message on the topic.
    pub sequence_number: u64,
    /// The bytes submitted.
    pub message: Vec<u8>,
    /// Running hash after this message.
    pub running_hash: Digest384,
    /// When it reached consensus.
    pub consensus_timestamp: Timestamp,
    /// Who paid.
    pub payer: EntityId,
}

/// The version of the running-hash input Hanvil produces
/// (`transaction_receipt.proto:91-110`).
pub const RUNNING_HASH_VERSION: u64 = 3;

/// SHA-384 over the v3 input list (`transaction_receipt.proto:93-110`): previous running hash,
/// version, payer shard/realm/num, topic shard/realm/num, consensus seconds and nanos, sequence
/// number, then SHA-384 of the message. Every integer is big-endian.
pub fn running_hash_v3(
    previous: &Digest384,
    payer: EntityId,
    topic: EntityId,
    consensus: Timestamp,
    sequence_number: u64,
    message: &[u8],
) -> Digest384 {
    use sha2::{Digest as _, Sha384};

    let mut input = Vec::with_capacity(48 + 8 * 9 + 4 + 48);
    input.extend_from_slice(previous.as_bytes());
    input.extend_from_slice(&RUNNING_HASH_VERSION.to_be_bytes());
    input.extend_from_slice(&0u64.to_be_bytes()); // payer shard
    input.extend_from_slice(&0u64.to_be_bytes()); // payer realm
    input.extend_from_slice(&payer.0.to_be_bytes());
    input.extend_from_slice(&0u64.to_be_bytes()); // topic shard
    input.extend_from_slice(&0u64.to_be_bytes()); // topic realm
    input.extend_from_slice(&topic.0.to_be_bytes());
    input.extend_from_slice(&consensus.secs.to_be_bytes());
    input.extend_from_slice(&consensus.nanos.to_be_bytes());
    input.extend_from_slice(&sequence_number.to_be_bytes());
    input.extend_from_slice(&Sha384::digest(message));
    Digest384(Sha384::digest(&input).into())
}

/// A transaction body after decoding, in the chain's own types. `hapi/wire.rs` builds one of
/// these; `Chain::apply_hapi` is the only thing that applies it.
#[derive(Clone, Debug)]
pub enum Body {
    /// Create an account with a key and, for an ECDSA key, an EVM alias.
    CreateAccount {
        /// The new account's key.
        key: Key,
        /// Funded from the payer.
        initial_balance: Tinybar,
        /// EVM alias, when the client set one.
        alias: Option<Address>,
        /// Account memo.
        memo: String,
    },
    /// Move HBAR between accounts. The amounts must sum to zero.
    Transfer {
        /// Signed tinybar per account, in the order the client sent them.
        amounts: Vec<(AccountRef, i64)>,
    },
    /// Delete an account and sweep its balance.
    Delete {
        /// Account to delete.
        account: EntityId,
        /// Where the remaining balance goes.
        transfer_to: EntityId,
    },
    /// Create a consensus topic.
    CreateTopic {
        /// Topic memo.
        memo: String,
        /// Key that may update or delete it.
        admin_key: Option<Key>,
        /// Key a submitted message must carry.
        submit_key: Option<Key>,
        /// Echoed by `getTopicInfo`.
        auto_renew_period: u64,
        /// Echoed by `getTopicInfo`.
        auto_renew_account: Option<EntityId>,
    },
    /// Append a message to a topic.
    SubmitMessage {
        /// Target topic.
        topic: EntityId,
        /// Message bytes.
        message: Vec<u8>,
    },
    /// An EVM transaction wrapped in a HAPI body; the RLP is what `eth_sendRawTransaction` takes.
    Ethereum {
        /// EIP-2718 bytes.
        rlp: Vec<u8>,
    },
}

impl Body {
    /// The mirror's name for this body.
    pub fn kind(&self) -> BodyKind {
        match self {
            Self::CreateAccount { .. } => BodyKind::CryptoCreateAccount,
            Self::Transfer { .. } => BodyKind::CryptoTransfer,
            Self::Delete { .. } => BodyKind::CryptoDelete,
            Self::CreateTopic { .. } => BodyKind::ConsensusCreateTopic,
            Self::SubmitMessage { .. } => BodyKind::ConsensusSubmitMessage,
            Self::Ethereum { .. } => BodyKind::EthereumTransaction,
        }
    }
}

/// A decoded HAPI transaction, ready to apply.
#[derive(Clone, Debug)]
pub struct Transaction {
    /// Payer and valid start.
    pub id: TxId,
    /// Transaction memo.
    pub memo: String,
    /// `transactionFee`: the most the payer offered. Hanvil charges a flat fee regardless.
    pub max_fee: Tinybar,
    /// SHA-384 of the submitted `Transaction` message.
    pub hash: Digest384,
    /// `transactionValidDuration`, echoed by the mirror.
    pub valid_duration_seconds: u64,
    /// What it does.
    pub body: Body,
}

/// The record a HAPI transaction leaves: what the receipt and record queries answer with, and
/// what the mirror renders as a transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    /// Transaction id.
    pub id: TxId,
    /// Body kind, for the mirror's `name`.
    pub kind: BodyKind,
    /// When it reached consensus.
    pub consensus_timestamp: Timestamp,
    /// Outcome; `SUCCESS` or the code that stopped it after the fee was charged.
    pub status: Status,
    /// Fee taken from the payer.
    pub charged_fee: Tinybar,
    /// What the payer offered as a maximum.
    pub max_fee: Tinybar,
    /// Transaction memo.
    pub memo: String,
    /// SHA-384 of the submitted `Transaction`.
    pub hash: Digest384,
    /// `transactionValidDuration`.
    pub valid_duration_seconds: u64,
    /// Every HBAR movement, fee included.
    pub transfers: Vec<Transfer>,
    /// Account created by `cryptoCreateAccount`.
    pub created_account: Option<EntityId>,
    /// Its EVM alias, when it has one; the SDK reads this back as the record's `evmAddress`.
    pub created_alias: Option<Address>,
    /// Topic created by `consensusCreateTopic`.
    pub created_topic: Option<EntityId>,
    /// Sequence number after a `consensusSubmitMessage`.
    pub topic_sequence_number: u64,
    /// Running hash after a `consensusSubmitMessage`; empty otherwise.
    pub topic_running_hash: Vec<u8>,
    /// EVM transaction hash, for an `ethereumTransaction`.
    pub ethereum_hash: Vec<u8>,
}

impl Record {
    /// The entity the transaction acted on, which is what the mirror reports as `entity_id`.
    pub fn entity(&self) -> Option<EntityId> {
        self.created_account.or(self.created_topic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_id_prints_the_mirror_url_form() {
        let id = TxId {
            payer: EntityId(1002),
            valid_start: Timestamp {
                secs: 1_700_000_000,
                nanos: 42,
            },
            nonce: 0,
            scheduled: false,
        };
        assert_eq!(id.to_string(), "0.0.1002-1700000000-000000042");
    }

    /// The v3 input is fixed by `transaction_receipt.proto:93-110`; this pins the byte layout so
    /// a reordering shows up as a changed digest rather than as a silently different chain.
    #[test]
    fn running_hash_v3_hashes_the_documented_input() {
        use sha2::{Digest as _, Sha384};

        let previous = Digest384::default();
        let consensus = Timestamp {
            secs: 1_700_000_000,
            nanos: 7,
        };
        let mut expected = Vec::new();
        expected.extend_from_slice(previous.as_bytes());
        expected.extend_from_slice(&3u64.to_be_bytes());
        expected.extend_from_slice(&[0u8; 16]);
        expected.extend_from_slice(&1002u64.to_be_bytes());
        expected.extend_from_slice(&[0u8; 16]);
        expected.extend_from_slice(&2000u64.to_be_bytes());
        expected.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        expected.extend_from_slice(&7u32.to_be_bytes());
        expected.extend_from_slice(&1u64.to_be_bytes());
        expected.extend_from_slice(&Sha384::digest(b"hello"));

        let hash = running_hash_v3(
            &previous,
            EntityId(1002),
            EntityId(2000),
            consensus,
            1,
            b"hello",
        );
        assert_eq!(hash.0, <[u8; 48]>::from(Sha384::digest(&expected)));
    }

    #[test]
    fn running_hash_chains_through_the_previous_value() {
        let first = running_hash_v3(
            &Digest384::default(),
            EntityId(1002),
            EntityId(2000),
            Timestamp::from_secs(1),
            1,
            b"one",
        );
        let second = running_hash_v3(
            &first,
            EntityId(1002),
            EntityId(2000),
            Timestamp::from_secs(2),
            2,
            b"two",
        );
        assert_ne!(first, second);
        let other = running_hash_v3(
            &Digest384::default(),
            EntityId(1002),
            EntityId(2000),
            Timestamp::from_secs(2),
            2,
            b"two",
        );
        assert_ne!(second, other);
    }
}
