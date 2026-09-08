//! Chain types rendered back into HAPI responses.

use super::proto;
use super::wire::{to_account_id, to_contract_id, to_proto_timestamp, to_topic_id};
use crate::state::{
    AUTO_RENEW_PERIOD_SECS, Account, CENT_EQUIVALENT, Chain, EXCHANGE_RATE_VALID_SECS, EntityId,
    HBAR_EQUIVALENT, Record, Status, Timestamp, Topic, hapi,
};

/// The same fixed rate `/api/v1/network/exchangerate` reports.
fn exchange_rate(chain: &Chain) -> proto::ExchangeRateSet {
    let expires = chain.latest_block().consensus_timestamp.secs + EXCHANGE_RATE_VALID_SECS;
    let rate = proto::ExchangeRate {
        hbar_equiv: HBAR_EQUIVALENT as i32,
        cent_equiv: CENT_EQUIVALENT as i32,
        expiration_time: Some(proto::TimestampSeconds {
            seconds: expires as i64,
        }),
    };
    proto::ExchangeRateSet {
        current_rate: Some(rate),
        next_rate: Some(rate),
    }
}

/// `TransactionReceipt` (`transaction_receipt.proto`). Only the fields the body actually filled
/// in are set; the rest stay at their protobuf defaults, as a node leaves them.
pub fn receipt(chain: &Chain, record: &Record) -> proto::TransactionReceipt {
    proto::TransactionReceipt {
        status: record.status.code(),
        account_id: record.created_account.map(to_account_id),
        contract_id: created_contract(chain, record),
        exchange_rate: Some(exchange_rate(chain)),
        topic_id: record.created_topic.map(to_topic_id),
        topic_sequence_number: record.topic_sequence_number,
        topic_running_hash: record.topic_running_hash.clone(),
        topic_running_hash_version: if record.topic_running_hash.is_empty() {
            0
        } else {
            hapi::RUNNING_HASH_VERSION
        },
        ..Default::default()
    }
}

/// `TransactionRecord` (`transaction_record.proto`).
pub fn transaction_record(chain: &Chain, record: &Record) -> proto::TransactionRecord {
    proto::TransactionRecord {
        receipt: Some(receipt(chain, record)),
        transaction_hash: record.hash.as_bytes().to_vec(),
        consensus_timestamp: Some(to_proto_timestamp(record.consensus_timestamp)),
        transaction_id: Some(super::wire::to_transaction_id(record.id)),
        memo: record.memo.clone(),
        transaction_fee: record.charged_fee.0,
        transfer_list: Some(proto::TransferList {
            account_amounts: record
                .transfers
                .iter()
                .map(|transfer| proto::AccountAmount {
                    account_id: Some(to_account_id(transfer.account)),
                    amount: transfer.amount,
                    is_approval: false,
                    hook_call: None,
                })
                .collect(),
        }),
        ethereum_hash: record.ethereum_hash.clone(),
        evm_address: record
            .created_alias
            .map(|alias| alias.to_vec())
            .unwrap_or_default(),
        ..Default::default()
    }
}

/// The contract an `ethereumTransaction` created, when it created one.
fn created_contract(chain: &Chain, record: &Record) -> Option<proto::ContractId> {
    let hash = alloy_primitives::B256::try_from(record.ethereum_hash.as_slice()).ok()?;
    let created = chain.transaction(&hash)?.receipt.contract_address?;
    chain.contract_id_by_evm(&created).map(to_contract_id)
}

/// `CryptoGetInfoResponse.AccountInfo` (`crypto_get_info.proto`).
pub fn account_info(account: &Account) -> proto::crypto_get_info_response::AccountInfo {
    use crate::evm::units::long_zero_address;

    let evm_address = account
        .alias
        .unwrap_or_else(|| long_zero_address(account.id));
    proto::crypto_get_info_response::AccountInfo {
        account_id: Some(to_account_id(account.id)),
        // The SDK reads this as `contractAccountId`, a hex EVM address without `0x`.
        contract_account_id: hex::encode(evm_address),
        deleted: account.deleted,
        key: account.key.as_ref().map(to_proto_key),
        balance: account.balance.0,
        memo: account.memo.clone(),
        // `alias` carries a serialized protobuf `Key`, and the SDK decodes it as one
        // (`hiero-sdk-js/src/account/AccountInfo.js:224-231`). Hanvil mints EVM-address aliases,
        // not key aliases, so the field stays empty and the address is in `contractAccountId`.
        // The mirror reports `alias: null` for the same reason (docs/research.md §16).
        ethereum_nonce: account.nonce as i64,
        expiration_time: Some(to_proto_timestamp(Timestamp {
            secs: account.created_at.secs + AUTO_RENEW_PERIOD_SECS,
            nanos: account.created_at.nanos,
        })),
        ..Default::default()
    }
}

/// `ConsensusTopicInfo` (`consensus_topic_info.proto`).
pub fn topic_info(topic: &Topic) -> proto::ConsensusTopicInfo {
    proto::ConsensusTopicInfo {
        memo: topic.memo.clone(),
        running_hash: topic.running_hash.as_bytes().to_vec(),
        sequence_number: topic.sequence_number,
        expiration_time: Some(to_proto_timestamp(Timestamp {
            secs: topic.created_at.secs
                + if topic.auto_renew_period > 0 {
                    topic.auto_renew_period
                } else {
                    AUTO_RENEW_PERIOD_SECS
                },
            nanos: topic.created_at.nanos,
        })),
        admin_key: topic.admin_key.as_ref().map(to_proto_key),
        submit_key: topic.submit_key.as_ref().map(to_proto_key),
        auto_renew_period: Some(proto::Duration {
            seconds: topic.auto_renew_period as i64,
        }),
        auto_renew_account: topic.auto_renew_account.map(to_account_id),
        ..Default::default()
    }
}

/// A stored key as the protocol writes it.
pub fn to_proto_key(key: &crate::state::Key) -> proto::Key {
    use crate::state::Key;
    proto::Key {
        key: Some(match key {
            Key::Ed25519(bytes) => proto::key::Key::Ed25519(bytes.to_vec()),
            Key::EcdsaSecp256k1(bytes) => proto::key::Key::EcdsaSecp256k1(bytes.to_vec()),
        }),
    }
}

/// The status a receipt query reports for a transaction id nothing is stored under.
pub const NOT_FOUND: Status = Status::ReceiptNotFound;

/// `EntityId` from a `ContractID` that names either a number or a 20-byte EVM address.
pub fn contract_entity(chain: &Chain, id: Option<&proto::ContractId>) -> Option<EntityId> {
    match id?.contract.as_ref()? {
        proto::contract_id::Contract::ContractNum(num) => Some(EntityId(*num as u64)),
        proto::contract_id::Contract::EvmAddress(bytes) if bytes.len() == 20 => {
            let address = alloy_primitives::Address::from_slice(bytes);
            chain
                .contract_id_by_evm(&address)
                .or_else(|| chain.entity_by_evm(&address))
        }
        proto::contract_id::Contract::EvmAddress(_) => None,
    }
}
