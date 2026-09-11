//! Findings and their lifecycle across attempts, and the result shapes the run report carries.
//! `types.ts:224-338` and `findingsLifecycle.ts` of hedera-harness dev @ 587a2f3. Field names
//! are the JSON names: `validation-attempt-N.json` and `report.json` are read by people and by
//! CI scripts written against the TypeScript harness.

use serde::{Deserialize, Serialize};

use crate::harness::command::Execution;
use crate::harness::ledger::Ledger;

/// `types.ts:271-279`, plus `chain` for Hanvil's deterministic chain assertions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Category {
    /// Required or forbidden files.
    Files,
    /// `validators/static.json` assertions.
    Static,
    /// Secret scan.
    Secret,
    /// A command exited non-zero, including deploy commands.
    Commands,
    /// The generator process itself failed or timed out.
    Agent,
    /// SMOKE.
    Playwright,
    /// EVALUATE.
    Eval,
    /// EVALUATE could not run for reasons that are not the app's.
    EvalInfra,
    /// Hanvil: a `chainValidation.assert` entry.
    Chain,
}

impl Category {
    /// The name as it appears in `[category]` console lines.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Static => "static",
            Self::Secret => "secret",
            Self::Commands => "commands",
            Self::Agent => "agent",
            Self::Playwright => "playwright",
            Self::Eval => "eval",
            Self::EvalInfra => "eval-infra",
            Self::Chain => "chain",
        }
    }
}

/// `types.ts:280-286`. `fixed` findings are carried forward from a prior attempt to show what
/// the last repair closed; they are not failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Status {
    /// Still failing.
    Open,
    /// Closed by the last attempt.
    Fixed,
}

/// `types.ts:269-291`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Finding {
    /// Stable across attempts; the lifecycle keys on it.
    pub(crate) id: String,
    /// Which stage produced it.
    pub(crate) category: Category,
    /// One line.
    pub(crate) message: String,
    /// Truncated output or evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) details: Option<String>,
    /// Lifecycle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) status: Option<Status>,
    /// Evaluate-checklist assertion id when category is `eval`, e.g. `E7`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) assertion: Option<String>,
    /// Route of an eval finding, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) route: Option<String>,
}

impl Finding {
    /// A finding with only the required fields.
    pub(crate) fn new(
        id: impl Into<String>,
        category: Category,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            category,
            message: message.into(),
            details: None,
            status: None,
            assertion: None,
            route: None,
        }
    }

    /// Attach details.
    pub(crate) fn with_details(mut self, details: impl Into<String>) -> Self {
        self.details = Some(details.into());
        self
    }

    /// The console form: `heading`, then the details indented under it. A finding whose cause
    /// is only in its details — a browser gate that failed before its first route says nothing
    /// else — otherwise reaches the caller as "it failed" with nowhere to look.
    pub(crate) fn console_lines(&self, heading: String) -> Vec<String> {
        let mut lines = vec![heading];
        if let Some(details) = self.details.as_deref() {
            let rendered = truncate_details(details);
            lines.extend(
                rendered
                    .lines()
                    .filter(|line| !line.trim().is_empty())
                    .map(|line| format!("    {}", line.trim_end())),
            );
        }
        lines
    }

    /// Not carried forward as `fixed`.
    pub(crate) fn is_open(&self) -> bool {
        self.status != Some(Status::Fixed)
    }
}

/// `types.ts:224-232`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RouteResult {
    /// From the gate config.
    pub(crate) name: String,
    /// From the gate config.
    pub(crate) path: String,
    /// HTTP status, when known.
    pub(crate) status_code: Option<u16>,
    /// Body text reached the minimum length.
    pub(crate) rendered: bool,
    /// Console errors seen on the route.
    pub(crate) console_errors: Vec<String>,
    /// Forbidden strings that were visible.
    pub(crate) forbidden_text_found: Vec<String>,
    /// Wall time for the route.
    pub(crate) duration_ms: u64,
}

/// `types.ts:234-241`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PlaywrightGateResult {
    /// No findings.
    pub(crate) passed: bool,
    /// The gate YAML.
    pub(crate) config_path: String,
    /// The dev server as reached.
    pub(crate) server_url: String,
    /// The dev server as started.
    pub(crate) server_command: String,
    /// One per configured route.
    pub(crate) routes: Vec<RouteResult>,
    /// Wall time for the gate.
    pub(crate) duration_ms: u64,
    /// Hanvil: time to the browser's first `about:blank`, before any route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) browser_launch_ms: Option<u64>,
}

/// `types.ts:243-250`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ValidatorIssue {
    /// Slug.
    pub(crate) id: String,
    /// `E1`, …
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) assertion: Option<String>,
    /// `critical`, `major` or `minor`.
    pub(crate) severity: String,
    /// Where.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) route: Option<String>,
    /// What failed and why.
    pub(crate) message: String,
    /// What the validator saw.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evidence: Option<String>,
}

/// `types.ts:252-256`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ValidatorVerdict {
    /// All assertions verified.
    pub(crate) passed: bool,
    /// Brief overall summary.
    pub(crate) summary: String,
    /// Failed assertions.
    pub(crate) issues: Vec<ValidatorIssue>,
}

/// `types.ts:258-267`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Evaluation {
    /// Verdict passed with no issues and no findings.
    pub(crate) passed: bool,
    /// As parsed from the validator.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) verdict: Option<ValidatorVerdict>,
    /// Issues as findings, plus configuration and runtime findings.
    pub(crate) findings: Vec<Finding>,
    /// The dev server graded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) server_url: Option<String>,
    /// Wall time.
    pub(crate) duration_ms: u64,
    /// True when the failure is harness/agent tooling, not the generated app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) infrastructure_failure: Option<bool>,
    /// Why, when it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) infrastructure_failure_reason: Option<String>,
}

impl Evaluation {
    /// `infrastructureFailure === true`.
    pub(crate) fn is_infrastructure_failure(&self) -> bool {
        self.infrastructure_failure == Some(true)
    }
}

/// `types.ts:293-299`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ValidationResult {
    /// Every gate that ran is clean.
    pub(crate) passed: bool,
    /// Open and carried-forward findings.
    pub(crate) findings: Vec<Finding>,
    /// Validator commands as run.
    pub(crate) command_results: Vec<Execution>,
    /// SMOKE result, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) playwright_gate: Option<PlaywrightGateResult>,
    /// EVALUATE result, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evaluation: Option<Evaluation>,
    /// Hanvil: the chain ledger CHAIN built, carried to the repair and validator prompts. Not
    /// serialised here — it has its own artifact, `logs/chain-ledger-attempt-N.json`, and one
    /// copy of the evidence is enough.
    #[serde(skip)]
    pub(crate) chain_ledger: Option<Ledger>,
}

impl ValidationResult {
    /// `attemptReporting.ts:284-297`: the state before any attempt ran.
    pub(crate) fn not_run_yet() -> Self {
        Self {
            passed: false,
            findings: vec![Finding {
                status: Some(Status::Open),
                ..Finding::new(
                    "generator-not-run",
                    Category::Agent,
                    "Generator did not complete a successful attempt.",
                )
            }],
            command_results: Vec::new(),
            playwright_gate: None,
            chain_ledger: None,
            evaluation: None,
        }
    }
}

/// `findingsLifecycle.ts:4-8`. Per-attempt movement in the finding set — convergence, not
/// just pass/fail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Delta {
    /// Ids failing now.
    pub(crate) open: Vec<String>,
    /// Ids that were failing and are not.
    pub(crate) fixed: Vec<String>,
    /// Ids failing now that were not before.
    pub(crate) introduced: Vec<String>,
}

/// `findingsLifecycle.ts:10-12`: ids, de-duplicated, in order.
pub(crate) fn finding_ids(findings: &[Finding]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    findings
        .iter()
        .filter(|finding| seen.insert(finding.id.as_str()))
        .map(|finding| finding.id.clone())
        .collect()
}

/// `findingsLifecycle.ts:14-27`.
pub(crate) fn compute_delta(previous_open: &[String], findings: &[Finding]) -> Delta {
    let open = finding_ids(findings);
    let current: std::collections::HashSet<&str> = open.iter().map(String::as_str).collect();
    let previous: std::collections::HashSet<&str> =
        previous_open.iter().map(String::as_str).collect();
    Delta {
        fixed: previous_open
            .iter()
            .filter(|id| !current.contains(id.as_str()))
            .cloned()
            .collect(),
        introduced: open
            .iter()
            .filter(|id| !previous.contains(id.as_str()))
            .cloned()
            .collect(),
        open,
    }
}

/// `findingsLifecycle.ts:30-50`: stamp the current findings `open` and re-surface, as `fixed`,
/// the previous attempt's findings this attempt closed.
pub(crate) fn apply_status(
    findings: Vec<Finding>,
    delta: &Delta,
    previous: &[Finding],
) -> Vec<Finding> {
    let mut result: Vec<Finding> = findings
        .into_iter()
        .map(|finding| Finding {
            status: Some(Status::Open),
            ..finding
        })
        .collect();
    let fixed: std::collections::HashSet<&str> = delta.fixed.iter().map(String::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    for finding in previous {
        if fixed.contains(finding.id.as_str()) && seen.insert(finding.id.as_str()) {
            result.push(Finding {
                status: Some(Status::Fixed),
                ..finding.clone()
            });
        }
    }
    result
}

/// `findingsLifecycle.ts:52-60`.
pub(crate) fn format_delta(delta: &Delta) -> String {
    if delta.open.is_empty() && delta.fixed.is_empty() {
        return "no findings".to_string();
    }
    let mut parts = vec![format!("{} open", delta.open.len())];
    if !delta.fixed.is_empty() {
        parts.push(format!("{} fixed", delta.fixed.len()));
    }
    if !delta.introduced.is_empty() {
        parts.push(format!("{} new", delta.introduced.len()));
    }
    parts.join(", ")
}

/// `validation/index.ts:395-399` and its twins: trim, then cut at 1200 with `...`.
pub(crate) fn truncate_details(value: &str) -> String {
    truncate_to(value, 1200)
}

/// Trim, then cut at `max` characters with `...`.
pub(crate) fn truncate_to(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(id: &str) -> Finding {
        Finding::new(id, Category::Commands, format!("{id} failed"))
    }

    #[test]
    fn a_delta_tracks_open_fixed_and_introduced() {
        let previous = vec!["a".to_string(), "b".to_string()];
        let delta = compute_delta(&previous, &[f("b"), f("c"), f("c")]);
        assert_eq!(delta.open, vec!["b", "c"]);
        assert_eq!(delta.fixed, vec!["a"]);
        assert_eq!(delta.introduced, vec!["c"]);
        assert_eq!(format_delta(&delta), "2 open, 1 fixed, 1 new");
        assert_eq!(format_delta(&Delta::default()), "no findings");
        assert_eq!(
            format_delta(&Delta {
                open: vec!["x".into()],
                ..Delta::default()
            }),
            "1 open"
        );
    }

    #[test]
    fn fixed_findings_are_carried_forward_once() {
        let previous = vec![f("a"), f("a"), f("b")];
        let delta = compute_delta(&["a".to_string(), "b".to_string()], &[f("b")]);
        let stamped = apply_status(vec![f("b")], &delta, &previous);
        assert_eq!(stamped.len(), 2);
        assert_eq!(stamped[0].status, Some(Status::Open));
        assert_eq!(stamped[1].id, "a");
        assert_eq!(stamped[1].status, Some(Status::Fixed));
        assert!(stamped[0].is_open());
        assert!(!stamped[1].is_open());
    }

    #[test]
    fn findings_serialise_with_the_upstream_names() {
        let finding = Finding {
            status: Some(Status::Fixed),
            assertion: Some("E1".into()),
            ..f("eval:x").with_details("seen")
        };
        let json = serde_json::to_string(&finding).expect("json");
        assert_eq!(
            json,
            r#"{"id":"eval:x","category":"commands","message":"eval:x failed","details":"seen","status":"fixed","assertion":"E1"}"#
        );
        assert_eq!(
            serde_json::to_string(&Category::EvalInfra).expect("json"),
            "\"eval-infra\""
        );
        let not_run = ValidationResult::not_run_yet();
        assert!(!not_run.passed);
        assert_eq!(not_run.findings[0].id, "generator-not-run");
    }

    #[test]
    fn a_findings_cause_is_printed_under_it() {
        let bare = Finding::new("playwright:gate", Category::Playwright, "gate failed");
        assert_eq!(
            bare.console_lines("- gate failed".into()),
            vec!["- gate failed"]
        );
        let with_cause = Finding::new("playwright:gate", Category::Playwright, "gate failed")
            .with_details("the server closed the connection\n\nnpm error ENOTEMPTY");
        assert_eq!(
            with_cause.console_lines("- gate failed".into()),
            vec![
                "- gate failed",
                "    the server closed the connection",
                "    npm error ENOTEMPTY",
            ]
        );
    }

    #[test]
    fn details_are_cut_at_twelve_hundred() {
        let long = "x".repeat(1_300);
        let cut = truncate_details(&long);
        assert_eq!(cut.len(), 1_203);
        assert!(cut.ends_with("..."));
        assert_eq!(truncate_details("  short  "), "short");
    }
}
