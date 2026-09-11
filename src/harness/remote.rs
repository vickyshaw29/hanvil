//! HAPI over the wire, for `chainValidation.network: testnet`.
//!
//! Everywhere else in this binary Hanvil *is* the network: `hapi/` decodes what a client sent and
//! `state/` applies it. Here the direction reverses. The harness builds a `TransactionBody`,
//! signs it with the operator's key, wraps it in the two envelopes a node expects and submits it
//! over gRPC to somebody else's consensus node, then polls for the receipt.
//!
//! This is the only module in the binary that opens an outbound socket, and it is why
//! `.claude/CLAUDE.md` §3.9 is scoped to the node half rather than to the whole binary.
//! `tests/hermetic.rs` fails the build if an outbound client appears anywhere under `state/`,
//! `evm/`, `rpc/`, `mirror/` or `hapi/`.
//!
//! The tonic client stubs are the ones `build.rs` already generates from the same 130 vendored
//! protobuf files the server side uses, so a message that goes out is a message Hanvil could
//! have received.

use std::time::Duration;

use prost::Message as _;

use crate::evm::units::Tinybar;
use crate::hapi::proto;
use crate::hapi::wire::{to_account_id, to_transaction_id};
use crate::keys::sig::Signer;
use crate::state::{EntityId, Key, Status, Timestamp, TxId};

/// `transaction.proto`: the network caps `transactionValidDuration` at 180 s.
const VALID_DURATION_SECS: i64 = 120;

/// What a client offers for a transaction. Real testnet charges a real fee; this is the ceiling,
/// not the price. Two HBAR covers an account create, which is the dearest thing here.
const MAX_FEE_TINYBAR: i64 = 200_000_000;

/// How long to poll for a receipt before giving up. A testnet transaction reaches consensus in
/// seconds; anything past this is an outage, not latency.
const RECEIPT_DEADLINE: Duration = Duration::from_secs(30);

/// `RECEIPT_NOT_FOUND` and `UNKNOWN` mean "ask again", not "it failed".
const RECEIPT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What stops the harness talking to a network.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The endpoint could not be reached, or the call failed at the transport.
    #[error("{network} at {endpoint}: {source}")]
    Transport {
        /// `testnet`, or whatever the recipe named.
        network: String,
        /// `host:port`.
        endpoint: String,
        /// The tonic status or connection error.
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    /// The node refused the transaction before consensus.
    #[error("{what} was refused before consensus: {status:?} ({code})")]
    Precheck {
        /// Which submission.
        what: &'static str,
        /// The `ResponseCodeEnum` name.
        status: String,
        /// Its number.
        code: i32,
    },
    /// It reached consensus and failed there.
    #[error("{what} failed at consensus: {status} ({code})")]
    Consensus {
        /// Which submission.
        what: &'static str,
        /// The `ResponseCodeEnum` name.
        status: String,
        /// Its number.
        code: i32,
    },
    /// No receipt inside [`RECEIPT_DEADLINE`].
    #[error("{what} reached no receipt in {}s; the network is not answering", RECEIPT_DEADLINE.as_secs())]
    ReceiptTimeout {
        /// Which submission.
        what: &'static str,
    },
    /// A response arrived but did not carry what the protocol says it must.
    #[error("{network} answered {what} without {missing}")]
    Malformed {
        /// The network.
        network: String,
        /// Which call.
        what: &'static str,
        /// The field the protocol requires.
        missing: &'static str,
    },
    /// The operator's environment is not set up.
    #[error("{0}")]
    Operator(String),
}

/// A consensus node to talk to, and the account id it answers for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Endpoint {
    /// `host:port`, no scheme.
    pub(crate) address: String,
    /// The node account that address serves. Every transaction body names it, and a node
    /// refuses one addressed to a different account with `INVALID_NODE_ACCOUNT`.
    pub(crate) node: EntityId,
}

impl Endpoint {
    /// Hedera testnet's first consensus node. `0.testnet.hedera.com:50211` is plaintext gRPC;
    /// :50212 is the TLS port, which this does not use.
    pub(crate) fn testnet() -> Self {
        Self {
            address: "0.testnet.hedera.com:50211".to_string(),
            node: EntityId(3),
        }
    }
}

/// The account that pays, and the key that signs for it.
pub(crate) struct Operator {
    /// `0.0.N`.
    pub(crate) account: EntityId,
    /// Its private key. Never rendered.
    pub(crate) signer: Signer,
}

impl std::fmt::Debug for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Operator({}, <redacted>)", self.account)
    }
}

/// A client of somebody else's Hedera network.
pub(crate) struct Remote {
    /// For error messages: `testnet`, or the recipe's name for it.
    pub(crate) network: String,
    /// Where to send.
    pub(crate) endpoint: Endpoint,
    /// Who pays.
    pub(crate) operator: Operator,
}

/// What a submission produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Receipt {
    /// The transaction id, for a record query or a mirror lookup.
    pub(crate) transaction_id: TxId,
    /// Account created, when the body created one.
    pub(crate) created_account: Option<EntityId>,
}

impl Remote {
    /// The URI tonic dials. Plaintext h2c, as the node serves on :50211.
    fn uri(&self) -> String {
        format!("http://{}", self.endpoint.address)
    }

    fn transport(&self, source: impl std::error::Error + Send + Sync + 'static) -> Error {
        Error::Transport {
            network: self.network.clone(),
            endpoint: self.endpoint.address.clone(),
            source: Box::new(source),
        }
    }

    /// A transaction body addressed to this endpoint's node, paid by the operator.
    fn body(&self, now: Timestamp, data: proto::transaction_body::Data) -> proto::TransactionBody {
        proto::TransactionBody {
            transaction_id: Some(to_transaction_id(TxId {
                payer: self.operator.account,
                valid_start: now,
                nonce: 0,
                scheduled: false,
            })),
            node_account_id: Some(to_account_id(self.endpoint.node)),
            transaction_fee: MAX_FEE_TINYBAR as u64,
            transaction_valid_duration: Some(proto::Duration {
                seconds: VALID_DURATION_SECS,
            }),
            memo: String::new(),
            data: Some(data),
            ..Default::default()
        }
    }

    /// Wrap and sign. Every signer in `signers` adds a pair; the operator always signs, and a
    /// body that acts on another account needs that account's key too.
    fn envelope(body: &proto::TransactionBody, signers: &[&Signer]) -> proto::Transaction {
        let body_bytes = body.encode_to_vec();
        let sig_pair = signers
            .iter()
            .map(|signer| {
                let signature = signer.sign(&body_bytes).to_vec();
                let prefix = match signer.public() {
                    Key::EcdsaSecp256k1(bytes) => bytes.to_vec(),
                    Key::Ed25519(bytes) => bytes.to_vec(),
                };
                proto::SignaturePair {
                    pub_key_prefix: prefix,
                    signature: Some(if signer.is_ed25519() {
                        proto::signature_pair::Signature::Ed25519(signature)
                    } else {
                        proto::signature_pair::Signature::EcdsaSecp256k1(signature)
                    }),
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

    /// Submit, check the precheck code, then poll until the receipt says what happened.
    async fn submit(
        &self,
        what: &'static str,
        now: Timestamp,
        data: proto::transaction_body::Data,
        extra: &[&Signer],
    ) -> Result<Receipt, Error> {
        let body = self.body(now, data);
        let id = crate::hapi::wire::transaction_id(body.transaction_id.as_ref()).ok_or(
            Error::Malformed {
                network: self.network.clone(),
                what,
                missing: "a transaction id it was just given",
            },
        )?;
        let mut signers: Vec<&Signer> = vec![&self.operator.signer];
        signers.extend_from_slice(extra);
        let envelope = Self::envelope(&body, &signers);

        let mut client = proto::crypto_service_client::CryptoServiceClient::connect(self.uri())
            .await
            .map_err(|e| self.transport(e))?;
        let response = match &body.data {
            Some(proto::transaction_body::Data::CryptoCreateAccount(_)) => {
                client.create_account(envelope).await
            }
            Some(proto::transaction_body::Data::CryptoDelete(_)) => {
                client.crypto_delete(envelope).await
            }
            _ => client.crypto_transfer(envelope).await,
        }
        .map_err(|e| self.transport(e))?
        .into_inner();

        let precheck = response.node_transaction_precheck_code;
        if precheck != proto::ResponseCodeEnum::Ok as i32 {
            return Err(Error::Precheck {
                what,
                status: code_name(precheck),
                code: precheck,
            });
        }
        self.receipt(what, id).await
    }

    /// Poll `TransactionGetReceipt` until it is not "ask again".
    async fn receipt(&self, what: &'static str, id: TxId) -> Result<Receipt, Error> {
        let mut client = proto::crypto_service_client::CryptoServiceClient::connect(self.uri())
            .await
            .map_err(|e| self.transport(e))?;
        let query = proto::Query {
            query: Some(proto::query::Query::TransactionGetReceipt(
                proto::TransactionGetReceiptQuery {
                    header: Some(proto::QueryHeader {
                        response_type: proto::ResponseType::AnswerOnly as i32,
                        ..Default::default()
                    }),
                    transaction_id: Some(to_transaction_id(id)),
                    ..Default::default()
                },
            )),
        };
        let deadline = std::time::Instant::now() + RECEIPT_DEADLINE;
        loop {
            let response = client
                .get_transaction_receipts(query.clone())
                .await
                .map_err(|e| self.transport(e))?
                .into_inner();
            let Some(proto::response::Response::TransactionGetReceipt(got)) = response.response
            else {
                return Err(Error::Malformed {
                    network: self.network.clone(),
                    what,
                    missing: "a TransactionGetReceipt response",
                });
            };
            let status = got.receipt.as_ref().map_or(0, |r| r.status);
            let pending = status == proto::ResponseCodeEnum::ReceiptNotFound as i32
                || status == proto::ResponseCodeEnum::Unknown as i32
                || status == proto::ResponseCodeEnum::Ok as i32;
            if !pending {
                if status != proto::ResponseCodeEnum::Success as i32 {
                    return Err(Error::Consensus {
                        what,
                        status: code_name(status),
                        code: status,
                    });
                }
                return Ok(Receipt {
                    transaction_id: id,
                    created_account: got
                        .receipt
                        .as_ref()
                        .and_then(|r| r.account_id.as_ref())
                        .and_then(|a| crate::hapi::wire::account_id(Some(a))),
                });
            }
            if std::time::Instant::now() >= deadline {
                return Err(Error::ReceiptTimeout { what });
            }
            tokio::time::sleep(RECEIPT_POLL_INTERVAL).await;
        }
    }

    /// Create a funded account whose key is `signer`'s, and return its id.
    pub(crate) async fn create_account(
        &self,
        signer: &Signer,
        balance: Tinybar,
        now: Timestamp,
    ) -> Result<EntityId, Error> {
        let receipt = self
            .submit(
                "the run's signer",
                now,
                proto::transaction_body::Data::CryptoCreateAccount(
                    proto::CryptoCreateTransactionBody {
                        key: Some(crate::hapi::render::to_proto_key(&signer.public())),
                        initial_balance: balance.0,
                        // An ECDSA key gets an EVM alias, which is what an app connecting over
                        // JSON-RPC needs. The network derives it from the key.
                        alias: Vec::new(),
                        ..Default::default()
                    },
                ),
                // A create is authorised by the payer; the new key does not sign for itself.
                &[],
            )
            .await?;
        receipt.created_account.ok_or(Error::Malformed {
            network: self.network.clone(),
            what: "the run's signer",
            missing: "the created account id in its receipt",
        })
    }

    /// Move tinybar from the operator to `to`.
    pub(crate) async fn fund(
        &self,
        to: EntityId,
        amount: Tinybar,
        now: Timestamp,
    ) -> Result<(), Error> {
        let amount = i64::try_from(amount.0).unwrap_or(i64::MAX);
        self.submit(
            "funding the run's signer",
            now,
            proto::transaction_body::Data::CryptoTransfer(proto::CryptoTransferTransactionBody {
                transfers: Some(proto::TransferList {
                    account_amounts: vec![
                        proto::AccountAmount {
                            account_id: Some(to_account_id(self.operator.account)),
                            amount: -amount,
                            ..Default::default()
                        },
                        proto::AccountAmount {
                            account_id: Some(to_account_id(to)),
                            amount,
                            ..Default::default()
                        },
                    ],
                }),
                ..Default::default()
            }),
            &[],
        )
        .await
        .map(|_| ())
    }

    /// Delete `account`, sending what is left to the operator. The account signs for its own
    /// deletion, so its key is required alongside the payer's.
    pub(crate) async fn delete_account(
        &self,
        account: EntityId,
        signer: &Signer,
        now: Timestamp,
    ) -> Result<(), Error> {
        self.submit(
            "sweeping the run's signer",
            now,
            proto::transaction_body::Data::CryptoDelete(proto::CryptoDeleteTransactionBody {
                delete_account_id: Some(to_account_id(account)),
                transfer_account_id: Some(to_account_id(self.operator.account)),
            }),
            &[signer],
        )
        .await
        .map(|_| ())
    }

    /// Balance of an account, for `doctor`. A balance query is free, so it carries no payment.
    pub(crate) async fn balance(&self, account: EntityId) -> Result<Tinybar, Error> {
        let mut client = proto::crypto_service_client::CryptoServiceClient::connect(self.uri())
            .await
            .map_err(|e| self.transport(e))?;
        let query = proto::Query {
            query: Some(proto::query::Query::CryptogetAccountBalance(
                proto::CryptoGetAccountBalanceQuery {
                    header: Some(proto::QueryHeader {
                        response_type: proto::ResponseType::AnswerOnly as i32,
                        ..Default::default()
                    }),
                    balance_source: Some(
                        proto::crypto_get_account_balance_query::BalanceSource::AccountId(
                            to_account_id(account),
                        ),
                    ),
                },
            )),
        };
        let response = client
            .crypto_get_balance(query)
            .await
            .map_err(|e| self.transport(e))?
            .into_inner();
        let Some(proto::response::Response::CryptogetAccountBalance(got)) = response.response
        else {
            return Err(Error::Malformed {
                network: self.network.clone(),
                what: "a balance query",
                missing: "a CryptoGetAccountBalance response",
            });
        };
        let status = got
            .header
            .as_ref()
            .map_or(0, |h| h.node_transaction_precheck_code);
        if status != proto::ResponseCodeEnum::Ok as i32 {
            return Err(Error::Precheck {
                what: "a balance query",
                status: code_name(status),
                code: status,
            });
        }
        Ok(Tinybar(got.balance))
    }
}

impl Operator {
    /// The operator named by the recipe's `operator.accountIdEnv` / `privateKeyEnv`.
    ///
    /// `hedera-harness` reads the same two variables, so a recipe written for it needs no change.
    /// The key is ECDSA hex, which is what `portal.hedera.com` issues.
    pub(crate) fn from_env(account_env: &str, key_env: &str) -> Result<Self, Error> {
        let id = std::env::var(account_env)
            .ok()
            .filter(|v| !v.trim().is_empty());
        let key = std::env::var(key_env).ok().filter(|v| !v.trim().is_empty());
        let (Some(id), Some(key)) = (id, key) else {
            return Err(Error::Operator(format!(
                "chainValidation.network is \"testnet\" and needs an operator: set {account_env} \
                 and {key_env}. Get both from portal.hedera.com, or use network: local, which \
                 needs neither."
            )));
        };
        let account = crate::harness::chain::parse_entity_id(id.trim()).ok_or_else(|| {
            Error::Operator(format!(
                "{account_env} is {id:?}, which is not an id like 0.0.N"
            ))
        })?;
        let bytes = crate::keys::decode_hex(key.trim())
            .map_err(|_| Error::Operator(format!("{key_env} is not hex")))?;
        let signer = Signer::from_bytes(&bytes, false).ok_or_else(|| {
            Error::Operator(format!(
                "{key_env} is {} bytes; an ECDSA private key is 32",
                bytes.len()
            ))
        })?;
        Ok(Self { account, signer })
    }
}

/// Provision the run's signer on a network Hanvil does not own.
///
/// The same shape as `chain::provision` — reuse a live signer from an earlier cycle, top it up if
/// it has drifted below `fundingHbar`, replace one that was swept — except that "is it alive" is
/// a balance query rather than a map lookup, and a deleted account answers `ACCOUNT_DELETED`
/// rather than carrying a flag.
pub(crate) async fn provision(
    remote: &Remote,
    funding_hbar: f64,
    run_directory: &std::path::Path,
    now: Timestamp,
) -> Result<crate::harness::chain::Provisioned, Error> {
    use crate::harness::chain::{Provisioned, SIGNER_FILENAME, Signer as Persisted};

    let path = run_directory.join(SIGNER_FILENAME);
    let funding = Tinybar((funding_hbar * 100_000_000.0).round() as u64);
    let mut replaced_deleted = false;

    if let Some(existing) = Persisted::read(&path)
        && existing.network == remote.network
        && let Some(account) = crate::harness::chain::parse_entity_id(&existing.account_id)
    {
        match remote.balance(account).await {
            Ok(balance) => {
                let topped_up_hbar = if balance.0 < funding.0 {
                    let delta = Tinybar(funding.0 - balance.0);
                    remote.fund(account, delta, now).await?;
                    Some(delta.0 as f64 / 100_000_000.0)
                } else {
                    None
                };
                return Ok(Provisioned {
                    signer: existing.public(),
                    reused: true,
                    topped_up_hbar,
                    replaced_deleted: false,
                });
            }
            // Deleted, or never existed on this network: fall through and make a new one.
            Err(_) => replaced_deleted = true,
        }
    }

    let signing = k256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let private = signing.to_bytes();
    let (_, alias) = crate::keys::ecdsa_public(&private)
        .map_err(|e| Error::Operator(format!("generating the run's signer: {e}")))?;
    let signer = Signer::from_bytes(&private, false)
        .ok_or_else(|| Error::Operator("generated key is not 32 bytes".to_string()))?;
    let account = remote.create_account(&signer, funding, now).await?;

    let persisted = Persisted {
        account_id: account.to_string(),
        private_key_hex: format!("0x{}", hex::encode(private)),
        evm_address: format!("{alias:#x}"),
        network: remote.network.clone(),
        created_at: None,
    };
    persisted
        .write(&path)
        .map_err(|e| Error::Operator(format!("writing {}: {e}", path.display())))?;
    Ok(Provisioned {
        signer: persisted.public(),
        reused: false,
        topped_up_hbar: None,
        replaced_deleted,
    })
}

/// Delete the run's signer and send what is left back to the operator. Best-effort, as upstream
/// is: a run that passed does not fail because a sweep did.
pub(crate) async fn sweep(
    remote: &Remote,
    persisted: &crate::harness::chain::Signer,
    run_directory: &std::path::Path,
    now: Timestamp,
) -> crate::harness::chain::Swept {
    use crate::harness::chain::{SIGNER_FILENAME, Swept};

    let Some(account) = crate::harness::chain::parse_entity_id(&persisted.account_id) else {
        return Swept {
            success: false,
            error: Some(format!("{} is not an id", persisted.account_id)),
        };
    };
    let Some(bytes) = crate::keys::decode_hex(&persisted.private_key_hex).ok() else {
        return Swept {
            success: false,
            error: Some("the persisted signer key is not hex".to_string()),
        };
    };
    let Some(signer) = Signer::from_bytes(&bytes, false) else {
        return Swept {
            success: false,
            error: Some("the persisted signer key is not 32 bytes".to_string()),
        };
    };
    match remote.delete_account(account, &signer, now).await {
        Ok(()) => {
            let _ = std::fs::remove_file(run_directory.join(SIGNER_FILENAME));
            Swept {
                success: true,
                error: None,
            }
        }
        Err(e) => Swept {
            success: false,
            error: Some(e.to_string()),
        },
    }
}

/// The `ResponseCodeEnum` name for a wire number, so an error says `INVALID_SIGNATURE` and not
/// `7`. Falls back to the number when the node knows a code this build does not.
fn code_name(code: i32) -> String {
    proto::ResponseCodeEnum::try_from(code)
        .map(|c| c.as_str_name().to_string())
        .unwrap_or_else(|_| format!("code {code}"))
}

/// The `ResponseCodeEnum` that `Status` already names, for the codes Hanvil itself models.
#[allow(dead_code)]
pub(crate) fn status_of(code: i32) -> Option<Status> {
    [
        Status::Success,
        Status::InvalidSignature,
        Status::InsufficientPayerBalance,
        Status::InvalidAccountId,
    ]
    .into_iter()
    .find(|status| status.code() == code)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{Chain, Genesis};
    use parking_lot::RwLock;
    use std::sync::Arc;

    const NOW: Timestamp = Timestamp {
        secs: 1_757_000_000,
        nanos: 0,
    };

    /// The node's own clock, fixed, so a valid start is never skewed out of its window.
    struct Fixed;
    impl crate::state::Clock for Fixed {
        fn now(&self) -> Timestamp {
            NOW
        }
    }

    /// A real HAPI server on loopback, serving a real chain.
    ///
    /// This is the whole point of the test: the client is exercised against an implementation of
    /// the protocol, not a mock. Real protobuf encoding, real ECDSA signatures over
    /// `keccak256(bodyBytes)`, real prechecks, real receipts. What it does not cover is public
    /// testnet's node addressing, fee schedule, mirror lag and TLS — named in the README.
    async fn node() -> (Remote, Arc<RwLock<Chain>>, EntityId) {
        let chain = Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 1,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: NOW,
        })
        .expect("genesis");
        let operator = chain
            .accounts()
            .find(|a| a.private_key_hex.is_some())
            .expect("a predefined account");
        let operator_id = operator.id;
        let hex = operator
            .private_key_hex
            .clone()
            .expect("the key the banner prints");
        let shared = Arc::new(RwLock::new(chain));

        let bound = crate::hapi::serve(
            crate::hapi::Node {
                chain: shared.clone(),
                clock: Arc::new(Fixed),
                verify_signatures: true,
            },
            "127.0.0.1",
            0,
        )
        .await
        .expect("bind");

        let private = crate::keys::decode_hex(&hex).expect("hex");
        let remote = Remote {
            network: "loopback".to_string(),
            endpoint: Endpoint {
                address: bound.local_addr.to_string(),
                node: crate::state::NODE,
            },
            operator: Operator {
                account: operator_id,
                signer: Signer::from_bytes(&private, false).expect("32 bytes"),
            },
        };
        (remote, shared, operator_id)
    }

    /// Provision, fund, read the balance, sweep — the whole CHAIN tier, over the wire.
    #[tokio::test]
    async fn the_client_provisions_funds_and_sweeps_against_a_real_hapi_server() {
        let (remote, chain, operator) = node().await;
        let signer = Signer::from_bytes(&[11u8; 32], false).expect("32 bytes");

        let account = remote
            .create_account(&signer, Tinybar::from_hbar(5), NOW)
            .await
            .expect("the node took the create");
        assert!(account.0 >= crate::state::FIRST_USER_ID);
        assert_eq!(
            remote.balance(account).await.expect("balance"),
            Tinybar::from_hbar(5)
        );

        remote
            .fund(account, Tinybar::from_hbar(3), NOW.next_nano())
            .await
            .expect("the node took the transfer");
        assert_eq!(
            remote.balance(account).await.expect("balance"),
            Tinybar::from_hbar(8)
        );

        // The account signs for its own deletion; without its key this is INVALID_SIGNATURE.
        remote
            .delete_account(account, &signer, NOW.next_nano().next_nano())
            .await
            .expect("the node took the delete");
        assert!(
            chain.read().account(account).expect("account").deleted,
            "swept on the chain itself"
        );
        assert!(remote.balance(operator).await.expect("balance").0 > 0);
    }

    /// A body the account did not sign is refused, and the error names the code the node gave.
    #[tokio::test]
    async fn a_missing_signature_comes_back_as_the_networks_own_code() {
        let (remote, _chain, _operator) = node().await;
        let signer = Signer::from_bytes(&[12u8; 32], false).expect("32 bytes");
        let account = remote
            .create_account(&signer, Tinybar::from_hbar(1), NOW)
            .await
            .expect("create");

        // Delete it while signing with the wrong key.
        let wrong = Signer::from_bytes(&[13u8; 32], false).expect("32 bytes");
        let error = remote
            .delete_account(account, &wrong, NOW.next_nano())
            .await
            .expect_err("the node checks signatures");
        let rendered = error.to_string();
        assert!(rendered.contains("INVALID_SIGNATURE"), "{rendered}");
        assert!(
            rendered.contains("refused before consensus"),
            "a precheck failure, not a consensus one: {rendered}"
        );
    }

    /// Nothing listening is a transport error naming the endpoint, not a panic.
    #[tokio::test]
    async fn an_endpoint_that_is_not_there_says_so() {
        let remote = Remote {
            network: "testnet".to_string(),
            endpoint: Endpoint {
                address: "127.0.0.1:1".to_string(),
                node: EntityId(3),
            },
            operator: Operator {
                account: EntityId(2),
                signer: Signer::from_bytes(&[1u8; 32], false).expect("32 bytes"),
            },
        };
        let error = remote.balance(EntityId(2)).await.expect_err("no listener");
        assert!(
            error.to_string().starts_with("testnet at 127.0.0.1:1"),
            "{error}"
        );
    }
}
