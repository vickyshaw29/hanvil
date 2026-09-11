//! The chain under the run. `validation/chainSigner.ts` of hedera-harness dev @ 587a2f3 and the
//! fork's `chainSnapshot.ts`, except that here the chain is a struct in this process: the
//! signer is created with `Chain::apply_hapi`, snapshots are `Chain::snapshot`, and assertions
//! read the chain directly.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use alloy_primitives::Address;
use sha2::{Digest as _, Sha384};

use crate::evm::units::Tinybar;
use crate::harness::artifacts::now_iso8601;
use crate::harness::findings::{Category, Finding};
use crate::harness::ledger::Ledger;
use crate::harness::spec::{AccountRef, ChainAssertion, ChainValidation, ContractRef, TopicRef};
use crate::state::hapi::{Body, Digest384, Status, Transaction, TxId};
use crate::state::{Chain, EntityId, FIRST_USER_ID, HAPI_FEE, Timestamp};

/// `validation/chainSigner.ts:9`.
pub(crate) const SIGNER_FILENAME: &str = "chain-signer.json";

/// `types.ts` `ChainSigner`, as persisted in `chain-signer.json` (mode 0600).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Signer {
    /// `0.0.N`.
    pub(crate) account_id: String,
    /// `0x` + 64 hex.
    pub(crate) private_key_hex: String,
    /// `0x` + 40 hex.
    pub(crate) evm_address: String,
    /// `local`; `testnet` when written by hedera-harness.
    pub(crate) network: String,
    /// Persisted only; `toPublicSigner` drops it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) created_at: Option<String>,
}

impl Signer {
    /// `chainSigner.ts:415-422`: the shape handed to prompts and logs.
    pub(crate) fn public(&self) -> Self {
        Self {
            created_at: None,
            ..self.clone()
        }
    }

    /// `chainSigner.ts:90-102`: write with `createdAt`, readable by the owner only.
    pub(crate) fn write(&self, path: &Path) -> std::io::Result<()> {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let persisted = Self {
            created_at: Some(self.created_at.clone().unwrap_or_else(now_iso8601)),
            ..self.clone()
        };
        let json = serde_json::to_string_pretty(&persisted).map_err(std::io::Error::other)?;
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        file.write_all(format!("{json}\n").as_bytes())
    }

    /// `chainSigner.ts:389-405`: a persisted signer, when the file is present and well-formed.
    pub(crate) fn read(path: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(path).ok()?;
        let signer: Self = serde_json::from_str(&raw).ok()?;
        let hex_ok = |value: &str, len: usize| {
            value
                .strip_prefix("0x")
                .is_some_and(|hex| hex.len() == len && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        };
        (matches!(signer.network.as_str(), "local" | "testnet")
            && hex_ok(&signer.private_key_hex, 64)
            && hex_ok(&signer.evm_address, 40))
        .then_some(signer)
    }
}

/// The endpoints the node under the run listens on, and its chain id. Injected into every
/// subprocess and written into the generator prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalChain {
    /// JSON-RPC.
    pub(crate) rpc_url: String,
    /// Mirror REST.
    pub(crate) mirror_url: String,
    /// HAPI gRPC, `host:port`.
    pub(crate) grpc_url: String,
    /// EVM chain id.
    pub(crate) chain_id: u64,
}

impl LocalChain {
    /// `HANVIL_*` and `HEDERA_NETWORK=local` for the generator, deploy commands, validator
    /// commands and the dev server. Upstream gives the agent a mirror URL in a prompt and
    /// nothing else.
    pub(crate) fn env(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("HANVIL_RPC_URL".to_string(), self.rpc_url.clone()),
            ("HANVIL_MIRROR_URL".to_string(), self.mirror_url.clone()),
            ("HANVIL_GRPC_URL".to_string(), self.grpc_url.clone()),
            ("HANVIL_CHAIN_ID".to_string(), self.chain_id.to_string()),
            ("HEDERA_NETWORK".to_string(), "local".to_string()),
        ])
    }
}

/// `chainSigner.ts:239-252`: the signer for deploy commands, plus every `expose.envVars`
/// name set to the private key.
pub(crate) fn deploy_env(signer: &Signer, expose_env_vars: &[String]) -> BTreeMap<String, String> {
    let mut env = BTreeMap::from([
        (
            "HARNESS_SIGNER_ACCOUNT_ID".to_string(),
            signer.account_id.clone(),
        ),
        (
            "HARNESS_SIGNER_EVM_ADDRESS".to_string(),
            signer.evm_address.clone(),
        ),
        (
            "HARNESS_SIGNER_PRIVATE_KEY".to_string(),
            signer.private_key_hex.clone(),
        ),
    ]);
    for name in expose_env_vars {
        env.insert(name.clone(), signer.private_key_hex.clone());
    }
    env
}

/// The fork's `DEFAULT_LOCAL_OPERATOR`: the node's first predefined account, which funds and
/// receives every signer.
const OPERATOR: EntityId = EntityId(FIRST_USER_ID);

/// What stops the harness from working the chain.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `chainSigner.ts:83`.
    #[error("AccountCreateTransaction did not return an account ID.")]
    NoAccountId,
    /// The chain refused the create.
    #[error("creating the signer account: {0}")]
    Create(&'static str),
    /// The chain refused the top-up.
    #[error("funding the signer account: {0}")]
    TopUp(&'static str),
    /// The random key could not be turned into an alias.
    #[error("deriving the signer alias: {0}")]
    Key(#[source] crate::keys::Error),
    /// `chain-signer.json` could not be written.
    #[error("writing {path}: {source}")]
    Write {
        /// The file.
        path: std::path::PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `chainSigner.ts:21-58`, what the run learns about its signer.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Provisioned {
    /// The signer, `createdAt` dropped.
    pub(crate) signer: Signer,
    /// A signer from an earlier cycle was found and is alive.
    pub(crate) reused: bool,
    /// HBAR moved to bring a reused signer back to `fundingHbar`.
    pub(crate) topped_up_hbar: Option<f64>,
    /// A swept signer was found on disk and replaced.
    pub(crate) replaced_deleted: bool,
}

/// `chainSigner.ts:187-223`, best-effort by design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Swept {
    /// The account is deleted and the file is gone.
    pub(crate) success: bool,
    /// Why not.
    pub(crate) error: Option<String>,
}

fn tinybar_from_hbar(hbar: f64) -> Tinybar {
    Tinybar((hbar * 100_000_000.0).round() as u64)
}

fn hbar_from_tinybar(tinybar: Tinybar) -> f64 {
    tinybar.0 as f64 / 100_000_000.0
}

/// `Chain::apply_hapi` does not check for a repeated id (`hapi/wire.rs` does, one layer up):
/// two signers created in the same nanosecond would share a record.
fn unique_tx_id(chain: &Chain, now: Timestamp) -> TxId {
    let mut id = TxId {
        payer: OPERATOR,
        valid_start: now,
        nonce: 0,
        scheduled: false,
    };
    while chain.has_transaction_id(&id) {
        id.valid_start = id.valid_start.next_nano();
    }
    id
}

/// A HAPI transaction from the operator, as the wire layer would have decoded it.
fn operator_transaction(chain: &Chain, now: Timestamp, memo: &str, body: Body) -> Transaction {
    let id = unique_tx_id(chain, now);
    Transaction {
        hash: Digest384(Sha384::digest(format!("hanvil {memo} {id}")).into()),
        id,
        memo: memo.to_string(),
        max_fee: HAPI_FEE,
        valid_duration_seconds: 120,
        body,
    }
}

/// `0.0.N`.
fn parse_entity_id(value: &str) -> Option<EntityId> {
    let mut parts = value.split('.');
    let (shard, realm, num) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || shard != "0" || realm != "0" {
        return None;
    }
    num.parse().ok().map(EntityId)
}

/// `chainSigner.ts:21-58`: reuse the run's signer when it is alive, top it up when short,
/// replace it when it was swept, create it otherwise. Everything happens on `chain` in this
/// process; no signature is built because `apply_hapi` sits below the verifying layer.
pub(crate) fn provision(
    chain: &mut Chain,
    config: &ChainValidation,
    run_directory: &Path,
    now: Timestamp,
) -> Result<Provisioned, Error> {
    let path = run_directory.join(SIGNER_FILENAME);
    let funding = tinybar_from_hbar(config.funding_hbar);
    let mut replaced_deleted = false;
    if let Some(existing) = Signer::read(&path) {
        let alive = parse_entity_id(&existing.account_id)
            .and_then(|id| chain.account(id))
            .filter(|account| !account.deleted)
            .map(|account| account.balance);
        match alive {
            Some(balance) => {
                let topped_up_hbar = if balance.0 < funding.0 {
                    let delta = funding.0 - balance.0;
                    let account =
                        parse_entity_id(&existing.account_id).ok_or(Error::NoAccountId)?;
                    let transfer = operator_transaction(
                        chain,
                        now,
                        "hanvil run signer top-up",
                        Body::Transfer {
                            amounts: vec![
                                (
                                    crate::state::hapi::AccountRef::Id(OPERATOR),
                                    -(delta as i64),
                                ),
                                (crate::state::hapi::AccountRef::Id(account), delta as i64),
                            ],
                        },
                    );
                    let record = chain.apply_hapi(transfer, now);
                    if record.status != Status::Success {
                        return Err(Error::TopUp(record.status.name()));
                    }
                    Some(hbar_from_tinybar(Tinybar(delta)))
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
            None => {
                let _ = std::fs::remove_file(&path);
                replaced_deleted = true;
            }
        }
    }

    let signing = k256::ecdsa::SigningKey::random(&mut rand_core::OsRng);
    let private = signing.to_bytes();
    let (key, alias) = crate::keys::ecdsa_public(&private).map_err(Error::Key)?;
    let create = operator_transaction(
        chain,
        now,
        "hanvil run signer",
        Body::CreateAccount {
            key,
            initial_balance: funding,
            alias: Some(alias),
            memo: "hanvil run signer".to_string(),
        },
    );
    let record = chain.apply_hapi(create, now);
    if record.status != Status::Success {
        return Err(Error::Create(record.status.name()));
    }
    let account_id = record.created_account.ok_or(Error::NoAccountId)?;
    let signer = Signer {
        account_id: account_id.to_string(),
        private_key_hex: format!("0x{}", hex::encode(private)),
        evm_address: format!("{alias:#x}"),
        network: "local".to_string(),
        created_at: None,
    };
    signer.write(&path).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(Provisioned {
        signer,
        reused: false,
        topped_up_hbar: None,
        replaced_deleted,
    })
}

/// `chainSigner.ts:187-223`: delete the signer and sweep its balance back to the operator,
/// then remove the file. A recipe with `sweepBack: false` keeps the account.
pub(crate) fn sweep(
    chain: &mut Chain,
    signer: &Signer,
    config: &ChainValidation,
    run_directory: &Path,
    now: Timestamp,
) -> Swept {
    if !config.sweep_back {
        return Swept {
            success: true,
            error: None,
        };
    }
    let Some(account) = parse_entity_id(&signer.account_id) else {
        return Swept {
            success: false,
            error: Some(format!(
                "signer account id {} is not 0.0.N",
                signer.account_id
            )),
        };
    };
    let delete = operator_transaction(
        chain,
        now,
        "hanvil run signer sweep",
        Body::Delete {
            account,
            transfer_to: OPERATOR,
        },
    );
    let record = chain.apply_hapi(delete, now);
    if record.status != Status::Success {
        return Swept {
            success: false,
            error: Some(record.status.name().to_string()),
        };
    }
    let _ = std::fs::remove_file(run_directory.join(SIGNER_FILENAME));
    Swept {
        success: true,
        error: None,
    }
}

/// Where the chain stood when an attempt began. Assertions on "since the snapshot" count from
/// here; a revert restores the chain to exactly this point, so the mark stays valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Mark {
    /// `hapi_records().count()`.
    pub(crate) records: usize,
    /// `blocks().len()`.
    pub(crate) blocks: usize,
    /// `rejections().count()`.
    pub(crate) rejections: usize,
}

impl Mark {
    /// Take the mark now.
    pub(crate) fn of(chain: &Chain) -> Self {
        Self {
            records: chain.hapi_records().count(),
            blocks: chain.blocks().len(),
            rejections: chain.rejections().count(),
        }
    }
}

/// `Chain::snapshot`, rendered as the `0x…` id `evm_snapshot` would have answered, so the log
/// event reads the same as the fork's.
pub(crate) fn snapshot_id(id: u64) -> String {
    format!("0x{id:x}")
}

fn resolve_account<'a>(
    chain: &'a Chain,
    which: &AccountRef,
    signer: Option<&Signer>,
) -> (String, Option<&'a crate::state::Account>) {
    match which {
        AccountRef::Signer => match signer {
            Some(signer) => (
                signer.account_id.clone(),
                parse_entity_id(&signer.account_id).and_then(|id| chain.account(id)),
            ),
            None => ("signer".to_string(), None),
        },
        AccountRef::Id(id) => (
            id.clone(),
            parse_entity_id(id).and_then(|id| chain.account(id)),
        ),
        AccountRef::Evm(address) => (
            address.clone(),
            address
                .parse::<Address>()
                .ok()
                .and_then(|address| chain.account_by_evm(&address)),
        ),
    }
}

/// The transaction kind an assertion is about, for [`Ledger::attribution`]. Empty means any:
/// an account assertion can fail because of a create, a delete or a transfer.
fn attribution_kind(assertion: &ChainAssertion) -> &str {
    match assertion {
        ChainAssertion::Topic { .. } => "CONSENSUSSUBMITMESSAGE",
        ChainAssertion::Contract { .. } => "ETHEREUMTRANSACTION",
        ChainAssertion::Transactions { kind, .. } => kind,
        ChainAssertion::Rejections { kind, .. } => kind.as_deref().unwrap_or(""),
        ChainAssertion::Account { .. } => "",
    }
}

/// Evaluate `chainValidation.assert` against the chain as it stands. One finding per failed
/// entry, category `chain`, id `chain:<index>:<kind>`.
///
/// A failed assertion states the symptom — the effect that is missing. `ledger` supplies the
/// cause when the chain knows one: a transaction the node refused, or one that reached consensus
/// and failed. That half is invisible to `hedera-harness`, which reads a mirror node, and a
/// mirror node carries nothing that was refused before consensus.
pub(crate) fn run_assertions(
    chain: &Chain,
    assertions: &[ChainAssertion],
    signer: Option<&Signer>,
    since: &Mark,
    ledger: &Ledger,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (index, assertion) in assertions.iter().enumerate() {
        let failure = match assertion {
            ChainAssertion::Account {
                account,
                min_balance_hbar,
                exists,
                deleted,
            } => check_account(chain, account, signer, *min_balance_hbar, *exists, *deleted),
            ChainAssertion::Contract {
                contract,
                deployed,
                event,
                at_least,
            } => check_contract(
                chain,
                contract,
                *deployed,
                event.as_deref(),
                *at_least,
                since,
            ),
            ChainAssertion::Topic {
                topic,
                messages_at_least,
            } => check_topic(chain, topic, *messages_at_least),
            ChainAssertion::Transactions {
                kind,
                payer,
                at_least,
            } => check_transactions(chain, kind, payer.as_ref(), signer, *at_least, since),
            ChainAssertion::Rejections {
                kind,
                payer,
                at_most,
            } => check_rejections(
                chain,
                ledger,
                kind.as_deref(),
                payer.as_ref(),
                signer,
                *at_most,
            ),
        };
        if let Some(reason) = failure {
            let kind = match assertion {
                ChainAssertion::Account { .. } => "account",
                ChainAssertion::Contract { .. } => "contract",
                ChainAssertion::Topic { .. } => "topic",
                ChainAssertion::Transactions { .. } => "transactions",
                ChainAssertion::Rejections { .. } => "rejections",
            };
            let finding = Finding::new(
                format!("chain:{index}:{kind}"),
                Category::Chain,
                format!("Chain assertion {index} ({kind}) failed: {reason}"),
            );
            findings.push(match ledger.attribution(attribution_kind(assertion)) {
                Some(cause) => finding.with_details(cause),
                None => finding,
            });
        }
    }
    findings
}

fn check_account(
    chain: &Chain,
    which: &AccountRef,
    signer: Option<&Signer>,
    min_balance_hbar: Option<f64>,
    exists: Option<bool>,
    deleted: Option<bool>,
) -> Option<String> {
    let (label, account) = resolve_account(chain, which, signer);
    match (exists, account) {
        (Some(false), Some(_)) => return Some(format!("account {label} exists")),
        (Some(false), None) => return None,
        (_, None) => return Some(format!("account {label} does not exist")),
        _ => {}
    }
    let account = account?;
    if let Some(expected) = deleted
        && account.deleted != expected
    {
        return Some(if account.deleted {
            format!("account {label} is deleted")
        } else {
            format!("account {label} is not deleted")
        });
    }
    if let Some(minimum) = min_balance_hbar {
        let balance = hbar_from_tinybar(account.balance);
        if balance < minimum {
            return Some(format!(
                "account {label} holds {balance} ℏ, below the required {minimum} ℏ"
            ));
        }
    }
    None
}

fn check_topic(chain: &Chain, which: &TopicRef, messages_at_least: u64) -> Option<String> {
    let (label, topic) = match which {
        // The newest topic on the chain, not the newest since the mark: after a revert the two
        // are the same, and on `--continue` the app reuses the topic the reloaded chain already
        // holds rather than creating one.
        TopicRef::Created => {
            let newest = chain
                .hapi_records()
                .filter_map(|record| record.created_topic)
                .last();
            match newest {
                Some(id) => (id.to_string(), chain.topic(id)),
                None => return Some("no topic exists on the chain".to_string()),
            }
        }
        TopicRef::Id(id) => (
            id.clone(),
            parse_entity_id(id).and_then(|id| chain.topic(id)),
        ),
    };
    let Some(topic) = topic else {
        return Some(format!("topic {label} does not exist"));
    };
    if topic.sequence_number < messages_at_least {
        return Some(format!(
            "topic {label} has {} message(s), fewer than {messages_at_least}",
            topic.sequence_number
        ));
    }
    None
}

fn check_transactions(
    chain: &Chain,
    kind: &str,
    payer: Option<&AccountRef>,
    signer: Option<&Signer>,
    at_least: u64,
    since: &Mark,
) -> Option<String> {
    let payer_id = payer.map(|which| resolve_account(chain, which, signer));
    let payer_entity = payer_id
        .as_ref()
        .and_then(|(_, account)| account.map(|a| a.id));
    let payer_address = payer_id
        .as_ref()
        .and_then(|(_, account)| account.map(crate::state::Account::evm_address));
    let mut count = chain
        .hapi_records()
        .skip(since.records)
        .filter(|record| record.status == Status::Success && record.kind.name() == kind)
        .filter(|record| payer_entity.is_none_or(|payer| record.id.payer == payer))
        .count() as u64;
    if kind == "ETHEREUMTRANSACTION" {
        // EVM transactions sent over JSON-RPC live in blocks, not in HAPI records.
        count += chain
            .blocks()
            .iter()
            .skip(since.blocks)
            .flat_map(|block| block.transactions.iter())
            .filter_map(|hash| chain.transaction(hash))
            .filter(|tx| tx.receipt.success)
            .filter(|tx| payer_address.is_none_or(|address| tx.from == address))
            .count() as u64;
    }
    if count < at_least {
        let paid_by = payer_id
            .map(|(label, _)| format!(" paid by {label}"))
            .unwrap_or_default();
        return Some(format!(
            "{count} successful {kind} transaction(s){paid_by} since the attempt began, fewer than {at_least}"
        ));
    }
    None
}

/// Code at an address, and the events it emitted since the attempt's snapshot.
///
/// `contract: created` resolves to the newest contract the chain holds, so a recipe can assert on
/// a deployment whose address it never sees — the same way `topic: created` already works.
fn check_contract(
    chain: &Chain,
    which: &ContractRef,
    deployed: bool,
    event: Option<&str>,
    at_least: u64,
    since: &Mark,
) -> Option<String> {
    let (label, address) = match which {
        ContractRef::Created => match chain.contracts().next_back() {
            Some(contract) => (contract.address.to_string(), Some(contract.address)),
            None => return Some("no contract exists on the chain".to_string()),
        },
        ContractRef::Address(address) => (address.clone(), address.parse::<Address>().ok()),
    };
    let has_code = address.is_some_and(|address| !chain.code_by_evm(&address).is_empty());
    match (deployed, has_code) {
        (true, false) => return Some(format!("no contract code at {label}")),
        (false, true) => return Some(format!("contract code is present at {label}")),
        _ => {}
    }
    let (Some(signature), Some(address)) = (event, address) else {
        return None;
    };
    // Validated at load, so a signature that does not hash here is a bug, not a recipe error.
    let topic = crate::evm::event_topic(signature)?;
    let count = chain
        .logs(&crate::state::LogFilter {
            from_block: since.blocks as u64,
            to_block: chain.block_number(),
            addresses: vec![address],
            topics: vec![Some(vec![topic])],
        })
        .len() as u64;
    (count < at_least).then(|| {
        format!("{label} emitted {signature} {count} time(s) since the attempt began, fewer than {at_least}")
    })
}

/// Refusals since the attempt's snapshot, read off the ledger rather than the chain: a refused
/// transaction has no record to count. `hedera-harness` cannot express this assertion at all —
/// its ground truth is a mirror node, and a mirror node has no row for one.
fn check_rejections(
    chain: &Chain,
    ledger: &Ledger,
    kind: Option<&str>,
    payer: Option<&AccountRef>,
    signer: Option<&Signer>,
    at_most: u64,
) -> Option<String> {
    let payer_label = payer.map(|which| resolve_account(chain, which, signer).0);
    let matching: Vec<&crate::harness::ledger::Entry> = ledger
        .rejected()
        .filter(|entry| kind.is_none_or(|kind| entry.kind == kind))
        .filter(|entry| {
            payer_label
                .as_ref()
                .is_none_or(|label| &entry.payer == label)
        })
        .collect();
    let count = matching.len() as u64;
    if count <= at_most {
        return None;
    }
    let of_kind = kind.map(|kind| format!(" {kind}")).unwrap_or_default();
    let paid_by = payer_label
        .map(|label| format!(" from {label}"))
        .unwrap_or_default();
    let reasons = matching
        .iter()
        .map(|entry| entry.result.as_str())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "the node refused {count}{of_kind} submission(s){paid_by}, more than the {at_most} allowed ({reasons}); a refused transaction leaves no record on any Hedera network"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Genesis;

    fn genesis_chain() -> Chain {
        Chain::genesis(&Genesis {
            chain_id: 298,
            accounts_per_type: 2,
            balance: Tinybar::from_hbar(10_000),
            gas_price: Tinybar(71),
            now: Timestamp::from_secs(1_700_000_000),
        })
        .expect("genesis")
    }

    fn config(extra: &str) -> ChainValidation {
        let yaml = format!(
            "schemaVersion: 3\nname: t\nbaseline:\n  commands:\n    - name: install\n      command: \"true\"\nchainValidation:\n  network: local\n{extra}"
        );
        crate::harness::spec::parse(&yaml, Path::new("/p/.harness/spec.yaml"))
            .expect("spec")
            .spec
            .chain_validation
            .expect("chain")
    }

    fn run_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hanvil-chainrun-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn a_signer_is_created_reused_topped_up_and_swept_in_process() {
        let mut chain = genesis_chain();
        let dir = run_dir("signer");
        let now = Timestamp::from_secs(1_700_000_100);
        let operator_before = chain.account(OPERATOR).expect("operator").balance;

        let first =
            provision(&mut chain, &config("  fundingHbar: 10\n"), &dir, now).expect("provision");
        assert!(!first.reused && !first.replaced_deleted && first.topped_up_hbar.is_none());
        let id = parse_entity_id(&first.signer.account_id).expect("id");
        let account = chain.account(id).expect("created");
        assert_eq!(account.balance, Tinybar::from_hbar(10));
        assert_eq!(
            format!("{:#x}", account.evm_address()),
            first.signer.evm_address
        );
        assert!(account.alias.is_some());
        let address = account.evm_address();
        assert_eq!(first.signer.network, "local");
        assert!(dir.join(SIGNER_FILENAME).exists());
        let charged = operator_before.0 - chain.account(OPERATOR).expect("operator").balance.0;
        assert_eq!(charged, Tinybar::from_hbar(10).0 + HAPI_FEE.0);

        // Same run directory, same chain: reused, nothing moved.
        let again = provision(
            &mut chain,
            &config("  fundingHbar: 10\n"),
            &dir,
            now.next_nano(),
        )
        .expect("reuse");
        assert!(again.reused);
        assert_eq!(again.signer, first.signer);
        assert_eq!(again.topped_up_hbar, None);

        // Spent down: topped up back to the funding level.
        chain.set_balance(address, Tinybar::from_hbar(3), now);
        let topped = provision(
            &mut chain,
            &config("  fundingHbar: 10\n"),
            &dir,
            now.next_nano(),
        )
        .expect("top up");
        assert!(topped.reused);
        assert_eq!(topped.topped_up_hbar, Some(7.0));
        assert_eq!(
            chain.account(id).expect("account").balance,
            Tinybar::from_hbar(10)
        );

        // Swept: deleted on chain, file gone, balance back with the operator.
        let swept = sweep(
            &mut chain,
            &first.signer,
            &config(""),
            &dir,
            now.next_nano(),
        );
        assert_eq!(
            swept,
            Swept {
                success: true,
                error: None
            }
        );
        assert!(chain.account(id).expect("account").deleted);
        assert!(!dir.join(SIGNER_FILENAME).exists());

        // A signer file pointing at a swept account is replaced.
        first
            .signer
            .write(&dir.join(SIGNER_FILENAME))
            .expect("write stale");
        let replaced = provision(&mut chain, &config(""), &dir, now.next_nano()).expect("replace");
        assert!(replaced.replaced_deleted && !replaced.reused);
        assert_ne!(replaced.signer.account_id, first.signer.account_id);

        let kept = sweep(
            &mut chain,
            &replaced.signer,
            &config("  sweepBack: false\n"),
            &dir,
            now,
        );
        assert!(kept.success);
        assert!(
            dir.join(SIGNER_FILENAME).exists(),
            "sweepBack: false keeps the file"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn assertions_read_the_chain_since_the_mark() {
        let mut chain = genesis_chain();
        let dir = run_dir("assert");
        let now = Timestamp::from_secs(1_700_000_200);
        let mark = Mark::of(&chain);
        let signer = provision(&mut chain, &config(""), &dir, now)
            .expect("provision")
            .signer;
        let address = format!("0x{}", "ab".repeat(20));
        let yaml = format!(
            "  assert:\n    - account: signer\n      minBalanceHbar: 9\n    - account: signer\n      minBalanceHbar: 11\n    - account: 0.0.999999\n      exists: true\n    - account: 0.0.999999\n      exists: false\n    - account: signer\n      deleted: true\n    - contract: \"{address}\"\n    - contract: \"{address}\"\n      deployed: false\n    - topic: created\n      messagesAtLeast: 1\n    - topic: 0.0.5\n      messagesAtLeast: 0\n    - transactions:\n        type: CRYPTOCREATEACCOUNT\n        atLeast: 1\n    - transactions:\n        type: CRYPTOCREATEACCOUNT\n        payer: signer\n    - transactions:\n        type: ETHEREUMTRANSACTION\n        atLeast: 2\n"
        );
        let findings = run_assertions(
            &chain,
            &config(&yaml).assertions,
            Some(&signer),
            &mark,
            &Ledger::since(&chain, &mark),
        );
        let ids: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "chain:1:account",
                "chain:2:account",
                "chain:4:account",
                "chain:5:contract",
                "chain:7:topic",
                "chain:8:topic",
                "chain:10:transactions",
                "chain:11:transactions",
            ]
        );
        assert!(findings.iter().all(|f| f.category == Category::Chain));
        assert!(
            findings[0]
                .message
                .contains("holds 10 ℏ, below the required 11 ℏ"),
            "{}",
            findings[0].message
        );
        assert_eq!(
            findings[1].message,
            "Chain assertion 2 (account) failed: account 0.0.999999 does not exist"
        );
        assert!(
            findings[2].message.ends_with("is not deleted"),
            "{}",
            findings[2].message
        );
        assert!(
            findings[3].message.contains("no contract code at"),
            "{}",
            findings[3].message
        );
        assert_eq!(
            findings[4].message,
            "Chain assertion 7 (topic) failed: no topic exists on the chain"
        );
        assert!(
            findings[5].message.contains("topic 0.0.5 does not exist"),
            "{}",
            findings[5].message
        );
        assert!(findings[6].message.contains(&format!("0 successful CRYPTOCREATEACCOUNT transaction(s) paid by {} since the attempt began, fewer than 1", signer.account_id)), "{}", findings[6].message);
        assert!(
            findings[7].message.starts_with(
                "Chain assertion 11 (transactions) failed: 0 successful ETHEREUMTRANSACTION"
            ),
            "{}",
            findings[7].message
        );

        // A later mark sees none of the provisioning.
        let later = Mark::of(&chain);
        let none_since = run_assertions(
            &chain,
            &config("  assert:\n    - transactions:\n        type: CRYPTOCREATEACCOUNT\n")
                .assertions,
            Some(&signer),
            &later,
            &Ledger::since(&chain, &later),
        );
        assert_eq!(none_since.len(), 1);
        assert_eq!(snapshot_id(26), "0x1a");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// `contract: created` needs no address, and the event count is read off the logs the
    /// attempt's own blocks carry.
    #[test]
    fn a_contract_assertion_finds_the_newest_deployment_and_counts_its_events() {
        use crate::state::UnsignedTx;

        let mut chain = genesis_chain();
        let now = Timestamp::from_secs(1_700_000_300);
        let sender = chain
            .accounts()
            .find(|account| account.id == OPERATOR)
            .map(crate::state::Account::evm_address)
            .expect("the operator");
        let mark = Mark::of(&chain);

        let fixture: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/Counter.json"
            ))
            .expect("fixture"),
        )
        .expect("json");
        let init_code = hex_bytes(fixture["bytecode"].as_str().expect("bytecode"));

        let deploy =
            |chain: &mut Chain, to: Option<Address>, input: Vec<u8>, nonce: u64, at: Timestamp| {
                chain
                    .send_unsigned(
                        UnsignedTx {
                            from: sender,
                            to,
                            nonce,
                            gas_limit: 3_000_000,
                            gas_price: 71,
                            value: 0,
                            input: input.into(),
                        },
                        at,
                    )
                    .expect("mined")
            };
        let hash = deploy(&mut chain, None, init_code, 0, now);
        let deployed = chain
            .transaction(&hash)
            .and_then(|tx| tx.receipt.contract_address)
            .expect("an address");

        // increment() twice: two Incremented logs.
        let increment = hex_bytes("0xd09de08a");
        deploy(
            &mut chain,
            Some(deployed),
            increment.clone(),
            1,
            now.next_nano(),
        );
        deploy(&mut chain, Some(deployed), increment, 2, now.next_nano());

        let assertions = config(
            "  assert:\n    - contract: created\n      event: \"Incremented(address,uint256)\"\n      atLeast: 2\n    - contract: created\n      event: \"Incremented(address,uint256)\"\n      atLeast: 3\n    - contract: created\n      event: \"Paused()\"\n",
        )
        .assertions;
        let findings = run_assertions(
            &chain,
            &assertions,
            None,
            &mark,
            &Ledger::since(&chain, &mark),
        );

        // 0 passes; 1 wanted three; 2 wanted an event the contract never emits.
        let ids: Vec<&str> = findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["chain:1:contract", "chain:2:contract"]);
        assert!(
            findings[0].message.contains(&format!(
                "{deployed} emitted Incremented(address,uint256) 2 time(s) since the attempt began, fewer than 3"
            )),
            "{}",
            findings[0].message
        );

        // A chain with no contract says so rather than counting zero events.
        let empty = genesis_chain();
        let empty_mark = Mark::of(&empty);
        let none = run_assertions(
            &empty,
            &assertions,
            None,
            &empty_mark,
            &Ledger::since(&empty, &empty_mark),
        );
        assert_eq!(none.len(), 3);
        assert!(
            none[0].message.ends_with("no contract exists on the chain"),
            "{}",
            none[0].message
        );
    }

    fn hex_bytes(hex: &str) -> Vec<u8> {
        let hex = hex.trim_start_matches("0x");
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex"))
            .collect()
    }

    /// The assertion `hedera-harness` cannot express: an app that had transactions refused.
    #[test]
    fn the_rejections_assertion_fails_on_what_no_mirror_node_would_show() {
        use crate::state::{BodyKind, Rejection};

        let mut chain = genesis_chain();
        let signer = signer();
        let mark = Mark::of(&chain);
        let at = Timestamp::from_secs(1_700_000_100);

        // Two refusals: one the signer caused, one from another account.
        chain.reject(Rejection {
            at,
            kind: Some(BodyKind::ConsensusSubmitMessage),
            payer: Some(EntityId(1032)),
            status: Some(Status::InvalidSignature),
            from: None,
            message: String::new(),
        });
        chain.reject(Rejection {
            at,
            kind: Some(BodyKind::CryptoTransfer),
            payer: Some(EntityId(1002)),
            status: Some(Status::InsufficientPayerBalance),
            from: None,
            message: String::new(),
        });
        let ledger = Ledger::since(&chain, &mark);

        let assertions = config(
            "  assert:\n    - rejections: {}\n    - rejections:\n        type: CONSENSUSSUBMITMESSAGE\n        payer: signer\n    - rejections:\n        type: CRYPTODELETE\n    - rejections:\n        atMost: 2\n",
        )
        .assertions;
        let findings = run_assertions(&chain, &assertions, Some(&signer), &mark, &ledger);

        // 0 fails (two refusals, none allowed); 1 fails (the signer's one); 2 and 3 pass.
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].id, "chain:0:rejections");
        assert!(
            findings[0].message.contains(
                "the node refused 2 submission(s), more than the 0 allowed (INSUFFICIENT_PAYER_BALANCE; INVALID_SIGNATURE)"
            ),
            "{}",
            findings[0].message
        );
        assert!(
            findings[0]
                .message
                .ends_with("a refused transaction leaves no record on any Hedera network"),
            "{}",
            findings[0].message
        );
        assert_eq!(findings[1].id, "chain:1:rejections");
        assert!(
            findings[1]
                .message
                .contains("refused 1 CONSENSUSSUBMITMESSAGE submission(s) from 0.0.1032"),
            "{}",
            findings[1].message
        );

        // A clean chain has nothing to report.
        let clean = genesis_chain();
        let clean_mark = Mark::of(&clean);
        assert!(
            run_assertions(
                &clean,
                &assertions,
                Some(&signer),
                &clean_mark,
                &Ledger::since(&clean, &clean_mark),
            )
            .is_empty()
        );
    }

    /// A body that never decoded has no kind, so an unfiltered assertion still counts it.
    #[test]
    fn a_rejection_with_no_kind_is_counted_only_when_no_type_is_named() {
        use crate::state::Rejection;

        let mut chain = genesis_chain();
        let mark = Mark::of(&chain);
        chain.reject(Rejection {
            at: Timestamp::from_secs(1_700_000_100),
            kind: None,
            payer: None,
            status: Some(Status::InvalidTransaction),
            from: None,
            message: String::new(),
        });
        let ledger = Ledger::since(&chain, &mark);

        let any = config("  assert:\n    - rejections: {}\n").assertions;
        assert_eq!(run_assertions(&chain, &any, None, &mark, &ledger).len(), 1);

        let typed =
            config("  assert:\n    - rejections:\n        type: CRYPTOTRANSFER\n").assertions;
        assert!(run_assertions(&chain, &typed, None, &mark, &ledger).is_empty());
    }

    fn signer() -> Signer {
        Signer {
            account_id: "0.0.1032".into(),
            private_key_hex: format!("0x{}", "ab".repeat(32)),
            evm_address: format!("0x{}", "cd".repeat(20)),
            network: "local".into(),
            created_at: None,
        }
    }

    #[test]
    fn the_signer_file_round_trips_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = std::env::temp_dir().join(format!("hanvil-chain-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join(SIGNER_FILENAME);
        signer().write(&path).expect("write");
        assert_eq!(
            std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777,
            0o600
        );
        let raw = std::fs::read_to_string(&path).expect("read");
        assert!(raw.contains("\"createdAt\": \""), "{raw}");
        assert!(raw.contains("\"privateKeyHex\": \"0xabab"), "{raw}");
        let read = Signer::read(&path).expect("parses");
        assert_eq!(read.public(), signer());
        assert!(read.created_at.is_some());
        std::fs::write(
            &path,
            r#"{"accountId":"0.0.1","privateKeyHex":"0x12","evmAddress":"0x34","network":"local"}"#,
        )
        .expect("write");
        assert_eq!(Signer::read(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn env_injection_names_every_endpoint_and_exposed_variable() {
        let env = deploy_env(&signer(), &["DEPLOYER_PRIVATE_KEY".to_string()]);
        assert_eq!(env["HARNESS_SIGNER_ACCOUNT_ID"], "0.0.1032");
        assert_eq!(
            env["DEPLOYER_PRIVATE_KEY"],
            env["HARNESS_SIGNER_PRIVATE_KEY"]
        );
        let local = LocalChain {
            rpc_url: "http://127.0.0.1:7546".into(),
            mirror_url: "http://127.0.0.1:5551".into(),
            grpc_url: "127.0.0.1:50211".into(),
            chain_id: 298,
        }
        .env();
        assert_eq!(local["HEDERA_NETWORK"], "local");
        assert_eq!(local["HANVIL_CHAIN_ID"], "298");
        assert_eq!(local["HANVIL_GRPC_URL"], "127.0.0.1:50211");
    }
}
