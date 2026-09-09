//! HAPI gRPC end to end with a tonic client: the sequence the harness's chain tier drives, which
//! is `createAccount` → `cryptoTransfer` → `cryptoGetBalance` → `cryptoDelete` → receipt.
//!
//! `tests/js/sdk.test.mjs` covers the same ground through `@hiero-ledger/sdk`. This one holds the
//! wire format still from the Rust side, so a change to the encoding fails here rather than in a
//! Node process. The bodies are built and signed by hand because the crate is a binary and the
//! generated types are reachable only through `OUT_DIR`.

mod common;

use k256::ecdsa::signature::hazmat::PrehashSigner as _;
use prost::Message as _;

#[allow(
    clippy::all,
    dead_code,
    unused_imports,
    unused_qualifications,
    rustdoc::all
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/hapi.rs"));
}
use generated::proto;

use proto::crypto_service_client::CryptoServiceClient;

/// 0.0.1002, the first predefined ECDSA account, and the key the banner prints for it.
const PAYER: i64 = 1002;
const PAYER_KEY: &str = "7f109a9e3b0d8ecfba9cc23a3614433ce0fa7ddcc80f2a8f10b222179a5a80d6";
/// The single consensus node every body must name.
const NODE: i64 = 3;
/// 0.0.1003, an existing account to transfer to.
const RECIPIENT: i64 = 1003;

fn account_id(num: i64) -> proto::AccountId {
    proto::AccountId {
        shard_num: 0,
        realm_num: 0,
        account: Some(proto::account_id::Account::AccountNum(num)),
    }
}

fn now() -> proto::Timestamp {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch");
    proto::Timestamp {
        seconds: since_epoch.as_secs() as i64,
        nanos: since_epoch.subsec_nanos() as i32,
    }
}

/// A body naming the payer, this node, and a valid start of `start`.
fn body(start: proto::Timestamp, data: proto::transaction_body::Data) -> proto::TransactionBody {
    proto::TransactionBody {
        transaction_id: Some(proto::TransactionId {
            transaction_valid_start: Some(start),
            account_id: Some(account_id(PAYER)),
            scheduled: false,
            nonce: 0,
        }),
        node_account_id: Some(account_id(NODE)),
        transaction_fee: 100_000_000,
        transaction_valid_duration: Some(proto::Duration { seconds: 120 }),
        memo: "grpc integration test".to_string(),
        data: Some(data),
        ..Default::default()
    }
}

/// ECDSA secp256k1 over `keccak256(bodyBytes)`, 64 bytes of r‖s, as the SDK signs.
fn sign_with(body: &proto::TransactionBody, keys: &[&str]) -> proto::Transaction {
    let body_bytes = body.encode_to_vec();
    let digest = alloy_primitives::keccak256(&body_bytes);
    let sig_pair = keys
        .iter()
        .map(|hex_key| {
            let private = hex::decode(hex_key).expect("hex");
            let signing = k256::ecdsa::SigningKey::from_slice(&private).expect("scalar");
            let signature: k256::ecdsa::Signature =
                signing.sign_prehash(digest.as_slice()).expect("prehash");
            let public = signing.verifying_key().to_encoded_point(true);
            proto::SignaturePair {
                pub_key_prefix: public.as_bytes().to_vec(),
                signature: Some(proto::signature_pair::Signature::EcdsaSecp256k1(
                    signature.to_bytes().to_vec(),
                )),
            }
        })
        .collect();
    let signed = proto::SignedTransaction {
        body_bytes,
        sig_map: Some(proto::SignatureMap { sig_pair }),
        ..Default::default()
    };
    proto::Transaction {
        signed_transaction_bytes: signed.encode_to_vec(),
        ..Default::default()
    }
}

/// `ANSWER_ONLY`, which is what the SDK sends for a free query.
fn answer_only() -> proto::QueryHeader {
    proto::QueryHeader {
        payment: None,
        response_type: proto::ResponseType::AnswerOnly as i32,
    }
}

/// Poll the receipt for a transaction id and return its status code.
async fn receipt_status(
    client: &mut CryptoServiceClient<tonic::transport::Channel>,
    id: &proto::TransactionId,
) -> i32 {
    let query = proto::Query {
        query: Some(proto::query::Query::TransactionGetReceipt(
            proto::TransactionGetReceiptQuery {
                header: Some(answer_only()),
                transaction_id: Some(id.clone()),
                include_duplicates: false,
                include_child_receipts: false,
            },
        )),
    };
    let response = client
        .get_transaction_receipts(query)
        .await
        .expect("receipt");
    let Some(proto::response::Response::TransactionGetReceipt(receipt)) =
        response.into_inner().response
    else {
        panic!("expected a transactionGetReceipt response");
    };
    receipt.receipt.expect("a receipt is present").status
}

async fn balance_of(client: &mut CryptoServiceClient<tonic::transport::Channel>, num: i64) -> u64 {
    let query = proto::Query {
        query: Some(proto::query::Query::CryptogetAccountBalance(
            proto::CryptoGetAccountBalanceQuery {
                header: Some(answer_only()),
                balance_source: Some(
                    proto::crypto_get_account_balance_query::BalanceSource::AccountId(account_id(
                        num,
                    )),
                ),
            },
        )),
    };
    let response = client.crypto_get_balance(query).await.expect("balance");
    let Some(proto::response::Response::CryptogetAccountBalance(balance)) =
        response.into_inner().response
    else {
        panic!("expected a cryptogetAccountBalance response");
    };
    balance.balance
}

/// `ResponseCodeEnum`: OK is the precheck code, SUCCESS the consensus one.
const OK: i32 = 0;
const SUCCESS: i32 = 22;

#[tokio::test]
async fn create_transfer_balance_delete_and_read_every_receipt() {
    let node = common::Node::boot();
    let mut client = CryptoServiceClient::connect(format!("http://127.0.0.1:{}", node.grpc_port))
        .await
        .expect("the gRPC listener accepts h2c");

    // A fresh ECDSA key, created with its EVM alias the way the harness provisions a signer.
    let new_key = k256::ecdsa::SigningKey::from_slice(&[9u8; 32]).expect("scalar");
    let new_key_hex = hex::encode(new_key.to_bytes());
    let public = new_key.verifying_key().to_encoded_point(true);
    let alias = {
        let uncompressed = new_key.verifying_key().to_encoded_point(false);
        alloy_primitives::keccak256(&uncompressed.as_bytes()[1..])[12..].to_vec()
    };

    // ---- create ----------------------------------------------------------------------------
    let create_body = body(
        now(),
        proto::transaction_body::Data::CryptoCreateAccount(proto::CryptoCreateTransactionBody {
            key: Some(proto::Key {
                key: Some(proto::key::Key::EcdsaSecp256k1(public.as_bytes().to_vec())),
            }),
            initial_balance: 500_000_000,
            alias: alias.clone(),
            memo: "ephemeral signer".to_string(),
            ..Default::default()
        }),
    );
    let create_id = create_body.transaction_id.clone().expect("id");
    let response = client
        .create_account(sign_with(&create_body, &[PAYER_KEY]))
        .await
        .expect("createAccount");
    assert_eq!(
        response.into_inner().node_transaction_precheck_code,
        OK,
        "precheck accepts a signed create"
    );
    assert_eq!(
        receipt_status(&mut client, &create_id).await,
        SUCCESS,
        "the receipt is available as soon as submit returns"
    );

    // The receipt carries the new account id; read it back for the transfer.
    let created = {
        let query = proto::Query {
            query: Some(proto::query::Query::TransactionGetReceipt(
                proto::TransactionGetReceiptQuery {
                    header: Some(answer_only()),
                    transaction_id: Some(create_id.clone()),
                    include_duplicates: false,
                    include_child_receipts: false,
                },
            )),
        };
        let response = client
            .get_transaction_receipts(query)
            .await
            .expect("receipt");
        let Some(proto::response::Response::TransactionGetReceipt(receipt)) =
            response.into_inner().response
        else {
            panic!("expected a transactionGetReceipt response");
        };
        let account = receipt
            .receipt
            .expect("receipt")
            .account_id
            .expect("createAccount fills in accountID");
        match account.account.expect("account num") {
            proto::account_id::Account::AccountNum(num) => num,
            other => panic!("expected a number, got {other:?}"),
        }
    };
    assert_eq!(
        balance_of(&mut client, created).await,
        500_000_000,
        "the create funded it from the payer"
    );

    // ---- transfer --------------------------------------------------------------------------
    let before = balance_of(&mut client, RECIPIENT).await;
    let transfer_body = body(
        now(),
        proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
            transfers: Some(proto::TransferList {
                account_amounts: vec![
                    proto::AccountAmount {
                        account_id: Some(account_id(created)),
                        amount: -100_000_000,
                        ..Default::default()
                    },
                    proto::AccountAmount {
                        account_id: Some(account_id(RECIPIENT)),
                        amount: 100_000_000,
                        ..Default::default()
                    },
                ],
            }),
            token_transfers: Vec::new(),
        }),
    );
    let transfer_id = transfer_body.transaction_id.clone().expect("id");
    // The debited account signs for itself, as well as the payer.
    let response = client
        .crypto_transfer(sign_with(&transfer_body, &[PAYER_KEY, &new_key_hex]))
        .await
        .expect("cryptoTransfer");
    assert_eq!(response.into_inner().node_transaction_precheck_code, OK);
    assert_eq!(receipt_status(&mut client, &transfer_id).await, SUCCESS);

    // ---- balance ---------------------------------------------------------------------------
    assert_eq!(
        balance_of(&mut client, RECIPIENT).await,
        before + 100_000_000,
        "the recipient sees the transfer"
    );

    // ---- delete ----------------------------------------------------------------------------
    let delete_body = body(
        now(),
        proto::transaction_body::Data::CryptoDelete(proto::CryptoDeleteTransactionBody {
            transfer_account_id: Some(account_id(PAYER)),
            delete_account_id: Some(account_id(created)),
        }),
    );
    let delete_id = delete_body.transaction_id.clone().expect("id");
    let response = client
        .crypto_delete(sign_with(&delete_body, &[PAYER_KEY, &new_key_hex]))
        .await
        .expect("cryptoDelete");
    assert_eq!(response.into_inner().node_transaction_precheck_code, OK);
    assert_eq!(receipt_status(&mut client, &delete_id).await, SUCCESS);
    assert_eq!(
        balance_of(&mut client, created).await,
        0,
        "the sweep left nothing behind"
    );
}

#[tokio::test]
async fn an_unsigned_body_is_refused_with_invalid_signature() {
    const INVALID_SIGNATURE: i32 = 7;

    let node = common::Node::boot();
    let mut client = CryptoServiceClient::connect(format!("http://127.0.0.1:{}", node.grpc_port))
        .await
        .expect("the gRPC listener accepts h2c");

    let unsigned = {
        let body = body(
            now(),
            proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
                transfers: Some(proto::TransferList {
                    account_amounts: vec![
                        proto::AccountAmount {
                            account_id: Some(account_id(PAYER)),
                            amount: -1,
                            ..Default::default()
                        },
                        proto::AccountAmount {
                            account_id: Some(account_id(RECIPIENT)),
                            amount: 1,
                            ..Default::default()
                        },
                    ],
                }),
                token_transfers: Vec::new(),
            }),
        );
        let signed = proto::SignedTransaction {
            body_bytes: body.encode_to_vec(),
            sig_map: Some(proto::SignatureMap::default()),
            ..Default::default()
        };
        proto::Transaction {
            signed_transaction_bytes: signed.encode_to_vec(),
            ..Default::default()
        }
    };

    let response = client
        .crypto_transfer(unsigned)
        .await
        .expect("transport ok");
    assert_eq!(
        response.into_inner().node_transaction_precheck_code,
        INVALID_SIGNATURE,
        "an unsigned body never reaches consensus"
    );
}

/// A body Hanvil does not execute has to come back as a precheck the network refused, not as a
/// transport failure. Before these services were registered, tonic answered a `TokenCreate` or a
/// `FileCreate` with gRPC status 12 `UNIMPLEMENTED` and an empty message, which the SDK reports
/// as `Error: 12 UNIMPLEMENTED:` — nothing a caller can act on, and not a Hedera status at all.
#[tokio::test]
async fn a_body_hanvil_does_not_execute_is_refused_with_not_supported() {
    const NOT_SUPPORTED: i32 = 13;

    let node = common::Node::boot();
    let endpoint = format!("http://127.0.0.1:{}", node.grpc_port);
    let mut files = proto::file_service_client::FileServiceClient::connect(endpoint.clone())
        .await
        .expect("the gRPC listener accepts h2c");
    let mut tokens = proto::token_service_client::TokenServiceClient::connect(endpoint)
        .await
        .expect("the gRPC listener accepts h2c");

    let file = sign_with(
        &body(
            now(),
            proto::transaction_body::Data::FileCreate(proto::FileCreateTransactionBody {
                contents: b"hanvil".to_vec(),
                ..Default::default()
            }),
        ),
        &[PAYER_KEY],
    );
    let response = files.create_file(file).await.expect("routed, not dropped");
    assert_eq!(
        response.into_inner().node_transaction_precheck_code,
        NOT_SUPPORTED,
        "FileCreate answers a Hedera precheck code, not a gRPC transport error"
    );

    let token = sign_with(
        &body(
            now(),
            proto::transaction_body::Data::TokenCreation(proto::TokenCreateTransactionBody {
                name: "T".to_string(),
                symbol: "T".to_string(),
                treasury: Some(account_id(PAYER)),
                ..Default::default()
            }),
        ),
        &[PAYER_KEY],
    );
    let response = tokens
        .create_token(token)
        .await
        .expect("routed, not dropped");
    assert_eq!(
        response.into_inner().node_transaction_precheck_code,
        NOT_SUPPORTED,
        "TokenCreate answers a Hedera precheck code, not a gRPC transport error"
    );

    node.shutdown();
}
