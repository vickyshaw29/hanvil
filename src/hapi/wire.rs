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

    let decoded = decode_body(body.data.as_ref())?;

    if verify_signatures {
        let pairs = signature_pairs(signed.sig_map.as_ref());
        for key in required_keys(chain, id.payer, &decoded)? {
            if !sig::signed_by(&key, &signed.body_bytes, &pairs) {
                return Err(Status::InvalidSignature);
            }
        }
    }

    // Last, so a transaction that is wrong in some other way is told what is wrong with it
    // rather than what it could not have afforded.
    if balance < HAPI_FEE {
        return Err(Status::InsufficientPayerBalance);
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
                // An alias with no account has no key to require. Whether such a debit is
                // legal is `apply_transfer`'s decision, made at consensus, so that the answer
                // does not change with `--no-sig-verify`.
                let id = match account {
                    AccountRef::Id(id) => Some(*id),
                    AccountRef::Alias(address) => chain.account_by_evm(address).map(|a| a.id),
                };
                let Some(id) = id.filter(|id| *id != payer) else {
                    continue;
                };
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys;
    use crate::state::{FIRST_USER_ID, Genesis};

    const PAYER: EntityId = EntityId(FIRST_USER_ID);
    /// 0.0.1002's key, the first of hiero-local-node's ECDSA accounts.
    const PAYER_KEY: &str = "0x7f109a9e3b0d8ecfba9cc23a3614433ce0fa7ddcc80f2a8f10b222179a5a80d6";
    const NOW: Timestamp = Timestamp {
        secs: 1_700_000_000,
        nanos: 0,
    };

    fn chain() -> Chain {
        Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 2,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: NOW,
        })
        .expect("genesis")
    }

    /// A body with everything a precheck looks at set to something valid.
    fn body(data: proto::transaction_body::Data) -> proto::TransactionBody {
        proto::TransactionBody {
            transaction_id: Some(to_transaction_id(TxId {
                payer: PAYER,
                valid_start: NOW,
                nonce: 0,
                scheduled: false,
            })),
            node_account_id: Some(to_account_id(NODE)),
            transaction_fee: 100_000_000,
            transaction_valid_duration: Some(proto::Duration { seconds: 120 }),
            memo: "unit test".to_string(),
            data: Some(data),
            ..Default::default()
        }
    }

    /// Wrap a body in the two envelopes and sign it with the payer's key.
    fn signed(body: &proto::TransactionBody) -> proto::Transaction {
        use k256::ecdsa::signature::hazmat::PrehashSigner as _;

        let body_bytes = body.encode_to_vec();
        let private = keys::decode_hex(PAYER_KEY).expect("hex");
        let signing = k256::ecdsa::SigningKey::from_slice(&private).expect("scalar");
        let signature: k256::ecdsa::Signature = signing
            .sign_prehash(alloy_primitives::keccak256(&body_bytes).as_slice())
            .expect("prehash");
        let (public, _) = keys::ecdsa_public(&private).expect("public key");
        let Key::EcdsaSecp256k1(prefix) = public else {
            unreachable!()
        };
        let signed = proto::SignedTransaction {
            body_bytes,
            sig_map: Some(proto::SignatureMap {
                sig_pair: vec![proto::SignaturePair {
                    pub_key_prefix: prefix.to_vec(),
                    signature: Some(proto::signature_pair::Signature::EcdsaSecp256k1(
                        signature.to_bytes().to_vec(),
                    )),
                }],
            }),
            ..Default::default()
        };
        proto::Transaction {
            signed_transaction_bytes: signed.encode_to_vec(),
            ..Default::default()
        }
    }

    /// An unsigned envelope: the same body with an empty signature map.
    fn unsigned(body: &proto::TransactionBody) -> proto::Transaction {
        let signed = proto::SignedTransaction {
            body_bytes: body.encode_to_vec(),
            sig_map: Some(proto::SignatureMap::default()),
            ..Default::default()
        };
        proto::Transaction {
            signed_transaction_bytes: signed.encode_to_vec(),
            ..Default::default()
        }
    }

    fn transfer(to: EntityId, tinybar: i64) -> proto::transaction_body::Data {
        proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
            transfers: Some(proto::TransferList {
                account_amounts: vec![
                    proto::AccountAmount {
                        account_id: Some(to_account_id(PAYER)),
                        amount: -tinybar,
                        ..Default::default()
                    },
                    proto::AccountAmount {
                        account_id: Some(to_account_id(to)),
                        amount: tinybar,
                        ..Default::default()
                    },
                ],
            }),
            token_transfers: Vec::new(),
        })
    }

    #[test]
    fn a_signed_transfer_reaches_consensus_and_charges_the_flat_fee() {
        let mut chain = chain();
        let before = chain.account(PAYER).expect("payer").balance;
        let record = submit(
            &mut chain,
            &signed(&body(transfer(EntityId(1003), 500))),
            NOW,
            true,
        )
        .expect("precheck passes");

        assert_eq!(record.status, Status::Success);
        assert_eq!(record.charged_fee, HAPI_FEE);
        assert_eq!(record.memo, "unit test");
        assert_eq!(
            chain.account(PAYER).expect("payer").balance.0,
            before.0 - HAPI_FEE.0 - 500
        );
        assert_eq!(
            chain.account(EntityId(1003)).expect("recipient").balance.0,
            {
                let genesis = Tinybar::from_hbar(10_000).0;
                genesis + 500
            }
        );
        // One entry per account: the fee leg and the transfer leg are the same payer.
        assert_eq!(record.transfers.len(), 3);
    }

    /// One body that fails a single precheck: what it does wrong, how, and the code it earns.
    type Case = (&'static str, fn(&mut proto::TransactionBody), Status);

    /// Each precheck in the order `docs/code-plan.md` §5 fixes, one body that fails only it.
    #[test]
    fn prechecks_map_to_their_response_codes() {
        let cases: [Case; 6] = [
            (
                "a node this network does not run",
                |body| body.node_account_id = Some(to_account_id(EntityId(4))),
                Status::InvalidNodeAccount,
            ),
            (
                "no transaction id",
                |body| body.transaction_id = None,
                Status::InvalidTransactionId,
            ),
            (
                "a duration past the 180 s cap",
                |body| body.transaction_valid_duration = Some(proto::Duration { seconds: 181 }),
                Status::InvalidTransactionDuration,
            ),
            (
                "a valid start an hour ahead",
                |body| {
                    body.transaction_id = Some(to_transaction_id(TxId {
                        payer: PAYER,
                        valid_start: Timestamp {
                            secs: NOW.secs + 3_600,
                            nanos: 0,
                        },
                        nonce: 0,
                        scheduled: false,
                    }));
                },
                Status::InvalidTransactionStart,
            ),
            (
                "a valid start that expired",
                |body| {
                    body.transaction_id = Some(to_transaction_id(TxId {
                        payer: PAYER,
                        valid_start: Timestamp {
                            secs: NOW.secs - 3_600,
                            nanos: 0,
                        },
                        nonce: 0,
                        scheduled: false,
                    }));
                },
                Status::TransactionExpired,
            ),
            (
                "a payer with no account",
                |body| {
                    body.transaction_id = Some(to_transaction_id(TxId {
                        payer: EntityId(9_999),
                        valid_start: NOW,
                        nonce: 0,
                        scheduled: false,
                    }));
                },
                Status::PayerAccountNotFound,
            ),
        ];

        for (what, break_it, expected) in cases {
            let mut chain = chain();
            let mut body = body(transfer(EntityId(1003), 500));
            break_it(&mut body);
            let outcome = submit(&mut chain, &signed(&body), NOW, true);
            assert_eq!(outcome.err(), Some(expected), "{what}");
        }
    }

    #[test]
    fn the_same_transaction_id_twice_is_a_duplicate() {
        let mut chain = chain();
        let envelope = signed(&body(transfer(EntityId(1003), 500)));
        submit(&mut chain, &envelope, NOW, true).expect("first submission");
        assert_eq!(
            submit(&mut chain, &envelope, NOW, true).err(),
            Some(Status::DuplicateTransaction)
        );
    }

    #[test]
    fn an_unsigned_body_is_refused_unless_signature_checking_is_off() {
        let body = body(transfer(EntityId(1003), 500));
        assert_eq!(
            submit(&mut chain(), &unsigned(&body), NOW, true).err(),
            Some(Status::InvalidSignature)
        );
        assert_eq!(
            submit(&mut chain(), &unsigned(&body), NOW, false)
                .expect("--no-sig-verify accepts it")
                .status,
            Status::Success
        );
    }

    #[test]
    fn a_body_hanvil_does_not_emulate_is_not_supported() {
        let data = proto::transaction_body::Data::TokenCreation(
            proto::TokenCreateTransactionBody::default(),
        );
        assert_eq!(
            submit(&mut chain(), &signed(&body(data)), NOW, true).err(),
            Some(Status::NotSupported)
        );
    }

    /// A body that fails at consensus still produces a record and still costs the fee.
    #[test]
    fn a_transfer_list_that_does_not_balance_is_recorded_as_a_failure() {
        let mut chain = chain();
        let before = chain.account(PAYER).expect("payer").balance;
        let data =
            proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
                transfers: Some(proto::TransferList {
                    account_amounts: vec![proto::AccountAmount {
                        account_id: Some(to_account_id(PAYER)),
                        amount: -500,
                        ..Default::default()
                    }],
                }),
                token_transfers: Vec::new(),
            });
        let record = submit(&mut chain, &signed(&body(data)), NOW, true).expect("precheck passes");

        assert_eq!(record.status, Status::InvalidAccountAmounts);
        assert_eq!(
            chain.account(PAYER).expect("payer").balance.0,
            before.0 - HAPI_FEE.0,
            "the fee is charged and the body is not applied"
        );
    }

    #[test]
    fn a_message_on_a_topic_that_does_not_exist_names_the_topic() {
        let data = proto::transaction_body::Data::ConsensusSubmitMessage(
            proto::ConsensusSubmitMessageTransactionBody {
                topic_id: Some(to_topic_id(EntityId(9_999))),
                message: b"hello".to_vec(),
                chunk_info: None,
            },
        );
        assert_eq!(
            submit(&mut chain(), &signed(&body(data)), NOW, true).err(),
            Some(Status::InvalidTopicId)
        );
    }

    /// The balance check runs last, so a transaction that is wrong in some other way is told so.
    /// The first version charged the payer's balance before the body and the signatures, which
    /// answered INSUFFICIENT_PAYER_BALANCE to a body that was never going to be applied.
    #[test]
    fn a_broke_payer_is_still_told_what_else_is_wrong() {
        let mut chain = chain();
        // Leave the payer with less than the flat fee.
        chain.set_balance(crate::evm::units::long_zero_address(PAYER), Tinybar(1), NOW);

        let unsupported = proto::transaction_body::Data::TokenCreation(
            proto::TokenCreateTransactionBody::default(),
        );
        assert_eq!(
            submit(&mut chain, &signed(&body(unsupported)), NOW, true).err(),
            Some(Status::NotSupported)
        );

        let transfer = body(transfer(EntityId(1003), 500));
        assert_eq!(
            submit(&mut chain, &unsigned(&transfer), NOW, true).err(),
            Some(Status::InvalidSignature)
        );

        // With nothing else wrong, the balance is what stops it.
        assert_eq!(
            submit(&mut chain, &signed(&transfer), NOW, true).err(),
            Some(Status::InsufficientPayerBalance)
        );
    }

    /// A debit from an alias with no account is refused at consensus, not in the signature check,
    /// so the payer is charged the same either way. Before, `--no-sig-verify` decided whether the
    /// fee was taken.
    #[test]
    fn an_unknown_alias_debit_costs_the_fee_whether_or_not_signatures_are_checked() {
        let unknown = Address::repeat_byte(0x42);
        let data =
            proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
                transfers: Some(proto::TransferList {
                    account_amounts: vec![
                        proto::AccountAmount {
                            account_id: Some(proto::AccountId {
                                shard_num: 0,
                                realm_num: 0,
                                account: Some(proto::account_id::Account::Alias(unknown.to_vec())),
                            }),
                            amount: -500,
                            ..Default::default()
                        },
                        proto::AccountAmount {
                            account_id: Some(to_account_id(PAYER)),
                            amount: 500,
                            ..Default::default()
                        },
                    ],
                }),
                token_transfers: Vec::new(),
            });

        for verify in [true, false] {
            let mut chain = chain();
            let before = chain.account(PAYER).expect("payer").balance;
            let record = submit(&mut chain, &signed(&body(data.clone())), NOW, verify)
                .expect("precheck passes");
            assert_eq!(record.status, Status::InvalidAccountId, "verify={verify}");
            assert_eq!(
                chain.account(PAYER).expect("payer").balance.0,
                before.0 - HAPI_FEE.0,
                "verify={verify}"
            );
        }
    }
}
