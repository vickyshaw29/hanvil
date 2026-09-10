//! EVALUATE: an adversarial validator agent grades the live app against the evaluate checklist.
//! `evaluation.ts`, `validatorVerdictParser.ts` and `evalInfra.ts` of hedera-harness dev
//! @ 587a2f3. The verdict is a JSON object the agent must print; failures that are the harness's
//! own tooling — no browser, no MCP — are told apart from the app's so no repair is spent on them.

use std::path::Path;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::Value;

use crate::harness::agent::{Provider, RunInput};
use crate::harness::artifacts::{Layout, write_prompt_file};
use crate::harness::chain::Signer;
use crate::harness::findings::{
    Category, Evaluation, Finding, ValidatorIssue, ValidatorVerdict, truncate_details,
};
use crate::harness::prompt;
use crate::harness::spec::Spec;

/// `evalInfra.ts:3-9`.
const INFRA_FINDING_ID_PREFIXES: [&str; 5] = [
    "validator-config",
    "validator-runtime",
    "validator-exit:",
    "validator-output-unparseable",
    "validator-empty-issues",
];

/// `evalInfra.ts:11-47`. Text that says the evaluator had no browser, no MCP, or no chain,
/// none of which is the app's fault.
static INFRA_TEXT_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)user rejected mcp",
        r"(?i)playwright mcp(?: browser tools)? (?:were |was )?(?:rejected|unavailable)",
        r"(?i)mcp(?: tool)?s? (?:were |was )?(?:rejected|unavailable)",
        r"(?i)no playwright mcp",
        r"(?i)playwright/?mcp.*(unavailable|rejected)",
        r"(?i)browser[_ ]navigate was rejected",
        r"(?i)browser .{0,40}is not installed",
        r"(?i)browser[- _]unavailable",
        r"(?i)executable doesn't exist",
        r"(?i)expected executable at",
        r"(?i)install-browser",
        r"(?i)chrome-for-testing",
        r"(?i)failed to launch",
        r"(?i)no browser session could be started",
        r"(?i)browser automation (?:was )?(?:unavailable|could not|failed)",
        r"(?i)could not drive .{0,80} in a browser",
        r"(?i)without browser access",
        r"(?i)no browser access available",
        r"(?i)no[- ]live[- ]browser",
        r"(?i)evaluator-no-browser",
        r"(?i)shell commands to run browser automation were rejected",
        r"(?i)webfetch cannot reach localhost",
        r"(?i)mirror[_ ]?node (?:unreachable|unavailable|timeout|timed out|failed)",
        r"(?i)testnet(?:/relay)? (?:unreachable|unavailable|timeout|timed out)",
        r"(?i)(?:hashio|json-?rpc(?: relay)?) .{0,60}(?:unreachable|unavailable|timeout|timed out|ECONNREFUSED)",
        r"(?i)insufficient[_ ]payer[_ ]balance",
        r"INSUFFICIENT_PAYER_BALANCE",
        r"(?i)test signer account not found",
        r"(?i)chain signer (?:unavailable|failed|missing)",
        r"(?i)hedera testnet (?:unreachable|unavailable)",
    ]
    .iter()
    .filter_map(|pattern| Regex::new(pattern).ok())
    .collect()
});

/// `evalInfra.ts:141` and `:153`.
static BROWSER_BLOCKED_ISSUE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)browser|mcp|playwright|no-live-browser|evaluator-no-browser|unverified|no browser",
    )
    .ok()
});
static BROWSER_BLOCKED_FINDING: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)browser|mcp|playwright|without browser").ok());

/// `evaluation.ts:23-36`.
pub(crate) struct EvaluationInput<'a> {
    /// The project.
    pub(crate) workspace: &'a Path,
    /// The recipe.
    pub(crate) spec: &'a Spec,
    /// Attempt number, for file names.
    pub(crate) attempt: u64,
    /// Where to write.
    pub(crate) layout: &'a Layout,
    /// The live app.
    pub(crate) server_url: &'a str,
    /// The funded signer, when CHAIN is on.
    pub(crate) signer: Option<&'a Signer>,
    /// `.harness/runtime/context/eval.json`, relative to the workspace.
    pub(crate) eval_relative_path: Option<&'a str>,
    /// `--mcp-config <path> --strict-mcp-config`, or nothing for a workspace-file delivery.
    pub(crate) extra_args: &'a [String],
    /// The mirror the validator is told to read.
    pub(crate) mirror_base_url: &'a str,
    /// Env for the validator process: the network and the signer.
    pub(crate) env: std::collections::BTreeMap<String, String>,
}

fn finding_from_message(id: &str, message: &str, details: Option<&str>) -> Finding {
    let mut finding = Finding::new(id, Category::Eval, message);
    if let Some(details) = details.filter(|d| !d.trim().is_empty()) {
        finding = finding.with_details(truncate_details(details));
    }
    finding
}

fn failure(started: Instant, findings: Vec<Finding>, server_url: Option<&str>) -> Evaluation {
    Evaluation {
        passed: false,
        verdict: None,
        findings,
        server_url: server_url.map(str::to_string),
        duration_ms: started.elapsed().as_millis() as u64,
        infrastructure_failure: None,
        infrastructure_failure_reason: None,
    }
}

/// `evaluation.ts:23-154`.
pub(crate) async fn run(input: EvaluationInput<'_>) -> Evaluation {
    let started = Instant::now();
    let Some(validator) = input.spec.validator.as_ref().filter(|v| v.enabled) else {
        return Evaluation {
            passed: true,
            verdict: None,
            findings: Vec::new(),
            server_url: None,
            duration_ms: 0,
            infrastructure_failure: None,
            infrastructure_failure_reason: None,
        };
    };
    if input.spec.eval_paths.is_none() {
        return annotate_infrastructure_failure(failure(
            started,
            vec![finding_from_message(
                "validator-config",
                "Evaluator requires spec.eval to be configured.",
                None,
            )],
            None,
        ));
    }
    if input.spec.validators.playwright_path.is_none() {
        return annotate_infrastructure_failure(failure(
            started,
            vec![finding_from_message(
                "validator-config",
                "Evaluator requires validators.playwright so the harness can start the dev server.",
                None,
            )],
            None,
        ));
    }

    let eval_relative = input
        .eval_relative_path
        .unwrap_or(".harness-context/eval.json");
    let eval_content = match std::fs::read_to_string(input.workspace.join(eval_relative)) {
        Ok(content) => content,
        Err(error) => {
            return annotate_infrastructure_failure(failure(
                started,
                vec![finding_from_message(
                    "validator-runtime",
                    &error.to_string(),
                    None,
                )],
                Some(input.server_url),
            ));
        }
    };
    let browser_key = input
        .spec
        .chain_validation
        .as_ref()
        .map(|c| c.browser_local_storage_key.as_str())
        .unwrap_or("burnerWallet.pk");
    let prompt = match prompt::build_validator_prompt(
        input.spec,
        &eval_content,
        input.server_url,
        input.signer,
        browser_key,
        input.mirror_base_url,
    ) {
        Ok(prompt) => prompt,
        Err(error) => {
            return annotate_infrastructure_failure(failure(
                started,
                vec![finding_from_message(
                    "validator-runtime",
                    &error.to_string(),
                    None,
                )],
                Some(input.server_url),
            ));
        }
    };
    let prompt_path = input
        .layout
        .prompts_directory
        .join(format!("validator-attempt-{}.txt", input.attempt));
    let secrets: Vec<&str> = input
        .signer
        .map(|s| vec![s.private_key_hex.as_str()])
        .unwrap_or_default();
    if let Err(error) = write_prompt_file(&prompt_path, &prompt, &secrets) {
        return annotate_infrastructure_failure(failure(
            started,
            vec![finding_from_message(
                "validator-runtime",
                &error.to_string(),
                None,
            )],
            Some(input.server_url),
        ));
    }

    let mut config = validator.command.clone();
    if !input.extra_args.is_empty() {
        let mut args = config.args.clone().unwrap_or_default();
        args.extend(input.extra_args.iter().cloned());
        config.args = Some(args);
    }
    let provider = match Provider::new(config, input.spec.agent) {
        Ok(provider) => provider.with_env(input.env.clone()),
        Err(error) => {
            return annotate_infrastructure_failure(failure(
                started,
                vec![finding_from_message(
                    "validator-runtime",
                    &error.to_string(),
                    None,
                )],
                Some(input.server_url),
            ));
        }
    };
    let log_path = input
        .layout
        .logs_directory
        .join(format!("validator-attempt-{}.log", input.attempt));
    let activity_path = input
        .layout
        .logs_directory
        .join(format!("validator-attempt-{}.activity.log", input.attempt));
    let result = match provider
        .run(RunInput {
            prompt: &prompt,
            workspace: input.workspace,
            log_path: Some(&log_path),
            activity_log_path: Some(&activity_path),
            timeout: validator.command.timeout_ms.map(Duration::from_millis),
            on_progress: None,
        })
        .await
    {
        Ok(result) => result,
        Err(error) => {
            return annotate_infrastructure_failure(failure(
                started,
                vec![finding_from_message(
                    "validator-runtime",
                    &error.to_string(),
                    None,
                )],
                Some(input.server_url),
            ));
        }
    };

    if result.exit_code != Some(0) {
        let message = if result.timed_out {
            format!(
                "Validator agent timed out after {}s",
                (result.duration_ms as f64 / 1000.0).round() as u64
            )
        } else {
            format!(
                "Validator agent exited with code {}",
                result
                    .exit_code
                    .map_or_else(|| "null".to_string(), |c| c.to_string())
            )
        };
        let output = if result.stderr.is_empty() {
            &result.stdout
        } else {
            &result.stderr
        };
        return annotate_infrastructure_failure(failure(
            started,
            vec![finding_from_message(
                &format!("validator-exit:{}", input.attempt),
                &message,
                Some(output),
            )],
            Some(input.server_url),
        ));
    }

    let Some(verdict) = parse_verdict(&result.stdout) else {
        return annotate_infrastructure_failure(failure(
            started,
            vec![finding_from_message(
                "validator-output-unparseable",
                "Validator agent did not return a parseable JSON verdict.",
                Some(&result.stdout),
            )],
            Some(input.server_url),
        ));
    };
    let findings = verdict_findings(&verdict);
    annotate_infrastructure_failure(Evaluation {
        passed: verdict.passed && verdict.issues.is_empty() && findings.is_empty(),
        verdict: Some(verdict),
        findings,
        server_url: Some(input.server_url.to_string()),
        duration_ms: started.elapsed().as_millis() as u64,
        infrastructure_failure: None,
        infrastructure_failure_reason: None,
    })
}

/// `evaluation.ts:156-194`.
pub(crate) fn verdict_findings(verdict: &ValidatorVerdict) -> Vec<Finding> {
    let findings: Vec<Finding> = verdict.issues.iter().map(issue_finding).collect();
    if !verdict.passed && findings.is_empty() {
        return vec![finding_from_message(
            "validator-empty-issues",
            "Validator reported failure without listing issues.",
            Some(&verdict.summary),
        )];
    }
    if verdict.passed && !findings.is_empty() {
        let mut all = vec![finding_from_message(
            "validator-inconsistent",
            "Validator reported pass=true but listed issues.",
            Some(&verdict.summary),
        )];
        all.extend(findings);
        return all;
    }
    findings
}

fn issue_finding(issue: &ValidatorIssue) -> Finding {
    let assertion = issue
        .assertion
        .as_ref()
        .map(|a| format!(" [{a}]"))
        .unwrap_or_default();
    let route = issue
        .route
        .as_ref()
        .map(|r| format!(" ({r})"))
        .unwrap_or_default();
    Finding {
        id: format!("eval:{}", issue.id),
        category: Category::Eval,
        message: format!("{}{assertion}{route}: {}", issue.severity, issue.message),
        details: issue.evidence.clone(),
        status: None,
        assertion: issue.assertion.clone(),
        route: issue.route.clone(),
    }
}

/// `validatorVerdictParser.ts:3-28`: every stream-json `result` string first, then the raw
/// stdout; each tried directly, then as fenced blocks, then as balanced objects.
pub(crate) fn parse_verdict(stdout: &str) -> Option<ValidatorVerdict> {
    let mut candidates: Vec<String> = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("result"))
        .filter_map(|event| {
            event
                .get("result")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect();
    candidates.push(stdout.to_string());
    candidates
        .iter()
        .find_map(|candidate| try_parse_verdict(candidate))
}

/// `validatorVerdictParser.ts:46-64`. First-brace-to-last-brace would swallow the prose Claude
/// keeps writing after a fenced verdict.
fn try_parse_verdict(text: &str) -> Option<ValidatorVerdict> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(direct) = try_parse_object(trimmed) {
        return Some(direct);
    }
    for block in fenced_json_blocks(trimmed) {
        if let Some(verdict) = try_parse_object(&block) {
            return Some(verdict);
        }
    }
    balanced_json_objects(trimmed)
        .into_iter()
        .find_map(|object| try_parse_object(&object))
}

/// ```` ```json … ``` ```` and bare ```` ``` … ``` ```` blocks.
fn fenced_json_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find("```") {
        let after = &rest[open + 3..];
        let body_start = if after.to_lowercase().starts_with("json") {
            4
        } else {
            0
        };
        let body = &after[body_start..];
        let Some(close) = body.find("```") else {
            break;
        };
        blocks.push(body[..close].trim().to_string());
        rest = &body[close + 3..];
    }
    blocks
}

/// `validatorVerdictParser.ts:66-106`: every `{ … }` with balanced braces, strings and
/// escapes respected.
pub(crate) fn balanced_json_objects(text: &str) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut objects = Vec::new();
    for (start, byte) in bytes.iter().enumerate() {
        if *byte != b'{' {
            continue;
        }
        if let Some(end) = matching_brace(bytes, start) {
            objects.push(text[start..=end].to_string());
        }
    }
    objects
}

fn matching_brace(bytes: &[u8], start: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape = false;
    for (i, byte) in bytes.iter().enumerate().skip(start) {
        if in_string {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        match byte {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// `validatorVerdictParser.ts:108-154`: `passed` and `summary` are required; malformed issues
/// are dropped, not fatal.
fn try_parse_object(text: &str) -> Option<ValidatorVerdict> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    let passed = parsed.get("passed")?.as_bool()?;
    let summary = parsed.get("summary")?.as_str()?.to_string();
    let issues = parsed
        .get("issues")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(normalise_issue)
        .collect();
    Some(ValidatorVerdict {
        passed,
        summary,
        issues,
    })
}

fn normalise_issue(value: &Value) -> Option<ValidatorIssue> {
    let issue = value.as_object()?;
    let id = issue.get("id")?.as_str()?.to_string();
    let message = issue.get("message")?.as_str()?.to_string();
    let severity = issue.get("severity")?.as_str()?;
    if !matches!(severity, "critical" | "major" | "minor") {
        return None;
    }
    let text = |key: &str| issue.get(key).and_then(Value::as_str).map(str::to_string);
    Some(ValidatorIssue {
        id,
        assertion: text("assertion"),
        severity: severity.to_string(),
        route: text("route"),
        message,
        evidence: text("evidence"),
    })
}

/// `evalInfra.ts:53-92`.
pub(crate) fn detect_infrastructure_failure(result: &Evaluation) -> Option<String> {
    if result.passed {
        return None;
    }
    if let Some(explicit) = result.findings.iter().find(|f| is_explicit_infra_id(&f.id)) {
        let text = if explicit.message.is_empty() {
            explicit.id.clone()
        } else {
            explicit.message.clone()
        };
        return Some(truncate_collapsed(&text, 400));
    }
    let corpus = failure_corpus(result);
    if corpus.trim().is_empty() {
        return None;
    }
    let lower = corpus.to_lowercase();
    if lower.contains("user rejected mcp")
        || BROWSER_BLOCKED_ISSUE.as_ref().is_some_and(|_| {
            Regex::new(r"(?i)browser[_ ]navigate was rejected").is_ok_and(|re| re.is_match(&corpus))
        })
    {
        return Some(
            "Playwright MCP tool calls were rejected (need --force / --approve-mcps for headless validator)."
                .to_string(),
        );
    }
    if let Some(re) = INFRA_TEXT_PATTERNS.get(1)
        && re.is_match(&corpus)
    {
        return Some(
            "Playwright MCP was unavailable or rejected; validator could not drive the live app."
                .to_string(),
        );
    }
    if !looks_like_browser_access_blocked(result, &corpus) {
        return None;
    }
    if !INFRA_TEXT_PATTERNS.iter().any(|re| re.is_match(&corpus)) {
        return None;
    }
    let summary = result
        .verdict
        .as_ref()
        .map(|v| v.summary.trim().to_string())
        .filter(|s| !s.is_empty());
    Some(summary.unwrap_or_else(|| {
        "Evaluator could not access a browser / Playwright MCP (infrastructure), not an app defect."
            .to_string()
    }))
}

/// `evalInfra.ts:94-115`: mark the result and re-categorise its `eval` findings as
/// `eval-infra`, which the repair prompt then leaves out.
pub(crate) fn annotate_infrastructure_failure(mut result: Evaluation) -> Evaluation {
    let Some(reason) = detect_infrastructure_failure(&result) else {
        return result;
    };
    result.infrastructure_failure = Some(true);
    result.infrastructure_failure_reason = Some(truncate_collapsed(&reason, 400));
    for finding in &mut result.findings {
        if finding.category == Category::Eval {
            finding.category = Category::EvalInfra;
        }
    }
    result
}

fn is_explicit_infra_id(id: &str) -> bool {
    INFRA_FINDING_ID_PREFIXES.iter().any(|prefix| {
        if let Some(stem) = prefix.strip_suffix(':') {
            id.starts_with(stem) && id.len() > stem.len() && id.as_bytes()[stem.len()] == b':'
        } else {
            id == *prefix
        }
    })
}

fn failure_corpus(result: &Evaluation) -> String {
    let mut parts = vec![
        result
            .verdict
            .as_ref()
            .map(|v| v.summary.clone())
            .unwrap_or_default(),
    ];
    parts.extend(result.findings.iter().map(|f| {
        format!(
            "{}\n{}\n{}",
            f.id,
            f.message,
            f.details.clone().unwrap_or_default()
        )
    }));
    if let Some(verdict) = &result.verdict {
        parts.extend(verdict.issues.iter().map(|i| {
            format!(
                "{}\n{}\n{}",
                i.id,
                i.message,
                i.evidence.clone().unwrap_or_default()
            )
        }));
    }
    parts.join("\n")
}

/// `evalInfra.ts:137-161`: at least 80 % of three or more issues (or eval findings) talk about
/// the browser, or any infrastructure pattern matches.
fn looks_like_browser_access_blocked(result: &Evaluation, corpus: &str) -> bool {
    if let (Some(verdict), Some(re)) = (&result.verdict, BROWSER_BLOCKED_ISSUE.as_ref())
        && verdict.issues.len() >= 3
    {
        let blocked = verdict
            .issues
            .iter()
            .filter(|i| {
                re.is_match(&format!(
                    "{} {} {}",
                    i.id,
                    i.message,
                    i.evidence.clone().unwrap_or_default()
                ))
            })
            .count();
        if blocked as f64 / verdict.issues.len() as f64 >= 0.8 {
            return true;
        }
    }
    let eval_findings: Vec<&Finding> = result
        .findings
        .iter()
        .filter(|f| f.category == Category::Eval)
        .collect();
    if let Some(re) = BROWSER_BLOCKED_FINDING.as_ref()
        && eval_findings.len() >= 3
    {
        let blocked = eval_findings
            .iter()
            .filter(|f| {
                re.is_match(&format!(
                    "{} {}",
                    f.message,
                    f.details.clone().unwrap_or_default()
                ))
            })
            .count();
        if blocked as f64 / eval_findings.len() as f64 >= 0.8 {
            return true;
        }
    }
    INFRA_TEXT_PATTERNS.iter().any(|re| re.is_match(corpus))
}

fn truncate_collapsed(value: &str, max: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdicts_are_found_in_stream_json_fences_and_prose() {
        let stream = "{\"type\":\"system\"}\n{\"type\":\"result\",\"result\":\"Here you go:\\n```json\\n{\\\"passed\\\": false, \\\"summary\\\": \\\"one broke\\\", \\\"issues\\\": [{\\\"id\\\": \\\"e1-x\\\", \\\"assertion\\\": \\\"E1\\\", \\\"severity\\\": \\\"major\\\", \\\"route\\\": \\\"/\\\", \\\"message\\\": \\\"nope\\\", \\\"evidence\\\": \\\"saw it\\\"}, {\\\"id\\\": \\\"bad\\\", \\\"severity\\\": \\\"wild\\\", \\\"message\\\": \\\"dropped\\\"}]}\\n```\\nnotes: enc: {alg: aes}\"}\n";
        let verdict = parse_verdict(stream).expect("verdict");
        assert!(!verdict.passed);
        assert_eq!(verdict.summary, "one broke");
        assert_eq!(
            verdict.issues.len(),
            1,
            "an issue with an unknown severity is dropped"
        );
        assert_eq!(verdict.issues[0].assertion.as_deref(), Some("E1"));

        let prose = "Thinking… the answer is {\"passed\": true, \"summary\": \"all good\"} and more {not json}";
        let verdict = parse_verdict(prose).expect("balanced object");
        assert!(verdict.passed && verdict.issues.is_empty());
        assert_eq!(
            parse_verdict("{\"passed\": \"yes\", \"summary\": \"x\"}"),
            None
        );
        assert_eq!(parse_verdict("no braces here"), None);
        assert_eq!(
            balanced_json_objects("a {\"k\": \"}\\\"\"} b {x}"),
            vec!["{\"k\": \"}\\\"\"}", "{x}"]
        );
        assert_eq!(
            fenced_json_blocks("```json\n{\"a\":1}\n```\n```\n{\"b\":2}\n```"),
            vec!["{\"a\":1}", "{\"b\":2}"]
        );
    }

    #[test]
    fn issues_map_to_findings_with_upstreams_wording() {
        let verdict = ValidatorVerdict {
            passed: false,
            summary: "s".into(),
            issues: vec![ValidatorIssue {
                id: "home-blank".into(),
                assertion: Some("E2".into()),
                severity: "critical".into(),
                route: Some("/".into()),
                message: "blank page".into(),
                evidence: Some("nothing rendered".into()),
            }],
        };
        let findings = verdict_findings(&verdict);
        assert_eq!(findings[0].id, "eval:home-blank");
        assert_eq!(findings[0].message, "critical [E2] (/): blank page");
        assert_eq!(findings[0].details.as_deref(), Some("nothing rendered"));
        assert_eq!(findings[0].category, Category::Eval);

        let empty = ValidatorVerdict {
            passed: false,
            summary: "why".into(),
            issues: vec![],
        };
        assert_eq!(verdict_findings(&empty)[0].id, "validator-empty-issues");
        let inconsistent = ValidatorVerdict {
            passed: true,
            ..verdict.clone()
        };
        let findings = verdict_findings(&inconsistent);
        assert_eq!(findings[0].id, "validator-inconsistent");
        assert_eq!(findings.len(), 2);
    }

    fn evaluation(findings: Vec<Finding>, verdict: Option<ValidatorVerdict>) -> Evaluation {
        Evaluation {
            passed: false,
            verdict,
            findings,
            server_url: None,
            duration_ms: 0,
            infrastructure_failure: None,
            infrastructure_failure_reason: None,
        }
    }

    #[test]
    fn infrastructure_failures_are_recognised_and_recategorised() {
        assert_eq!(INFRA_TEXT_PATTERNS.len(), 30, "every pattern compiles");
        let explicit = annotate_infrastructure_failure(evaluation(
            vec![Finding::new(
                "validator-exit:2",
                Category::Eval,
                "Validator agent exited with code 1",
            )],
            None,
        ));
        assert_eq!(explicit.infrastructure_failure, Some(true));
        assert_eq!(
            explicit.infrastructure_failure_reason.as_deref(),
            Some("Validator agent exited with code 1")
        );
        assert_eq!(explicit.findings[0].category, Category::EvalInfra);
        assert!(
            is_explicit_infra_id("validator-exit:9")
                && !is_explicit_infra_id("validator-exit")
                && is_explicit_infra_id("validator-config")
        );

        let app_defect = annotate_infrastructure_failure(evaluation(
            vec![Finding::new(
                "eval:x",
                Category::Eval,
                "critical: the button does nothing",
            )],
            Some(ValidatorVerdict {
                passed: false,
                summary: "button broken".into(),
                issues: vec![],
            }),
        ));
        assert_eq!(app_defect.infrastructure_failure, None);
        assert_eq!(app_defect.findings[0].category, Category::Eval);

        let no_browser = annotate_infrastructure_failure(evaluation(
            vec![Finding::new(
                "eval:y",
                Category::Eval,
                "Could not verify: browserType.launch: Executable doesn't exist at /x",
            )],
            Some(ValidatorVerdict {
                passed: false,
                summary: "no browser".into(),
                issues: vec![],
            }),
        ));
        assert_eq!(no_browser.infrastructure_failure, Some(true));
        assert_eq!(
            no_browser.infrastructure_failure_reason.as_deref(),
            Some("no browser")
        );

        let rejected = annotate_infrastructure_failure(evaluation(
            vec![Finding::new(
                "eval:z",
                Category::Eval,
                "User rejected MCP tool",
            )],
            None,
        ));
        assert!(
            rejected
                .infrastructure_failure_reason
                .as_deref()
                .is_some_and(|r| r.starts_with("Playwright MCP tool calls were rejected"))
        );

        let passed = annotate_infrastructure_failure(Evaluation {
            passed: true,
            ..evaluation(vec![], None)
        });
        assert_eq!(passed.infrastructure_failure, None);
    }
}
