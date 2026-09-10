//! What the agents are told. `promptBuilder.ts`, `promptTemplates.ts` and `prompts/*.md` of
//! hedera-harness dev @ 587a2f3 (the validator prompt is the fork's, which names the mirror it
//! points at). The templates are bundled with `include_str!` and a project may override any of
//! them whole-file under `.harness/prompts/`.
//!
//! Two additions of Hanvil's own: the generator prompt gets a `## Local Hedera network` section
//! naming the endpoints and the funded signer, and the repair preamble tells the agent when the
//! chain was reset under it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::harness::artifacts::{CONTEXT_DIR, SKILLS_DIR};
use crate::harness::chain::{LocalChain, Signer};
use crate::harness::findings::{Category, Finding};
use crate::harness::spec::Spec;
use crate::harness::{PROJECT_PROMPTS_DIR, PROMPT_TEMPLATE_NAMES};

/// `contextVendor.ts:14-15`: where the repair prompt looks when no context was vendored.
const LEGACY_CONTEXT_DIR: &str = ".harness-context";

/// What stops a prompt from being built.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `promptTemplates.ts:55-66`.
    #[error("Could not read prompt template {name:?} at {path}. {hint} {source}")]
    Template {
        /// The template.
        name: String,
        /// Where it was looked for.
        path: PathBuf,
        /// Override or bundled advice.
        hint: &'static str,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// Not one of the seven names.
    #[error("no prompt template named {0:?}")]
    UnknownTemplate(String),
    /// The PRD or eval checklist could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// A template variable: text, or a section flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Var {
    /// Rendered where `{{name}}` appears; truthy when non-blank.
    Text(String),
    /// Renders as nothing; drives `{{#name}}` / `{{^name}}`.
    Flag(bool),
}

impl Var {
    fn is_truthy(&self) -> bool {
        match self {
            Self::Flag(flag) => *flag,
            Self::Text(text) => !text.trim().is_empty(),
        }
    }
}

/// Variables by name.
pub(crate) type Vars = BTreeMap<&'static str, Var>;

fn text(value: impl Into<String>) -> Var {
    Var::Text(value.into())
}

fn bundled(name: &str) -> Option<&'static str> {
    Some(match name {
        "generator" => include_str!("prompts/generator.md"),
        "generator-continue" => include_str!("prompts/generator-continue.md"),
        "repair-preamble" => include_str!("prompts/repair-preamble.md"),
        "repair-eval" => include_str!("prompts/repair-eval.md"),
        "repair-runtime" => include_str!("prompts/repair-runtime.md"),
        "repair-broad" => include_str!("prompts/repair-broad.md"),
        "validator" => include_str!("prompts/validator.md"),
        _ => return None,
    })
}

/// `promptTemplates.ts:36-45`: the project override wins, whole-file.
pub(crate) fn override_path(project_root: &Path, name: &str) -> PathBuf {
    project_root
        .join(PROJECT_PROMPTS_DIR)
        .join(format!("{name}.md"))
}

/// `promptTemplates.ts:47-67`.
pub(crate) fn render(project_root: &Path, name: &str, vars: &Vars) -> Result<String, Error> {
    if !PROMPT_TEMPLATE_NAMES.contains(&name) {
        return Err(Error::UnknownTemplate(name.to_string()));
    }
    let override_file = override_path(project_root, name);
    let template = if override_file.exists() {
        std::fs::read_to_string(&override_file).map_err(|source| Error::Template {
            name: name.to_string(),
            path: override_file.clone(),
            hint: "Remove the override under .harness/prompts/ to fall back to the bundled prompt.",
            source,
        })?
    } else {
        bundled(name)
            .ok_or_else(|| Error::UnknownTemplate(name.to_string()))?
            .to_string()
    };
    Ok(render_template(&template, vars))
}

/// `promptTemplates.ts:69-96`: the mustache subset — `{{name}}`, `{{#name}}…{{/name}}`,
/// `{{^name}}…{{/name}}`. Innermost sections resolve first; a section tag alone on a line
/// leaves no blank line behind.
pub(crate) fn render_template(template: &str, vars: &Vars) -> String {
    let mut output = strip_section_only_lines(template);
    for _ in 0..100 {
        let Some((start, end, keep_body, body)) = innermost_section(&output, vars) else {
            break;
        };
        let replacement = if keep_body { body } else { String::new() };
        output.replace_range(start..end, &replacement);
    }
    let output = substitute_variables(&output, vars);
    tidy(&output)
}

/// `SECTION_ONLY_LINE`: `^[ \t]*({{[#^/]name}})[ \t]*\r?\n` → the tag alone.
fn strip_section_only_lines(template: &str) -> String {
    let mut out = String::with_capacity(template.len());
    for (i, line) in template.split_inclusive('\n').enumerate() {
        let _ = i;
        let trimmed = line.trim_end_matches(['\n', '\r']);
        let inner = trimmed.trim_matches([' ', '\t']);
        let is_section_tag = inner.len() > 5
            && inner.starts_with("{{")
            && inner.ends_with("}}")
            && matches!(inner.as_bytes()[2], b'#' | b'^' | b'/')
            && inner[3..inner.len() - 2]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !inner[3..inner.len() - 2].is_empty();
        if is_section_tag && line.ends_with('\n') {
            out.push_str(inner);
        } else {
            out.push_str(line);
        }
    }
    out
}

/// A section tag found in the text.
struct Tag {
    start: usize,
    end: usize,
    kind: u8,
    name: String,
}

fn section_tags(text: &str) -> Vec<Tag> {
    let bytes = text.as_bytes();
    let mut tags = Vec::new();
    let mut i = 0;
    while i + 4 < bytes.len() {
        if &bytes[i..i + 2] == b"{{" && matches!(bytes[i + 2], b'#' | b'^' | b'/') {
            let name_start = i + 3;
            let mut j = name_start;
            while j < bytes.len() && (bytes[j].is_ascii_alphanumeric() || bytes[j] == b'_') {
                j += 1;
            }
            if j > name_start && j + 1 < bytes.len() && &bytes[j..j + 2] == b"}}" {
                tags.push(Tag {
                    start: i,
                    end: j + 2,
                    kind: bytes[i + 2],
                    name: text[name_start..j].to_string(),
                });
                i = j + 2;
                continue;
            }
        }
        i += 1;
    }
    tags
}

/// `INNERMOST_SECTION`: the leftmost opener whose next section tag is its own closer.
fn innermost_section(text: &str, vars: &Vars) -> Option<(usize, usize, bool, String)> {
    let tags = section_tags(text);
    for (index, tag) in tags.iter().enumerate() {
        if tag.kind == b'/' {
            continue;
        }
        let Some(next) = tags.get(index + 1) else {
            continue;
        };
        if next.kind == b'/' && next.name == tag.name {
            let truthy = vars.get(tag.name.as_str()).is_some_and(Var::is_truthy);
            let keep = if tag.kind == b'#' { truthy } else { !truthy };
            let body = text[tag.end..next.start].to_string();
            return Some((tag.start, next.end, keep, body));
        }
    }
    None
}

/// `VARIABLE`: `{{name}}` → the text, or nothing for flags and unknowns.
fn substitute_variables(text: &str, vars: &Vars) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let name_len = after
            .bytes()
            .take_while(|b| b.is_ascii_alphanumeric() || *b == b'_')
            .count();
        if name_len > 0 && after[name_len..].starts_with("}}") {
            let name = &after[..name_len];
            if let Some(Var::Text(value)) = vars.get(name) {
                out.push_str(value);
            }
            rest = &after[name_len + 2..];
        } else {
            out.push_str("{{");
            rest = after;
        }
    }
    out.push_str(rest);
    out
}

/// `promptTemplates.ts:103-107`.
fn tidy(value: &str) -> String {
    let unix = value.replace("\r\n", "\n");
    let mut collapsed = String::with_capacity(unix.len());
    let mut newlines = 0;
    for ch in unix.chars() {
        if ch == '\n' {
            newlines += 1;
            if newlines <= 2 {
                collapsed.push(ch);
            }
        } else {
            newlines = 0;
            collapsed.push(ch);
        }
    }
    collapsed.trim().to_string()
}

/// `contextVendor.ts:17-22`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VendoredContext {
    /// `.harness/runtime/context/prd.md`, relative to the workspace.
    pub(crate) prd_relative_path: String,
    /// `.harness/runtime/context/eval.json`, when EVALUATE is configured.
    pub(crate) eval_relative_path: Option<String>,
    /// The recipe's PRD.
    pub(crate) prd_source_path: PathBuf,
    /// The recipe's eval checklist.
    pub(crate) eval_source_path: Option<PathBuf>,
}

/// `skillProvider.ts:20-26`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VendoredSkill {
    /// From the SKILL.md front matter.
    pub(crate) name: String,
    /// `.harness/runtime/skills/<slug>/SKILL.md`.
    pub(crate) relative_path: String,
    /// From the front matter.
    pub(crate) description: String,
    /// `references/` next to it, when present.
    pub(crate) references_path: Option<String>,
}

/// `attemptLoop.ts:59-62`: position in an ordered `prd:` list. `{0, 1}` renders no framing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Slice {
    /// Zero-based.
    pub(crate) index: usize,
    /// Total increments.
    pub(crate) count: usize,
}

/// The chain the generator is told about.
pub(crate) struct ChainContext<'a> {
    /// Endpoints and chain id.
    pub(crate) local: &'a LocalChain,
    /// The funded signer, when CHAIN is on.
    pub(crate) signer: Option<&'a Signer>,
}

/// `promptBuilder.ts:35-46`.
fn context_paths(
    skills: &[VendoredSkill],
    context: Option<&VendoredContext>,
) -> (String, String, String) {
    let prd = context
        .map(|c| c.prd_relative_path.clone())
        .unwrap_or_else(|| format!("{CONTEXT_DIR}/prd.md"));
    let eval = context
        .and_then(|c| c.eval_relative_path.clone())
        .unwrap_or_else(|| format!("{CONTEXT_DIR}/eval.json"));
    let skills_root = skills
        .first()
        .map(|skill| {
            let parts: Vec<&str> = skill.relative_path.split('/').collect();
            parts[..parts.len().saturating_sub(2)].join("/")
        })
        .filter(|root| !root.is_empty())
        .unwrap_or_else(|| SKILLS_DIR.to_string());
    (prd, eval, skills_root)
}

/// `promptBuilder.ts:252-263`.
fn slice_vars(vars: &mut Vars, slice: Option<Slice>) {
    match slice {
        Some(slice) if slice.count > 1 => {
            vars.insert("hasSlices", Var::Flag(true));
            vars.insert("sliceNumber", text((slice.index + 1).to_string()));
            vars.insert("sliceCount", text(slice.count.to_string()));
            vars.insert("hasCompletedSlices", Var::Flag(slice.index > 0));
            vars.insert("completedSlices", text(slice.index.to_string()));
        }
        _ => {
            vars.insert("hasSlices", Var::Flag(false));
            vars.insert("hasCompletedSlices", Var::Flag(false));
        }
    }
}

/// Hanvil: the `## Local Hedera network` section.
fn chain_vars(vars: &mut Vars, chain: Option<&ChainContext<'_>>) {
    match chain {
        Some(chain) => {
            vars.insert("hasLocalChain", Var::Flag(true));
            vars.insert("localChainId", text(chain.local.chain_id.to_string()));
            vars.insert("localRpcUrl", text(&chain.local.rpc_url));
            vars.insert("localMirrorUrl", text(&chain.local.mirror_url));
            vars.insert("localGrpcUrl", text(&chain.local.grpc_url));
            vars.insert("hasSigner", Var::Flag(chain.signer.is_some()));
            if let Some(signer) = chain.signer {
                vars.insert("signerAccountId", text(&signer.account_id));
                vars.insert("signerEvmAddress", text(&signer.evm_address));
            }
        }
        None => {
            vars.insert("hasLocalChain", Var::Flag(false));
            vars.insert("hasSigner", Var::Flag(false));
        }
    }
}

fn common_generator_vars(
    spec: &Spec,
    skills: &[VendoredSkill],
    context: Option<&VendoredContext>,
    slice: Option<Slice>,
    chain: Option<&ChainContext<'_>>,
) -> Result<Vars, Error> {
    let active = spec.slice(slice.map_or(0, |s| s.index));
    let (prd_path, eval_path, skills_root) = context_paths(skills, context);
    let has_eval = context
        .and_then(|c| c.eval_relative_path.as_ref())
        .is_some()
        || active.1.is_some();
    let mut vars = Vars::new();
    slice_vars(&mut vars, slice);
    chain_vars(&mut vars, chain);
    vars.insert("prdPath", text(prd_path));
    vars.insert("evalPath", text(eval_path));
    vars.insert("skillsRoot", text(skills_root));
    vars.insert("hasEval", Var::Flag(has_eval));
    vars.insert("hardConstraints", text(hard_constraints(spec)));
    vars.insert(
        "hasRequiredFiles",
        Var::Flag(!spec.required_files.is_empty()),
    );
    vars.insert("requiredFiles", text(bullet_list(&spec.required_files)));
    vars.insert("hasSkills", Var::Flag(!skills.is_empty()));
    vars.insert("skillSummaries", text(skill_summaries(skills)));
    Ok(vars)
}

/// `promptBuilder.ts:49-73`: the first attempt.
pub(crate) fn build_session_prompt(
    spec: &Spec,
    attempt: u64,
    skills: &[VendoredSkill],
    context: Option<&VendoredContext>,
    slice: Option<Slice>,
    chain: Option<&ChainContext<'_>>,
) -> Result<String, Error> {
    let active = spec.slice(slice.map_or(0, |s| s.index));
    let prd = std::fs::read_to_string(&active.0).map_err(|source| Error::Read {
        path: active.0.clone(),
        source,
    })?;
    let mut vars = common_generator_vars(spec, skills, context, slice, chain)?;
    vars.insert("attempt", text(attempt.to_string()));
    vars.insert("prd", text(prd.trim()));
    render(&spec.project_root, "generator", &vars)
}

/// `promptBuilder.ts:76-98`: a `--continue` cycle with a fresh-context agent.
pub(crate) fn build_continue_prompt(
    spec: &Spec,
    cycle: u64,
    skills: &[VendoredSkill],
    context: Option<&VendoredContext>,
    slice: Option<Slice>,
    chain: Option<&ChainContext<'_>>,
) -> Result<String, Error> {
    let mut vars = common_generator_vars(spec, skills, context, slice, chain)?;
    vars.insert("cycle", text(cycle.to_string()));
    render(&spec.project_root, "generator-continue", &vars)
}

/// `promptBuilder.ts:15`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepairScope {
    /// Only evaluate-checklist gaps; ASSERT and SMOKE already green.
    EvalScoped,
    /// Build, lint or gate failures.
    Runtime,
    /// Structural, static or mixed failures.
    Broad,
}

/// `promptBuilder.ts:168-193`.
pub(crate) fn classify_repair_scope(findings: &[Finding]) -> RepairScope {
    let categories: std::collections::BTreeSet<Category> = findings
        .iter()
        .filter(|f| f.category != Category::EvalInfra)
        .map(|f| f.category)
        .collect();
    if categories.is_empty() {
        return RepairScope::Broad;
    }
    if categories.iter().all(|c| *c == Category::Eval) {
        return RepairScope::EvalScoped;
    }
    let structural = categories.iter().any(|c| {
        matches!(
            c,
            Category::Files | Category::Static | Category::Secret | Category::Agent
        )
    });
    // Chain assertions are runtime-class: the app ran, the chain says it did the wrong thing.
    let runtime_only = categories.iter().all(|c| {
        matches!(
            c,
            Category::Commands | Category::Playwright | Category::Eval | Category::Chain
        )
    });
    if !structural
        && runtime_only
        && categories.iter().any(|c| {
            matches!(
                c,
                Category::Commands | Category::Playwright | Category::Chain
            )
        })
    {
        return RepairScope::Runtime;
    }
    RepairScope::Broad
}

/// `promptBuilder.ts:195-205`: `E7` from the finding's assertion, message or id.
pub(crate) fn extract_assertion_id(finding: &Finding) -> Option<String> {
    if let Some(assertion) = &finding.assertion {
        return Some(assertion.to_uppercase());
    }
    assertion_in(&finding.message).or_else(|| assertion_in(&finding.id))
}

/// `/\b(E\d+)\b/i`.
fn assertion_in(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let boundary_before = i == 0 || !is_word(bytes[i - 1]);
        if boundary_before && bytes[i].eq_ignore_ascii_case(&b'E') {
            let mut j = i + 1;
            while j < bytes.len() && bytes[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1 && (j == bytes.len() || !is_word(bytes[j])) {
                return Some(value[i..j].to_uppercase());
            }
        }
        i += 1;
    }
    None
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// `promptBuilder.ts:101-112` and `:120-166`: the preamble, then a repair prompt scoped by
/// the findings. `chain_reset` adds Hanvil's sentence about the reverted chain.
pub(crate) fn build_repair_prompt(
    spec: &Spec,
    findings: &[Finding],
    attempt: u64,
    context: Option<&VendoredContext>,
    chain_reset: bool,
) -> Result<String, Error> {
    let mut preamble_vars = Vars::new();
    preamble_vars.insert("chainReset", Var::Flag(chain_reset));
    let preamble = render(&spec.project_root, "repair-preamble", &preamble_vars)?;

    let actionable: Vec<&Finding> = findings
        .iter()
        .filter(|f| f.category != Category::EvalInfra)
        .collect();
    let actionable_owned: Vec<Finding> = actionable.iter().map(|f| (*f).clone()).collect();
    let scope = classify_repair_scope(&actionable_owned);
    let eval_path = context
        .and_then(|c| c.eval_relative_path.clone())
        .unwrap_or_else(|| format!("{LEGACY_CONTEXT_DIR}/eval.json"));
    let prd_path = context
        .map(|c| c.prd_relative_path.clone())
        .unwrap_or_else(|| format!("{LEGACY_CONTEXT_DIR}/prd.md"));
    let fallback = spec.slice(spec.prd_paths.len().saturating_sub(1));
    let eval_source = context
        .and_then(|c| c.eval_source_path.clone())
        .or(fallback.1.clone());
    let assertions = load_eval_assertions(eval_source.as_deref());

    let has_eval_findings = actionable.iter().any(|f| f.category == Category::Eval);
    let has_eval = context
        .and_then(|c| c.eval_relative_path.as_ref())
        .is_some()
        || fallback.1.is_some();
    let metadata = spec.template_metadata.as_ref();
    let has_metadata = metadata.is_some_and(|m| {
        m.name.is_some() || m.frontend.is_some() || m.solidity_framework.is_some()
    });

    let mut vars = Vars::new();
    vars.insert("attempt", text(attempt.to_string()));
    vars.insert("prdPath", text(prd_path));
    vars.insert("evalPath", text(eval_path));
    vars.insert("hardConstraints", text(hard_constraints(spec)));
    vars.insert("findingsList", text(findings_list(&actionable)));
    vars.insert("hasEvalFindings", Var::Flag(has_eval_findings));
    vars.insert("evalTargets", text(eval_targets(&actionable, &assertions)));
    vars.insert("hasEval", Var::Flag(has_eval));
    vars.insert("hasMetadata", Var::Flag(has_metadata));
    vars.insert("metadata", text(format_metadata(spec)));
    vars.insert(
        "hasRequiredFiles",
        Var::Flag(!spec.required_files.is_empty()),
    );
    vars.insert("requiredFiles", text(bullet_list(&spec.required_files)));

    let body = match scope {
        RepairScope::EvalScoped => render(&spec.project_root, "repair-eval", &vars)?,
        RepairScope::Runtime => {
            vars.insert("hasEvalChecklist", Var::Flag(has_eval_findings && has_eval));
            render(&spec.project_root, "repair-runtime", &vars)?
        }
        RepairScope::Broad => render(&spec.project_root, "repair-broad", &vars)?,
    };
    Ok(format!("{preamble}\n\n{body}"))
}

/// `promptBuilder.ts:207-249`.
pub(crate) fn build_validator_prompt(
    spec: &Spec,
    eval_json: &str,
    server_url: &str,
    signer: Option<&Signer>,
    browser_local_storage_key: &str,
    mirror_base_url: &str,
) -> Result<String, Error> {
    let output_schema = serde_json::json!({
        "passed": true,
        "summary": "Brief overall summary of the evaluation.",
        "issues": [{
            "id": "issue-slug",
            "assertion": "E1",
            "severity": "critical",
            "route": "/",
            "message": "What failed and why.",
            "evidence": "Route visited, elements observed, console output.",
        }],
    });
    let wallet_rule = match signer {
        Some(signer) => format!(
            "- Assertions flagged executableWithTestSigner=true MUST be executed end-to-end with the harness test signer and verified on the Hedera {} mirror node.\n- Other walletRequired assertions (without executableWithTestSigner) stay affordance-only: verify controls and no-wallet handling; do not require a completed on-chain tx.\n- Never use the test signer against mainnet.",
            signer.network
        ),
        None => "- For walletRequired assertions, do NOT complete on-chain transactions; verify affordances and no-wallet handling only.".to_string(),
    };
    let mut vars = Vars::new();
    vars.insert("serverUrl", text(server_url));
    vars.insert("eval", text(eval_json.trim()));
    vars.insert(
        "outputSchema",
        text(serde_json::to_string_pretty(&output_schema).unwrap_or_default()),
    );
    vars.insert("walletRule", text(wallet_rule));
    vars.insert("hasSigner", Var::Flag(signer.is_some()));
    vars.insert("browserKey", text(browser_local_storage_key));
    vars.insert("mirrorBaseUrl", text(mirror_base_url));
    if let Some(signer) = signer {
        vars.insert("signerAccountId", text(&signer.account_id));
        vars.insert("signerEvmAddress", text(&signer.evm_address));
        vars.insert("signerPrivateKey", text(&signer.private_key_hex));
        vars.insert("signerNetwork", text(&signer.network));
    }
    render(&spec.project_root, "validator", &vars)
}

/// `promptBuilder.ts:265-267`.
fn bullet_list(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("- {value}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// `promptBuilder.ts:269-280`.
fn format_metadata(spec: &Spec) -> String {
    let Some(metadata) = &spec.template_metadata else {
        return String::new();
    };
    [
        metadata
            .name
            .as_ref()
            .map(|name| format!("- template name: {name}")),
        metadata
            .frontend
            .as_ref()
            .map(|frontend| format!("- frontend capability: {frontend}")),
        metadata
            .solidity_framework
            .as_ref()
            .map(|framework| format!("- solidity framework capability: {framework}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n")
}

/// `promptBuilder.ts:282-292`.
fn findings_list(findings: &[&Finding]) -> String {
    if findings.is_empty() {
        return "- (no findings)".to_string();
    }
    findings
        .iter()
        .map(|finding| {
            let mut line = format!("- [{}] {}", finding.category.as_str(), finding.message);
            if let Some(details) = &finding.details {
                line.push_str(&format!("\n  {details}"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One checklist entry, keyed by upper-cased id.
struct EvalAssertion {
    route: Option<String>,
    severity: Option<String>,
    journey: Option<String>,
    statement: Option<String>,
    how_to_verify: Option<String>,
}

/// `promptBuilder.ts:324-345`: a missing or unreadable checklist leaves the map empty.
fn load_eval_assertions(eval_path: Option<&Path>) -> BTreeMap<String, EvalAssertion> {
    let mut map = BTreeMap::new();
    let Some(path) = eval_path else {
        return map;
    };
    let Some(parsed) = std::fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
    else {
        return map;
    };
    for assertion in parsed
        .get("assertions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = assertion.get("id").and_then(Value::as_str) else {
            continue;
        };
        let field = |key: &str| {
            assertion
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        map.insert(
            id.to_uppercase(),
            EvalAssertion {
                route: field("route"),
                severity: field("severity"),
                journey: field("journey"),
                statement: field("statement"),
                how_to_verify: field("howToVerify"),
            },
        );
    }
    map
}

/// `promptBuilder.ts:294-322`.
fn eval_targets(findings: &[&Finding], assertions: &BTreeMap<String, EvalAssertion>) -> String {
    let eval_findings: Vec<&&Finding> = findings
        .iter()
        .filter(|f| f.category == Category::Eval)
        .collect();
    if eval_findings.is_empty() {
        return "- (no evaluate findings)".to_string();
    }
    eval_findings
        .iter()
        .map(|finding| {
            let assertion_id = extract_assertion_id(finding);
            let from_checklist = assertion_id.as_ref().and_then(|id| assertions.get(id));
            let route = finding
                .route
                .clone()
                .or_else(|| from_checklist.and_then(|a| a.route.clone()));
            let mut lines = vec![format!(
                "### {}",
                assertion_id.clone().unwrap_or_else(|| finding.id.clone())
            )];
            if let Some(route) = route {
                lines.push(format!("- route: `{route}`"));
            }
            for (label, value) in [
                ("severity", from_checklist.and_then(|a| a.severity.as_ref())),
                ("journey", from_checklist.and_then(|a| a.journey.as_ref())),
                (
                    "statement",
                    from_checklist.and_then(|a| a.statement.as_ref()),
                ),
                (
                    "howToVerify",
                    from_checklist.and_then(|a| a.how_to_verify.as_ref()),
                ),
            ] {
                if let Some(value) = value {
                    lines.push(format!("- {label}: {value}"));
                }
            }
            lines.push(format!("- validator message: {}", finding.message));
            if let Some(details) = &finding.details {
                lines.push(format!("- evidence: {details}"));
            }
            lines.join("\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `promptBuilder.ts:347-363`.
fn hard_constraints(spec: &Spec) -> String {
    let mut lines = vec![
        "## Hard Constraints".to_string(),
        "- Keep all changes inside the current workspace.".to_string(),
        "- Use Yarn workspace commands only.".to_string(),
    ];
    if let Some(workspaces) = spec
        .constraints
        .forbidden_workspaces
        .as_ref()
        .filter(|w| !w.is_empty())
    {
        lines.push(format!("- Forbidden workspaces: {}", workspaces.join(", ")));
    }
    if !spec.constraints.forbidden_commands.is_empty() {
        lines.push(format!(
            "- Forbidden commands: {}",
            spec.constraints.forbidden_commands.join(", ")
        ));
    }
    lines.push(
        "- Do not add `.env` files, private keys, API keys, or live-network credential requirements."
            .to_string(),
    );
    lines.push(
        "- Produce `template.json`, `README.md`, and `AGENTS.md` suitable for scaffold-hbar."
            .to_string(),
    );
    lines.join("\n")
}

/// `promptBuilder.ts:365-374`.
fn skill_summaries(skills: &[VendoredSkill]) -> String {
    skills
        .iter()
        .map(|skill| {
            let refs = skill
                .references_path
                .as_ref()
                .map(|path| format!("\nReferences (read when needed): {path}/"))
                .unwrap_or_default();
            format!(
                "### {}\nSource: {}{refs}\n{}",
                skill.name, skill.relative_path, skill.description
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(entries: &[(&'static str, Var)]) -> Vars {
        entries.iter().cloned().collect()
    }

    #[test]
    fn the_renderer_handles_sections_variables_and_blank_lines() {
        let template = "Head\n{{#on}}\nkept {{name}}\n{{/on}}\n{{^on}}\ngone\n{{/on}}\n{{#outer}}\n{{#inner}}\nnested\n{{/inner}}\n{{/outer}}\n\n\n\nTail {{missing}}|{{flag}}|\r\n";
        let rendered = render_template(
            template,
            &vars(&[
                ("on", Var::Flag(true)),
                ("name", text("x")),
                ("outer", text("  ")),
                ("inner", Var::Flag(true)),
                ("flag", Var::Flag(true)),
            ]),
        );
        assert_eq!(rendered, "Head\nkept x\n\nTail ||");
        assert_eq!(
            render_template(
                "{{#a}}yes{{/a}}{{^a}}no{{/a}}",
                &vars(&[("a", Var::Flag(false))])
            ),
            "no"
        );
        assert_eq!(render_template("{{#a}}yes{{/a}}", &Vars::new()), "");
        assert_eq!(
            render_template("{topicId} stays", &Vars::new()),
            "{topicId} stays"
        );
    }

    #[test]
    fn assertion_ids_come_from_assertion_message_or_id() {
        let mut finding = Finding::new("eval:thing", Category::Eval, "e12 failed on /");
        assert_eq!(extract_assertion_id(&finding).as_deref(), Some("E12"));
        finding.message = "no id here, E123abc is not one".into();
        assert_eq!(extract_assertion_id(&finding), None);
        finding.id = "eval:E4".into();
        assert_eq!(extract_assertion_id(&finding).as_deref(), Some("E4"));
        finding.assertion = Some("e9".into());
        assert_eq!(extract_assertion_id(&finding).as_deref(), Some("E9"));
    }

    #[test]
    fn repair_scope_follows_the_upstream_table_plus_chain() {
        let f = |category: Category| Finding::new("x", category, "m");
        assert_eq!(classify_repair_scope(&[]), RepairScope::Broad);
        assert_eq!(
            classify_repair_scope(&[f(Category::EvalInfra)]),
            RepairScope::Broad
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Eval), f(Category::EvalInfra)]),
            RepairScope::EvalScoped
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Commands), f(Category::Eval)]),
            RepairScope::Runtime
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Playwright)]),
            RepairScope::Runtime
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Chain)]),
            RepairScope::Runtime
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Commands), f(Category::Files)]),
            RepairScope::Broad
        );
        assert_eq!(
            classify_repair_scope(&[f(Category::Agent)]),
            RepairScope::Broad
        );
    }

    fn spec_in(dir: &Path, extra: &str) -> Spec {
        let yaml = format!(
            "schemaVersion: 3\nname: t\nrequiredFiles: [README.md]\nbaseline:\n  commands:\n    - name: install\n      command: \"true\"\n{extra}"
        );
        crate::harness::spec::parse(&yaml, &dir.join(".harness/spec.yaml"))
            .expect("spec")
            .spec
    }

    #[test]
    fn the_generator_prompt_carries_the_prd_the_chain_and_the_constraints() {
        let dir = std::env::temp_dir().join(format!("hanvil-prompt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".harness")).expect("mkdir");
        std::fs::write(dir.join(".harness/prd.md"), "  Build a thing.  ").expect("prd");
        let spec = spec_in(
            &dir,
            "constraints:\n  packageManager: yarn\n  forbiddenWorkspaces: [packages/legacy]\n",
        );
        let local = LocalChain {
            rpc_url: "http://127.0.0.1:7546".into(),
            mirror_url: "http://127.0.0.1:5551".into(),
            grpc_url: "127.0.0.1:50211".into(),
            chain_id: 298,
        };
        let signer = Signer {
            account_id: "0.0.1032".into(),
            private_key_hex: "0xsecret".into(),
            evm_address: "0xaddr".into(),
            network: "local".into(),
            created_at: None,
        };
        let prompt = build_session_prompt(
            &spec,
            1,
            &[],
            None,
            None,
            Some(&ChainContext {
                local: &local,
                signer: Some(&signer),
            }),
        )
        .expect("prompt");
        assert!(
            prompt.starts_with(
                "You are the extension agent for an existing scaffold-hbar application."
            ),
            "{prompt}"
        );
        assert!(prompt.contains("Attempt: 1\n"), "{prompt}");
        assert!(!prompt.contains("Increment"), "{prompt}");
        assert!(
            prompt.contains("## Product Requirements (extension brief)\nBuild a thing.\n"),
            "{prompt}"
        );
        assert!(prompt.contains("## Local Hedera network\n"), "{prompt}");
        assert!(prompt.contains("chain id 298"), "{prompt}");
        assert!(prompt.contains("0.0.1032 (0xaddr)"), "{prompt}");
        assert!(
            !prompt.contains("0xsecret"),
            "the key travels in env, never in the prompt: {prompt}"
        );
        assert!(prompt.contains("- Forbidden workspaces: packages/legacy\n- Forbidden commands: npm install, npm run, pnpm install, pnpm run\n"), "{prompt}");
        assert!(
            prompt.contains("## Required Deliverables\n- README.md\n"),
            "{prompt}"
        );
        assert!(
            prompt.contains("- Use scaffold-hbar and Hedera best practices."),
            "{prompt}"
        );
        assert!(
            prompt.contains("The PRD is vendored at `.harness/runtime/context/prd.md`."),
            "{prompt}"
        );
        assert!(
            !prompt.contains("evaluate checklist is vendored"),
            "{prompt}"
        );

        let sliced = build_session_prompt(
            &spec,
            2,
            &[],
            None,
            Some(Slice { index: 1, count: 3 }),
            None,
        )
        .expect("prompt");
        assert!(sliced.contains("## Increment 2 of 3\n"), "{sliced}");
        assert!(
            sliced.contains("The first 1 increment(s) are already implemented"),
            "{sliced}"
        );
        assert!(!sliced.contains("Local Hedera network"), "{sliced}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn repair_prompts_are_scoped_and_carry_the_chain_reset_sentence() {
        let dir = std::env::temp_dir().join(format!("hanvil-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".harness")).expect("mkdir");
        std::fs::write(
            dir.join(".harness/eval.json"),
            r#"{"assertions":[{"id":"e1","route":"/","severity":"critical","statement":"loads","howToVerify":"open it"}]}"#,
        )
        .expect("eval");
        let spec = spec_in(
            &dir,
            "eval: .harness/eval.json\ntemplateMetadata:\n  name: demo\n",
        );
        let context = VendoredContext {
            prd_relative_path: ".harness/runtime/context/prd.md".into(),
            eval_relative_path: Some(".harness/runtime/context/eval.json".into()),
            prd_source_path: dir.join(".harness/prd.md"),
            eval_source_path: Some(dir.join(".harness/eval.json")),
        };
        let findings = vec![
            Finding::new(
                "command:lint",
                Category::Commands,
                "Validation command failed: lint",
            )
            .with_details("boom"),
            Finding::new("eval:x", Category::Eval, "critical [E1] (/): not loading"),
            Finding::new("validator-config", Category::EvalInfra, "ignored"),
        ];
        let prompt =
            build_repair_prompt(&spec, &findings, 2, Some(&context), true).expect("prompt");
        assert!(prompt.starts_with("You are repairing an in-place extension of an existing application.\nPreserve unrelated working features. Prefer the smallest fix that clears the findings.\nThe local chain was reset"), "{prompt}");
        assert!(prompt.contains("Repair scope: **runtime**"), "{prompt}");
        assert!(prompt.contains("- `.harness/runtime/context/eval.json` — only the failed assertion ids if listed below"), "{prompt}");
        assert!(prompt.contains("## Validation Findings\n- [commands] Validation command failed: lint\n  boom\n- [eval] critical [E1] (/): not loading\n"), "{prompt}");
        assert!(!prompt.contains("ignored"), "{prompt}");
        assert!(prompt.contains("### E1\n- route: `/`\n- severity: critical\n- statement: loads\n- howToVerify: open it\n- validator message: critical [E1] (/): not loading\n"), "{prompt}");
        assert!(
            prompt.contains("## Template Metadata Targets\n- template name: demo\n"),
            "{prompt}"
        );

        let eval_only =
            build_repair_prompt(&spec, &findings[1..2], 3, Some(&context), false).expect("prompt");
        assert!(
            eval_only.contains("Repair scope: **eval-scoped**"),
            "{eval_only}"
        );
        assert!(
            !eval_only.contains("The local chain was reset"),
            "{eval_only}"
        );
        let broad = build_repair_prompt(
            &spec,
            &[Finding::new("required-file:x", Category::Files, "missing")],
            2,
            None,
            false,
        )
        .expect("prompt");
        assert!(broad.contains("Repair scope: **broad**"), "{broad}");
        assert!(
            broad.contains("- `.harness-context/prd.md` — product requirements"),
            "{broad}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_validator_prompt_names_the_signer_and_the_mirror() {
        let dir = std::env::temp_dir().join(format!("hanvil-validator-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".harness")).expect("mkdir");
        let spec = spec_in(&dir, "");
        let signer = Signer {
            account_id: "0.0.1032".into(),
            private_key_hex: "0xkey".into(),
            evm_address: "0xaddr".into(),
            network: "local".into(),
            created_at: None,
        };
        let prompt = build_validator_prompt(
            &spec,
            " {\"assertions\":[]} ",
            "http://localhost:3000",
            Some(&signer),
            "burnerWallet.pk",
            "http://127.0.0.1:5551",
        )
        .expect("prompt");
        assert!(
            prompt.contains("Drive the running app at http://localhost:3000"),
            "{prompt}"
        );
        assert!(
            prompt.contains("## Test Signer (funded disposable account on local)"),
            "{prompt}"
        );
        assert!(prompt.contains("- Private key (hex): 0xkey"), "{prompt}");
        assert!(
            prompt.contains("- Base URL: http://127.0.0.1:5551"),
            "{prompt}"
        );
        assert!(
            prompt.contains("verified on the Hedera local mirror node."),
            "{prompt}"
        );
        assert!(
            prompt.contains(
                "\"passed\": true,\n  \"summary\": \"Brief overall summary of the evaluation.\""
            ),
            "{prompt}"
        );
        assert!(
            prompt.contains("GET /api/v1/topics/{topicId}"),
            "single braces survive: {prompt}"
        );
        let without =
            build_validator_prompt(&spec, "{}", "http://x", None, "k", "http://m").expect("prompt");
        assert!(!without.contains("Test Signer"), "{without}");
        assert!(
            without.contains("do NOT complete on-chain transactions"),
            "{without}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
