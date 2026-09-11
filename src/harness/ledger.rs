//! The chain ledger: every transaction an attempt caused, in consensus order, including the ones
//! the node refused.
//!
//! `hedera-harness` reads the chain through a mirror node, and a mirror node only carries what
//! reached consensus. A transaction refused at precheck — `INVALID_SIGNATURE`, a nonce the node
//! would not take, a value that is not a whole number of tinybar — leaves no record, no receipt
//! and no mirror row (`hapi/wire.rs:5-7`). An app that catches the error and carries on therefore
//! destroys the only evidence it happened, and a validator agent reading the mirror sees a
//! missing effect with no cause.
//!
//! Hanvil is the node. It keeps refusals on the chain ([`Chain::rejections`]), so the ledger
//! carries both halves: what the app did, and what it was stopped from doing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use alloy_primitives::Address;

use crate::evm;
use crate::harness::chain::Mark;
use crate::state::{Chain, Rejection, Status, Timestamp};

/// How a submission ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Outcome {
    /// Reached consensus and did what it said.
    Success,
    /// Reached consensus, was charged, and failed in the body.
    Failed,
    /// Refused before consensus. No record exists on any Hedera network.
    Rejected,
}

/// One submission the attempt made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Entry {
    /// 1-based, in consensus order.
    pub(crate) index: u64,
    /// `CONSENSUSSUBMITMESSAGE`, `ETHEREUMTRANSACTION`, … or `UNKNOWN` when the body did not
    /// decode far enough to name it.
    pub(crate) kind: String,
    /// `0.0.N`, or the EVM sender for a transaction that never had a Hedera id.
    pub(crate) payer: String,
    /// Whether it reached consensus, and how it ended.
    pub(crate) outcome: Outcome,
    /// `SUCCESS`, a `ResponseCodeEnum` name, a decoded revert reason, or the relay's message.
    pub(crate) result: String,
    /// The `ResponseCodeEnum` number, when the outcome came from HAPI.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) code: Option<i32>,
    /// What it created or touched: `0.0.1033`, `seq 3`, a contract address.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) entity: Option<String>,
    /// Milliseconds after the first entry in this ledger.
    pub(crate) at_millis: i64,
    /// Fee charged, in tinybar. A refused transaction is charged nothing.
    pub(crate) fee_tinybar: u64,
}

/// Every submission since a [`Mark`], in consensus order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Ledger {
    /// In consensus order, 1-based indices.
    pub(crate) entries: Vec<Entry>,
}

/// What the attempt did to the chain, in four numbers. Carried in `report.json` so CI can assert
/// on a run rather than read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Summary {
    /// Everything the attempt sent.
    pub(crate) transactions: usize,
    /// Reached consensus and did what it said.
    pub(crate) succeeded: usize,
    /// Reached consensus, was charged, failed in the body.
    pub(crate) failed: usize,
    /// Refused before consensus. No record exists for these anywhere on Hedera.
    pub(crate) rejected: usize,
}

/// Sort key: consensus order, with rejections placed by the instant the node refused them.
fn at(timestamp: Timestamp) -> i128 {
    i128::from(timestamp.secs) * 1_000_000_000 + i128::from(timestamp.nanos)
}

impl Ledger {
    /// Build the ledger from the chain as it stands, covering everything since `mark`.
    pub(crate) fn since(chain: &Chain, mark: &Mark) -> Self {
        let mut rows: Vec<(i128, Entry)> = Vec::new();

        for record in chain.hapi_records().skip(mark.records) {
            let outcome = if record.status == Status::Success {
                Outcome::Success
            } else {
                Outcome::Failed
            };
            rows.push((
                at(record.consensus_timestamp),
                Entry {
                    index: 0,
                    kind: record.kind.name().to_string(),
                    payer: record.id.payer.to_string(),
                    outcome,
                    result: record.status.name().to_string(),
                    code: Some(record.status.code()),
                    entity: hapi_entity(record),
                    at_millis: 0,
                    fee_tinybar: record.charged_fee.0,
                },
            ));
        }

        // An `ethereumTransaction` submitted over HAPI is in the records above and mines a block
        // as well; its EVM hash links the two, and it is one transaction, listed once.
        let over_hapi: Vec<&[u8]> = chain
            .hapi_records()
            .skip(mark.records)
            .map(|record| record.ethereum_hash.as_slice())
            .filter(|hash| !hash.is_empty())
            .collect();

        for block in chain.blocks().iter().skip(mark.blocks) {
            for hash in &block.transactions {
                if over_hapi.iter().any(|seen| *seen == hash.as_slice()) {
                    continue;
                }
                let Some(tx) = chain.transaction(hash) else {
                    continue;
                };
                let (outcome, result) = if tx.receipt.success {
                    (Outcome::Success, "SUCCESS".to_string())
                } else {
                    (Outcome::Failed, evm_failure(tx))
                };
                rows.push((
                    at(tx.consensus_timestamp),
                    Entry {
                        index: 0,
                        kind: "ETHEREUMTRANSACTION".to_string(),
                        payer: evm_payer(chain, &tx.from),
                        outcome,
                        result,
                        code: None,
                        entity: tx
                            .receipt
                            .contract_address
                            .map(|address| format!("{address} (deploy)")),
                        at_millis: 0,
                        fee_tinybar: tx
                            .receipt
                            .gas_used
                            .saturating_mul(tx.receipt.effective_gas_price),
                    },
                ));
            }
        }

        for rejection in chain.rejections().skip(mark.rejections) {
            rows.push((
                at(rejection.at),
                Entry {
                    index: 0,
                    kind: rejection.kind_name().to_string(),
                    payer: rejected_payer(chain, rejection),
                    outcome: Outcome::Rejected,
                    result: rejection.reason(),
                    code: rejection.status.map(Status::code),
                    entity: None,
                    at_millis: 0,
                    fee_tinybar: 0,
                },
            ));
        }

        rows.sort_by_key(|(key, _)| *key);
        let origin = rows.first().map_or(0, |(key, _)| *key);
        let entries = rows
            .into_iter()
            .enumerate()
            .map(|(position, (key, mut entry))| {
                entry.index = position as u64 + 1;
                entry.at_millis = ((key - origin) / 1_000_000) as i64;
                entry
            })
            .collect();
        Self { entries }
    }

    /// Whether the attempt touched the chain at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Submissions the node refused, in order.
    pub(crate) fn rejected(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Rejected)
    }

    /// Submissions that reached consensus and failed there.
    pub(crate) fn failed(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.outcome == Outcome::Failed)
    }

    /// The four counts, for `report.json`.
    pub(crate) fn counts(&self) -> Summary {
        let rejected = self.rejected().count();
        let failed = self.failed().count();
        Summary {
            transactions: self.entries.len(),
            succeeded: self.entries.len() - rejected - failed,
            failed,
            rejected,
        }
    }

    /// What the last attempt did on the chain that this one did again, when the repair changed
    /// nothing about it. `None` when no refusal repeated — including when there were none.
    ///
    /// A repair that fixes the symptom and leaves the cause sends the same refused transaction
    /// on the next attempt, and nothing else in the run would say so.
    pub(crate) fn repeated_from(&self, previous: &Self) -> Option<String> {
        let tally = |ledger: &Self| {
            let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
            for entry in ledger.rejected() {
                *counts
                    .entry((entry.kind.clone(), entry.result.clone()))
                    .or_default() += 1;
            }
            counts
        };
        let now = tally(self);
        let before = tally(previous);
        let repeated: Vec<String> = now
            .iter()
            .filter(|((kind, result), _)| before.contains_key(&(kind.clone(), result.clone())))
            .map(|((kind, result), count)| format!("{count}× {kind} — {}", clip(result)))
            .collect();
        (!repeated.is_empty()).then(|| repeated.join("; "))
    }

    /// One line summarising the ledger, for the console header.
    pub(crate) fn summary(&self) -> String {
        let rejected = self.rejected().count();
        let failed = self.failed().count();
        let mut summary = format!("{} transaction(s)", self.entries.len());
        if failed > 0 {
            summary.push_str(&format!(", {failed} failed"));
        }
        if rejected > 0 {
            summary.push_str(&format!(", {rejected} rejected before consensus"));
        }
        summary
    }

    /// The ledger as a fixed-width table. Used for the console, the repair prompt and the
    /// validator prompt, so all three read the same rows.
    pub(crate) fn table(&self) -> String {
        const HEAD: [&str; 6] = ["#", "kind", "payer", "result", "entity", "at"];
        let mut rows: Vec<[String; 6]> = vec![HEAD.map(str::to_string)];
        for entry in &self.entries {
            let result = match entry.code {
                Some(code) if entry.outcome != Outcome::Success => {
                    format!("{} {code}", clip(&entry.result))
                }
                _ => clip(&entry.result),
            };
            rows.push([
                entry.index.to_string(),
                entry.kind.clone(),
                entry.payer.clone(),
                match entry.outcome {
                    Outcome::Rejected => format!("REJECTED {result}"),
                    _ => result,
                },
                entry.entity.clone().unwrap_or_else(|| "—".to_string()),
                offset(entry.at_millis),
            ]);
        }
        let widths: Vec<usize> = (0..6)
            .map(|column| {
                rows.iter()
                    .map(|row| row[column].chars().count())
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        rows.iter()
            .map(|row| {
                let mut line = String::new();
                for (column, cell) in row.iter().enumerate() {
                    line.push_str(cell);
                    if column + 1 < row.len() {
                        let pad = widths[column] - cell.chars().count();
                        line.push_str(&" ".repeat(pad + 2));
                    }
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Why an assertion of this kind may have failed, in one sentence, or `None` when the ledger
    /// has nothing to add. Attached to the finding so the repair prompt carries the cause and
    /// not only the symptom.
    pub(crate) fn attribution(&self, kind: &str) -> Option<String> {
        let matching = |entry: &&Entry| kind.is_empty() || entry.kind == kind;
        let rejected: Vec<&Entry> = self.rejected().filter(matching).collect();
        let failed: Vec<&Entry> = self.failed().filter(matching).collect();
        if rejected.is_empty() && failed.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        if !rejected.is_empty() {
            parts.push(format!(
                "{} {} submission(s) were refused before consensus ({}) — no record exists for them on any Hedera network",
                rejected.len(),
                label(kind),
                reasons(&rejected),
            ));
        }
        if !failed.is_empty() {
            parts.push(format!(
                "{} reached consensus and failed ({})",
                failed.len(),
                reasons(&failed),
            ));
        }
        Some(parts.join("; "))
    }
}

/// A relay message runs to a sentence and would set the column width for every row. The table
/// is for reading; `result` in the JSON artifact and in a finding's details is never clipped.
const RESULT_WIDTH: usize = 56;

fn clip(result: &str) -> String {
    if result.chars().count() <= RESULT_WIDTH {
        return result.to_string();
    }
    let kept: String = result.chars().take(RESULT_WIDTH - 1).collect();
    format!("{}…", kept.trim_end())
}

/// Offsets inside a second are where a deploy script's transactions land, so milliseconds are
/// what the column has to show.
fn offset(millis: i64) -> String {
    if millis.abs() < 1_000 {
        format!("+{millis}ms")
    } else {
        format!("+{:.1}s", millis as f64 / 1000.0)
    }
}

fn label(kind: &str) -> &str {
    if kind.is_empty() { "chain" } else { kind }
}

/// The distinct reasons in a set of entries, each with the payer and the count, newest last.
fn reasons(entries: &[&Entry]) -> String {
    let mut seen: Vec<(String, String, usize)> = Vec::new();
    for entry in entries {
        match seen
            .iter_mut()
            .find(|(result, payer, _)| *result == entry.result && *payer == entry.payer)
        {
            Some((_, _, count)) => *count += 1,
            None => seen.push((entry.result.clone(), entry.payer.clone(), 1)),
        }
    }
    seen.iter()
        .map(|(result, payer, count)| match count {
            1 => format!("{result}, payer {payer}"),
            more => format!("{result} ×{more}, payer {payer}"),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// The entity a HAPI record acted on, as a person would name it.
fn hapi_entity(record: &crate::state::Record) -> Option<String> {
    if let Some(topic) = record.created_topic {
        return Some(topic.to_string());
    }
    if let Some(account) = record.created_account {
        return Some(account.to_string());
    }
    if record.topic_sequence_number > 0 {
        return Some(format!("seq {}", record.topic_sequence_number));
    }
    None
}

/// The Hedera id behind an EVM sender when the chain knows one, else the address itself.
fn evm_payer(chain: &Chain, from: &Address) -> String {
    chain
        .account_by_evm(from)
        .map_or_else(|| from.to_string(), |account| account.id.to_string())
}

fn rejected_payer(chain: &Chain, rejection: &Rejection) -> String {
    if let Some(payer) = rejection.payer {
        return payer.to_string();
    }
    match rejection.from {
        Some(address) => evm_payer(chain, &address),
        None => "unknown".to_string(),
    }
}

/// What stopped an EVM transaction, decoded the way viem and ethers decode it.
fn evm_failure(tx: &crate::state::TxRecord) -> String {
    if let Some(reason) = evm::revert_reason(&tx.receipt.output) {
        return format!("reverted: {reason}");
    }
    match &tx.receipt.halt_reason {
        Some(halt) => format!("halted: {halt}"),
        None => "reverted".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evm::units::Tinybar;
    use crate::state::{Body, BodyKind, Genesis, Key, Transaction, TxId};

    const NOW: Timestamp = Timestamp {
        secs: 1_757_000_000,
        nanos: 0,
    };
    const PAYER: crate::state::EntityId = crate::state::EntityId(1002);

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

    fn apply(chain: &mut Chain, body: Body, at: Timestamp) {
        let tx = Transaction {
            id: TxId {
                payer: PAYER,
                valid_start: at,
                nonce: 0,
                scheduled: false,
            },
            memo: String::new(),
            max_fee: Tinybar(100_000_000),
            hash: crate::state::Digest384([0; 48]),
            valid_duration_seconds: 120,
            body,
        };
        chain.apply_hapi(tx, at);
    }

    fn refuse(chain: &mut Chain, kind: BodyKind, status: Status, at: Timestamp) {
        chain.reject(Rejection {
            at,
            kind: Some(kind),
            payer: Some(PAYER),
            status: Some(status),
            from: None,
            message: String::new(),
        });
    }

    fn later(base: Timestamp, millis: u64) -> Timestamp {
        Timestamp {
            secs: base.secs + millis / 1000,
            nanos: ((millis % 1000) * 1_000_000) as u32,
        }
    }

    /// The whole point: a topic that is short of messages, and a ledger that says why.
    #[test]
    fn the_ledger_carries_what_reached_consensus_and_what_was_refused() {
        let mut chain = chain();
        let mark = Mark::of(&chain);

        apply(
            &mut chain,
            Body::CreateTopic {
                memo: "receipts".into(),
                admin_key: None,
                submit_key: Some(Key::Ed25519([7; 32])),
                auto_renew_period: 7_890_000,
                auto_renew_account: None,
            },
            later(NOW, 100),
        );
        let topic = chain
            .hapi_records()
            .last()
            .and_then(|record| record.created_topic)
            .expect("topic created");
        apply(
            &mut chain,
            Body::SubmitMessage {
                topic,
                message: b"one".to_vec(),
            },
            later(NOW, 1_200),
        );
        // The two the node refused: no record, no receipt, no mirror row.
        refuse(
            &mut chain,
            BodyKind::ConsensusSubmitMessage,
            Status::InvalidSignature,
            later(NOW, 1_800),
        );
        refuse(
            &mut chain,
            BodyKind::ConsensusSubmitMessage,
            Status::InvalidSignature,
            later(NOW, 1_900),
        );

        let ledger = Ledger::since(&chain, &mark);
        assert_eq!(ledger.entries.len(), 4);
        assert_eq!(ledger.rejected().count(), 2);
        assert_eq!(ledger.failed().count(), 0);
        assert_eq!(
            ledger.summary(),
            "4 transaction(s), 2 rejected before consensus"
        );

        // Consensus order, and the offsets are measured from the first entry.
        let kinds: Vec<&str> = ledger
            .entries
            .iter()
            .map(|entry| entry.kind.as_str())
            .collect();
        assert_eq!(
            kinds,
            [
                "CONSENSUSCREATETOPIC",
                "CONSENSUSSUBMITMESSAGE",
                "CONSENSUSSUBMITMESSAGE",
                "CONSENSUSSUBMITMESSAGE"
            ]
        );
        assert_eq!(ledger.entries[0].at_millis, 0);
        assert_eq!(ledger.entries[3].at_millis, 1_800);
        assert_eq!(offset(1_800), "+1.8s");
        assert_eq!(offset(6), "+6ms");
        assert_eq!(ledger.entries[0].entity.as_deref(), Some("0.0.1008"));
        assert_eq!(ledger.entries[1].entity.as_deref(), Some("seq 1"));
        assert_eq!(ledger.entries[1].fee_tinybar, crate::state::HAPI_FEE.0);
        assert_eq!(ledger.entries[3].fee_tinybar, 0, "a refusal costs nothing");

        let cause = ledger
            .attribution("CONSENSUSSUBMITMESSAGE")
            .expect("the ledger knows why");
        assert_eq!(
            cause,
            "2 CONSENSUSSUBMITMESSAGE submission(s) were refused before consensus \
             (INVALID_SIGNATURE ×2, payer 0.0.1002) — no record exists for them on any Hedera network"
        );
    }

    #[test]
    fn the_counts_add_up_and_a_repeated_refusal_is_named() {
        let mut first_chain = chain();
        let mark = Mark::of(&first_chain);
        apply(
            &mut first_chain,
            Body::CreateTopic {
                memo: String::new(),
                admin_key: None,
                submit_key: None,
                auto_renew_period: 7_890_000,
                auto_renew_account: None,
            },
            later(NOW, 10),
        );
        refuse(
            &mut first_chain,
            BodyKind::ConsensusSubmitMessage,
            Status::InvalidSignature,
            later(NOW, 20),
        );
        let first = Ledger::since(&first_chain, &mark);
        assert_eq!(
            first.counts(),
            Summary {
                transactions: 2,
                succeeded: 1,
                failed: 0,
                rejected: 1,
            }
        );

        // The repair changed nothing about the cause: the same refusal, again.
        let mut second_chain = chain();
        let second_mark = Mark::of(&second_chain);
        refuse(
            &mut second_chain,
            BodyKind::ConsensusSubmitMessage,
            Status::InvalidSignature,
            later(NOW, 5),
        );
        let second = Ledger::since(&second_chain, &second_mark);
        assert_eq!(
            second.repeated_from(&first).as_deref(),
            Some("1× CONSENSUSSUBMITMESSAGE — INVALID_SIGNATURE")
        );

        // A different refusal is not a repeat, and neither is none at all.
        let mut third_chain = chain();
        let third_mark = Mark::of(&third_chain);
        refuse(
            &mut third_chain,
            BodyKind::CryptoTransfer,
            Status::InsufficientPayerBalance,
            later(NOW, 5),
        );
        assert_eq!(
            Ledger::since(&third_chain, &third_mark).repeated_from(&first),
            None
        );
        assert_eq!(Ledger::default().repeated_from(&first), None);
    }

    #[test]
    fn a_clean_attempt_has_nothing_to_attribute() {
        let mut chain = chain();
        let mark = Mark::of(&chain);
        apply(
            &mut chain,
            Body::CreateTopic {
                memo: String::new(),
                admin_key: None,
                submit_key: None,
                auto_renew_period: 7_890_000,
                auto_renew_account: None,
            },
            later(NOW, 10),
        );
        let ledger = Ledger::since(&chain, &mark);
        assert_eq!(ledger.summary(), "1 transaction(s)");
        assert_eq!(ledger.attribution(""), None);
        assert!(!ledger.is_empty());
    }

    /// A relay message is a sentence; the table clips it and the artifact does not.
    #[test]
    fn a_long_relay_message_is_clipped_in_the_table_only() {
        let mut chain = chain();
        let mark = Mark::of(&chain);
        chain.reject(Rejection {
            at: later(NOW, 6),
            kind: Some(BodyKind::EthereumTransaction),
            payer: Some(PAYER),
            status: None,
            from: None,
            message: "Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); \
                      the relay rejects such values"
                .into(),
        });
        let ledger = Ledger::since(&chain, &mark);
        assert_eq!(
            ledger.entries[0].result,
            "Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values"
        );
        let row = ledger.table().lines().nth(1).expect("a row").to_string();
        assert!(
            row.contains("REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…"),
            "{row}"
        );
        assert!(row.ends_with("+0ms"), "the first row is the origin: {row}");
        // The cause the agent is given keeps the whole sentence.
        assert!(
            ledger
                .attribution("ETHEREUMTRANSACTION")
                .expect("a cause")
                .contains("the relay rejects such values"),
            "clipping is for the table only"
        );
    }

    #[test]
    fn the_table_aligns_and_marks_refusals() {
        let mut chain = chain();
        let mark = Mark::of(&chain);
        refuse(
            &mut chain,
            BodyKind::CryptoTransfer,
            Status::InsufficientPayerBalance,
            later(NOW, 500),
        );
        let table = Ledger::since(&chain, &mark).table();
        let lines: Vec<&str> = table.lines().collect();
        assert_eq!(
            lines[0],
            "#  kind            payer     result                                  entity  at"
        );
        assert_eq!(
            lines[1],
            "1  CRYPTOTRANSFER  0.0.1002  REJECTED INSUFFICIENT_PAYER_BALANCE 10  —       +0ms"
        );
    }

    /// The mark is what separates the attempt from the signer provisioning that came before it.
    #[test]
    fn the_ledger_starts_at_the_mark() {
        let mut chain = chain();
        refuse(
            &mut chain,
            BodyKind::CryptoTransfer,
            Status::InvalidSignature,
            later(NOW, 10),
        );
        let mark = Mark::of(&chain);
        assert!(Ledger::since(&chain, &mark).is_empty());
    }
}
