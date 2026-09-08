//! `Transaction` → `SignedTransaction` → `TransactionBody`, the prechecks a node runs before it
//! answers `TransactionResponse`, and the translation into the chain's own body types.
//!
//! Order and codes are fixed by `docs/code-plan.md` §5: node account, transaction id, duration,
//! valid start, duplicate, payer, supported body, signatures, payer balance. A failure here means
//! the transaction never reached consensus and left no record, which is what
//! `nodeTransactionPrecheckCode` reports.

use alloy_primitives::Address;
use prost::Message as _;
use sha2::{Digest as _, Sha384};

use super::proto;
use crate::evm::units::Tinybar;
use crate::keys::sig::{self, SignaturePair};
use crate::state::{
    AccountRef, Body, Chain, Digest384, EntityId, HAPI_FEE, Key, NODE, Record, Status, Timestamp,
    Transaction, TxId,
};

/// How far ahead of the node's clock a `validStart` may sit. The SDK stamps it from the client's
/// clock, so a small skew is not a rejection; a minute out is.
const FUTURE_TOLERANCE_SECS: i64 = 60;

/// Bounds on `transactionValidDuration` (`transaction.proto`: the network caps it at 180 s).
const MAX_VALID_DURATION_SECS: u64 = 180;

/// Decode, precheck, verify signatures and apply. `Err` is the precheck code the node returns
/// instead of `OK`; `Ok` means the transaction reached consensus and the record is stored.
pub fn submit(
    chain: &mut Chain,
    envelope: &proto::Transaction,
    now: Timestamp,
    verify_signatures: bool,
) -> Result<Record, Status> {
    if envelope.signed_transaction_bytes.is_empty() {
        // Every SDK since v2 sends this field; the deprecated `bodyBytes`/`sigMap` pair on
        // `Transaction` is not read.
        return Err(Status::InvalidTransaction);
    }
    let signed = proto::SignedTransaction::decode(envelope.signed_transaction_bytes.as_slice())
        .map_err(|_| Status::InvalidTransaction)?;
    let body = proto::TransactionBody::decode(signed.body_bytes.as_slice())
        .map_err(|_| Status::InvalidTransactionBody)?;

    if account_id(body.node_account_id.as_ref()) != Some(NODE) {
        return Err(Status::InvalidNodeAccount);
    }

    let id = transaction_id(body.transaction_id.as_ref()).ok_or(Status::InvalidTransactionId)?;

    let duration = body
        .transaction_valid_duration
        .as_ref()
        .map_or(120, |d| d.seconds.max(0) as u64);
    if duration == 0 || duration > MAX_VALID_DURATION_SECS {
        return Err(Status::InvalidTransactionDuration);
    }
    if id.valid_start.secs as i64 - now.secs as i64 > FUTURE_TOLERANCE_SECS {
        return Err(Status::InvalidTransactionStart);
    }
    if id.valid_start.secs + duration < now.secs {
        return Err(Status::TransactionExpired);
    }
    if chain.has_transaction_id(&id) {
        return Err(Status::DuplicateTransaction);
    }

    let (deleted, balance) = chain
        .account(id.payer)
        .map(|payer| (payer.deleted, payer.balance))
        .ok_or(Status::PayerAccountNotFound)?;
    if deleted {
        return Err(Status::PayerAccountDeleted);
    }
    if balance < HAPI_FEE {
        return Err(Status::InsufficientPayerBalance);
    }

    let decoded = decode_body(body.data.as_ref())?;

    if verify_signatures {
        let pairs = signature_pairs(signed.sig_map.as_ref());
        for key in required_keys(chain, id.payer, &decoded)? {
            if !sig::signed_by(&key, &signed.body_bytes, &pairs) {
                return Err(Status::InvalidSignature);
            }
        }
    }

    let transaction = Transaction {
        id,
        memo: body.memo,
        max_fee: Tinybar(body.transaction_fee),
        hash: Digest384(Sha384::digest(envelope.encode_to_vec()).into()),
        valid_duration_seconds: duration,
        body: decoded,
    };
    Ok(chain.apply_hapi(transaction, now))
}

/// Every key that must have signed, in the order they are checked. The payer always signs; a
/// debit signs for itself; a delete signs with the account being deleted; a topic with a submit
/// key signs with it. Hanvil does not require the alias key's signature on `cryptoCreateAccount`
/// — that is in the README's list of what is not emulated.
fn required_keys(chain: &Chain, payer: EntityId, body: &Body) -> Result<Vec<Key>, Status> {
    let mut keys = Vec::new();
    if let Some(key) = chain.account(payer).and_then(|a| a.key.clone()) {
        keys.push(key);
    }
    match body {
        Body::Transfer { amounts } => {
            for (account, _) in amounts.iter().filter(|(_, amount)| *amount < 0) {
                let id = match account {
                    AccountRef::Id(id) => *id,
                    AccountRef::Alias(address) => chain
                        .account_by_evm(address)
                        .map(|a| a.id)
                        .ok_or(Status::InvalidAccountId)?,
                };
                if id == payer {
                    continue;
                }
                if let Some(key) = chain.account(id).and_then(|a| a.key.clone()) {
                    keys.push(key);
                }
            }
        }
        Body::Delete { account, .. } if *account != payer => {
            if let Some(key) = chain.account(*account).and_then(|a| a.key.clone()) {
                keys.push(key);
            }
        }
        Body::SubmitMessage { topic, .. } => {
            match chain.topic(*topic) {
                Some(target) => {
                    if let Some(key) = target.submit_key.clone() {
                        keys.push(key);
                    }
                }
                // The topic id is checked again at consensus; here it only decides whether a
                // submit key is required.
                None => return Err(Status::InvalidTopicId),
            }
        }
        _ => {}
    }
    Ok(keys)
}

/// The `SignatureMap`, with the protobuf types left behind.
fn signature_pairs(map: Option<&proto::SignatureMap>) -> Vec<SignaturePair<'_>> {
    use proto::signature_pair::Signature as Wire;

    map.map(|map| {
        map.sig_pair
            .iter()
            .filter_map(|pair| {
                // The RSA and ECDSA-384 variants are deprecated in the protobuf and match no key
                // Hanvil can hold; they are matched only to keep the arm exhaustive.
                #[allow(deprecated)]
                let signature = match pair.signature.as_ref()? {
                    Wire::Ed25519(bytes) => sig::Signature::Ed25519(bytes),
                    Wire::EcdsaSecp256k1(bytes) => sig::Signature::EcdsaSecp256k1(bytes),
                    Wire::Contract(_) | Wire::Rsa3072(_) | Wire::Ecdsa384(_) => return None,
                };
                Some(SignaturePair {
                    prefix: &pair.pub_key_prefix,
                    signature,
                })
            })
            .collect()
    })
    .unwrap_or_default()
}

/// Translate the body oneof into the chain's own type, or say why the body is refused.
fn decode_body(data: Option<&proto::transaction_body::Data>) -> Result<Body, Status> {
    use proto::transaction_body::Data;

    match data {
        Some(Data::CryptoCreateAccount(create)) => {
            let key = key_from(create.key.as_ref()).ok_or(Status::KeyRequired)?;
            let alias = match create.alias.len() {
                0 => None,
                20 => Some(Address::from_slice(&create.alias)),
                _ => return Err(Status::BadEncoding),
            };
            Ok(Body::CreateAccount {
                key,
                initial_balance: Tinybar(create.initial_balance),
                alias,
                memo: create.memo.clone(),
            })
        }
        Some(Data::CryptoTransfer(transfer)) => {
            if !transfer.token_transfers.is_empty() {
                return Err(Status::NotSupported);
            }
            let amounts = transfer
                .transfers
                .iter()
                .flat_map(|list| list.account_amounts.iter())
                .map(|entry| {
                    account_ref(entry.account_id.as_ref())
                        .map(|account| (account, entry.amount))
                        .ok_or(Status::InvalidAccountId)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(Body::Transfer { amounts })
        }
        Some(Data::CryptoDelete(delete)) => Ok(Body::Delete {
            account: account_id(delete.delete_account_id.as_ref())
                .ok_or(Status::InvalidAccountId)?,
            transfer_to: account_id(delete.transfer_account_id.as_ref())
                .ok_or(Status::InvalidTransferAccountId)?,
        }),
        Some(Data::ConsensusCreateTopic(create)) => Ok(Body::CreateTopic {
            memo: create.memo.clone(),
            admin_key: key_from(create.admin_key.as_ref()),
            submit_key: key_from(create.submit_key.as_ref()),
            auto_renew_period: create
                .auto_renew_period
                .as_ref()
                .map_or(0, |d| d.seconds.max(0) as u64),
            auto_renew_account: account_id(create.auto_renew_account.as_ref()),
        }),
        Some(Data::ConsensusSubmitMessage(submit)) => {
            // `chunkInfo` is set even for a single-chunk message by the SDK's
            // `TopicMessageSubmitTransaction`, and the network stores each chunk as its own topic
            // message with its own sequence number. Hanvil does the same; the chunk metadata
            // itself is not reported back (README, what is not emulated).
            Ok(Body::SubmitMessage {
                topic: topic_id(submit.topic_id.as_ref()).ok_or(Status::InvalidTopicId)?,
                message: submit.message.clone(),
            })
        }
        Some(Data::EthereumTransaction(ethereum)) => {
            if ethereum.call_data.is_some() {
                // Call data held in a file needs FileService, which Hanvil does not serve.
                return Err(Status::NotSupported);
            }
            Ok(Body::Ethereum {
                rlp: ethereum.ethereum_data.clone(),
            })
        }
        Some(_) => Err(Status::NotSupported),
        None => Err(Status::InvalidTransactionBody),
    }
}

/// `AccountID` → `EntityId`. An alias-only id resolves through the chain's alias index elsewhere;
/// this form is for the fields that must name an existing account by number.
pub fn account_id(id: Option<&proto::AccountId>) -> Option<EntityId> {
    match id?.account.as_ref()? {
        proto::account_id::Account::AccountNum(num) => Some(EntityId(*num as u64)),
        proto::account_id::Account::Alias(_) => None,
    }
}

/// `AccountID` as a transfer list writes it: a number, or a 20-byte EVM alias.
fn account_ref(id: Option<&proto::AccountId>) -> Option<AccountRef> {
    match id?.account.as_ref()? {
        proto::account_id::Account::AccountNum(num) => Some(AccountRef::Id(EntityId(*num as u64))),
        proto::account_id::Account::Alias(bytes) if bytes.len() == 20 => {
            Some(AccountRef::Alias(Address::from_slice(bytes)))
        }
        proto::account_id::Account::Alias(_) => None,
    }
}

/// `TopicID` → `EntityId`.
pub fn topic_id(id: Option<&proto::TopicId>) -> Option<EntityId> {
    id.map(|id| EntityId(id.topic_num as u64))
}

/// `TransactionID` → `TxId`.
pub fn transaction_id(id: Option<&proto::TransactionId>) -> Option<TxId> {
    let id = id?;
    Some(TxId {
        payer: account_id(id.account_id.as_ref())?,
        valid_start: timestamp(id.transaction_valid_start.as_ref())?,
        nonce: id.nonce,
        scheduled: id.scheduled,
    })
}

/// `Timestamp` → the chain's own.
pub fn timestamp(stamp: Option<&proto::Timestamp>) -> Option<Timestamp> {
    let stamp = stamp?;
    Some(Timestamp {
        secs: u64::try_from(stamp.seconds).ok()?,
        nanos: u32::try_from(stamp.nanos).ok()?,
    })
}

/// A single ED25519 or ECDSA key. Threshold keys and key lists are refused by returning `None`,
/// which the callers turn into `KEY_REQUIRED`.
pub fn key_from(key: Option<&proto::Key>) -> Option<Key> {
    match key?.key.as_ref()? {
        proto::key::Key::Ed25519(bytes) => bytes.as_slice().try_into().ok().map(Key::Ed25519),
        proto::key::Key::EcdsaSecp256k1(bytes) => {
            bytes.as_slice().try_into().ok().map(Key::EcdsaSecp256k1)
        }
        _ => None,
    }
}

/// The chain's timestamp as the protocol writes it.
pub fn to_proto_timestamp(stamp: Timestamp) -> proto::Timestamp {
    proto::Timestamp {
        seconds: stamp.secs as i64,
        nanos: stamp.nanos as i32,
    }
}

/// `EntityId` as an `AccountID` in shard 0, realm 0.
pub fn to_account_id(id: EntityId) -> proto::AccountId {
    proto::AccountId {
        shard_num: 0,
        realm_num: 0,
        account: Some(proto::account_id::Account::AccountNum(id.0 as i64)),
    }
}

/// `EntityId` as a `TopicID`.
pub fn to_topic_id(id: EntityId) -> proto::TopicId {
    proto::TopicId {
        shard_num: 0,
        realm_num: 0,
        topic_num: id.0 as i64,
    }
}

/// `EntityId` as a `ContractID`.
pub fn to_contract_id(id: EntityId) -> proto::ContractId {
    proto::ContractId {
        shard_num: 0,
        realm_num: 0,
        contract: Some(proto::contract_id::Contract::ContractNum(id.0 as i64)),
    }
}

/// `TxId` as a `TransactionID`.
pub fn to_transaction_id(id: TxId) -> proto::TransactionId {
    proto::TransactionId {
        transaction_valid_start: Some(to_proto_timestamp(id.valid_start)),
        account_id: Some(to_account_id(id.payer)),
        scheduled: id.scheduled,
        nonce: id.nonce,
    }
}
