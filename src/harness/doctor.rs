//! `hanvil doctor`: everything a run needs, reported at once rather than at the first failure.
//! `doctor.ts` and `preflight.ts` of hedera-harness dev @ 587a2f3; the check names, details and
//! fixes are theirs. Two differences: the browser checks arrive with the MCP client, and there
//! is no bundled-dependency check because nothing is bundled — the chain is this process.

use std::path::{Path, PathBuf};

use crate::harness::command;
use crate::harness::spec::{self, ChainNetwork, Spec};
use crate::harness::{PROJECT_PROMPTS_DIR, PROMPT_TEMPLATE_NAMES};

/// `doctor.ts:16`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Status {
    /// Passed.
    Ok,
    /// Passed with something worth saying. Never fails the report.
    Warn,
    /// Would stop `run` at preflight.
    Fail,
}

/// `doctor.ts:18-24`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Check {
    /// Label in the report.
    pub(crate) name: String,
    /// Verdict.
    pub(crate) status: Status,
    /// One line, doctor-facing.
    pub(crate) detail: String,
    /// Shown only when the check did not pass.
    pub(crate) fix: Option<String>,
}

impl Check {
    fn ok(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Ok,
            detail: detail.into(),
            fix: None,
        }
    }

    fn warn(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Warn,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }

    fn fail(name: impl Into<String>, detail: impl Into<String>, fix: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            detail: detail.into(),
            fix: Some(fix.into()),
        }
    }
}

/// `doctor.ts:26-29`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// In the order they ran.
    pub(crate) checks: Vec<Check>,
    /// False when any check failed outright; warnings do not count.
    pub(crate) passed: bool,
}

impl Report {
    fn from_checks(checks: Vec<Check>) -> Self {
        let passed = checks.iter().all(|check| check.status != Status::Fail);
        Self { checks, passed }
    }
}

/// What `doctor` was asked to look at.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    /// The recipe.
    pub(crate) spec_path: PathBuf,
    /// Where git and the tools are checked. Defaults to the current directory.
    pub(crate) workspace: PathBuf,
    /// `--recipe-only`: load the recipe and report nothing else. CI checks recipes across
    /// template branches without building each app.
    pub(crate) recipe_only: bool,
    /// Called from `run`'s preflight: the EVALUATE browser is probed, the SMOKE-only browser
    /// check is doctor's alone (`preflight.ts:84-93` vs `doctor.ts:270-282`).
    pub(crate) preflight: bool,
}

/// `doctor.ts:37-92`, in the same order: node, git, git-repo, recipe, agent, package-manager,
/// recipe files, eval, prompts, chain.
pub(crate) async fn run(options: &Options) -> Report {
    let (recipe_check, loaded) = load_recipe(&options.spec_path);
    if options.recipe_only {
        return Report::from_checks(vec![recipe_check]);
    }

    let workspace = options.workspace.as_path();
    let mut checks = Vec::new();
    let Some(loaded) = loaded else {
        // Still report host git basics when the recipe cannot load.
        checks.push(check_node_version(workspace).await);
        checks.push(check_git_on_path(workspace).await);
        checks.push(check_git_repo(workspace).await);
        checks.push(recipe_check);
        return Report::from_checks(checks);
    };
    let spec = &loaded.spec;

    if needs_node(spec) {
        checks.push(check_node_version(workspace).await);
    }
    checks.push(check_git_on_path(workspace).await);
    checks.push(check_git_repo(workspace).await);
    checks.push(recipe_check);
    checks.push(check_agent_cli(spec, workspace).await);
    if let Some(check) = check_package_manager(spec, workspace).await {
        checks.push(check);
    }
    checks.extend(check_recipe_files(spec));
    if spec.validator_enabled() && spec.eval_paths.is_none() {
        checks.push(missing_eval_config());
    }
    checks.push(check_prompt_overrides(&spec.project_root));
    if let Some(check) = check_browser(spec, workspace, options.preflight).await {
        checks.push(check);
    }
    if let Some(check) = check_chain(spec) {
        checks.push(check);
    }
    Report::from_checks(checks)
}

/// `doctor.ts:94-118`. Only the first line of a check carries its symbol, so every
/// continuation is indented to stay inside the report.
pub(crate) fn format_report(report: &Report) -> String {
    let indent = |text: &str| text.split('\n').collect::<Vec<_>>().join("\n      ");
    let mut lines = vec!["hanvil doctor".to_string(), String::new()];
    for check in &report.checks {
        let symbol = match check.status {
            Status::Ok => "✔",
            Status::Warn => "!",
            Status::Fail => "✘",
        };
        let mut line = format!("  {symbol} {} — {}", check.name, indent(&check.detail));
        if check.status != Status::Ok
            && let Some(fix) = &check.fix
        {
            line.push_str("\n      ");
            line.push_str(&indent(fix));
        }
        lines.push(line);
    }
    lines.push(String::new());
    let failed = report
        .checks
        .iter()
        .filter(|c| c.status == Status::Fail)
        .count();
    let warned = report
        .checks
        .iter()
        .filter(|c| c.status == Status::Warn)
        .count();
    lines.push(if !report.passed {
        format!("{failed} check(s) failed — `run` would not get past preflight.")
    } else if warned > 0 {
        format!("Ready to run ({warned} warning(s)).")
    } else {
        "Ready to run.".to_string()
    });
    lines.join("\n")
}

/// `doctor.ts:143-172`.
fn load_recipe(spec_path: &Path) -> (Check, Option<spec::Loaded>) {
    match spec::load(spec_path) {
        Ok(loaded) => {
            let check = if loaded.warnings.is_empty() {
                Check::ok(
                    "recipe",
                    format!(
                        "{} (schema v{})",
                        loaded.spec.spec_path.display(),
                        loaded.spec.schema_version
                    ),
                )
            } else {
                Check::warn(
                    "recipe",
                    format!(
                        "{} loads with {} warning(s)",
                        loaded.spec.spec_path.display(),
                        loaded.warnings.len()
                    ),
                    loaded.warnings.join("\n      "),
                )
            };
            (check, Some(loaded))
        }
        Err(error) => (
            Check::fail(
                "recipe",
                error.to_string(),
                "Fix the recipe, or bootstrap one with `hanvil init`.",
            ),
            None,
        ),
    }
}

/// Node is the app's runtime, not the harness's. It is checked when a stage needs a browser
/// server or when the project is a Node project; a Solidity-only recipe on a machine without
/// Node is fine. Upstream always checks.
fn needs_node(spec: &Spec) -> bool {
    spec.validators.playwright_path.is_some()
        || spec.validator_enabled()
        || spec.constraints.package_manager.is_some()
        || spec.project_root.join("package.json").exists()
}

/// `preflight.ts:120-139`.
async fn check_node_version(cwd: &Path) -> Check {
    let version = match command::capture("node", &["--version"], cwd).await {
        Ok(captured) if captured.ok => captured.stdout.trim().to_string(),
        _ => {
            return Check::fail(
                "node",
                "not on PATH",
                "The harness requires Node.js 20 or newer.",
            );
        }
    };
    let major: u32 = version
        .trim_start_matches('v')
        .split('.')
        .next()
        .and_then(|major| major.parse().ok())
        .unwrap_or(0);
    if major >= 20 {
        Check::ok("node", version)
    } else {
        Check::fail(
            "node",
            format!("{version} is too old"),
            "The harness requires Node.js 20 or newer.",
        )
    }
}

/// `preflight.ts:141-154`.
async fn check_git_on_path(cwd: &Path) -> Check {
    if command::exists("git", cwd).await {
        Check::ok("git", "on PATH")
    } else {
        Check::fail(
            "git",
            "not on PATH",
            "git is required for branch and checkpoint handling.",
        )
    }
}

/// `preflight.ts:156-210`, reading the repository the way `harnessGit.ts` does.
async fn check_git_repo(cwd: &Path) -> Check {
    let fix = "Check out a branch — the harness records its work on one.";
    let toplevel = command::capture("git", &["rev-parse", "--show-toplevel"], cwd).await;
    let root = match toplevel {
        Ok(captured) if captured.ok && !captured.stdout.trim().is_empty() => {
            PathBuf::from(captured.stdout.trim())
        }
        _ => {
            return Check::fail(
                "git repo",
                format!("Not a git repository: {}", cwd.display()),
                "Run from inside a git repository (`hanvil init` creates one).",
            );
        }
    };
    let detached = !command::capture("git", &["symbolic-ref", "-q", "HEAD"], &root)
        .await
        .is_ok_and(|captured| captured.ok);
    if detached {
        return Check::fail("git repo", "HEAD is detached", fix);
    }
    if let Some(operation) = in_progress_operation(&root).await {
        return Check::fail(
            "git repo",
            format!("a {operation} is in progress"),
            "Finish or abort it first.",
        );
    }
    match command::capture("git", &["branch", "--show-current"], &root).await {
        Ok(captured) if captured.ok && !captured.stdout.trim().is_empty() => {
            Check::ok("git repo", format!("on {}", captured.stdout.trim()))
        }
        _ => Check::fail(
            "git repo",
            "Unable to determine the current git branch.",
            fix,
        ),
    }
}

/// `harnessGit.ts` `detectInProgressGitOperation`: marker files under the git dir.
async fn in_progress_operation(root: &Path) -> Option<&'static str> {
    let git_dir = command::capture("git", &["rev-parse", "--git-dir"], root)
        .await
        .ok()
        .filter(|captured| captured.ok)
        .map(|captured| root.join(captured.stdout.trim()))?;
    const MARKERS: [(&str, &str); 7] = [
        ("MERGE_HEAD", "merge"),
        ("REBASE_HEAD", "rebase"),
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
        ("BISECT_LOG", "bisect"),
    ];
    MARKERS
        .iter()
        .find(|(marker, _)| git_dir.join(marker).exists())
        .map(|(_, label)| *label)
}

/// `preflight.ts:212-244`.
async fn check_agent_cli(spec: &Spec, cwd: &Path) -> Check {
    let command = spec.generator.command.trim();
    let command = if command.is_empty() {
        spec.agent.command()
    } else {
        command
    };
    let name = format!("agent ({})", spec.agent.name());
    // Absolute paths and npx-style wrappers are not resolvable via PATH.
    if command.contains('/') || command.contains('\\') {
        return Check::ok(name, format!("{command} (not checked)"));
    }
    if command::exists(command, cwd).await {
        return Check::ok(name, format!("{command} on PATH"));
    }
    Check::fail(
        name,
        format!("{command} is not on PATH"),
        format!(
            "Install and authenticate the {} CLI, or set a different `agent:` in the recipe.",
            spec.agent.name()
        ),
    )
}

/// `preflight.ts:246-277`. `None` when the project is not a Node project and declares no
/// package manager — upstream would demand `npm` regardless.
async fn check_package_manager(spec: &Spec, cwd: &Path) -> Option<Check> {
    let declared = spec
        .constraints
        .package_manager
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let binary = match declared {
        Some(declared) => declared
            .split('@')
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(declared)
            .to_string(),
        None => {
            if !spec.project_root.join("package.json").exists() {
                return None;
            }
            package_install_tool(&spec.project_root)
        }
    };
    if command::exists(&binary, cwd).await {
        return Some(Check::ok("package manager", format!("{binary} on PATH")));
    }
    Some(Check::fail(
        "package manager",
        format!("{binary} is not on PATH"),
        match declared {
            Some(declared) => {
                format!("The recipe declares constraints.packageManager: {declared}.")
            }
            None => "Detected from the project's lockfile.".to_string(),
        },
    ))
}

/// `optionalDeps.ts` `resolvePackageInstallTool`: `package.json#packageManager`, then lockfiles,
/// then npm.
fn package_install_tool(project_root: &Path) -> String {
    let from_field = std::fs::read_to_string(project_root.join("package.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|pkg| {
            pkg.get("packageManager")
                .and_then(serde_json::Value::as_str)
                .and_then(tool_from_package_manager_field)
        });
    if let Some(tool) = from_field {
        return tool.to_string();
    }
    for (lockfile, tool) in [
        ("yarn.lock", "yarn"),
        ("pnpm-lock.yaml", "pnpm"),
        ("package-lock.json", "npm"),
    ] {
        if project_root.join(lockfile).exists() {
            return tool.to_string();
        }
    }
    "npm".to_string()
}

fn tool_from_package_manager_field(field: &str) -> Option<&'static str> {
    let lowered = field.trim().to_lowercase();
    ["yarn", "pnpm", "npm"]
        .into_iter()
        .find(|tool| lowered.starts_with(tool))
}

/// `preflight.ts:279-319`.
fn check_recipe_files(spec: &Spec) -> Vec<Check> {
    let mut targets: Vec<(String, &Path)> = Vec::new();
    for (i, prd) in spec.prd_paths.iter().enumerate() {
        let label = if spec.prd_paths.len() > 1 {
            format!("prd[{i}]")
        } else {
            "prd".to_string()
        };
        targets.push((label, prd));
    }
    targets.push((
        "validators.static".to_string(),
        &spec.validators.static_path,
    ));
    targets.push((
        "validators.commands".to_string(),
        &spec.validators.commands_path,
    ));
    if let Some(playwright) = &spec.validators.playwright_path {
        targets.push(("validators.playwright".to_string(), playwright));
    }
    if let Some(evals) = &spec.eval_paths {
        for (i, eval) in evals.iter().enumerate() {
            let label = if evals.len() > 1 {
                format!("eval[{i}]")
            } else {
                "eval".to_string()
            };
            targets.push((label, eval));
        }
    }
    targets
        .into_iter()
        .map(|(label, target)| {
            if target.exists() {
                Check::ok(label, "present")
            } else {
                Check::fail(
                    label,
                    format!("missing: {}", target.display()),
                    "The recipe points at a file that does not exist.",
                )
            }
        })
        .collect()
}

/// `preflight.ts:328-342`.
fn missing_eval_config() -> Check {
    Check::fail(
        "eval",
        "`validator.enabled` is set but `eval` is not",
        "Add `eval: .harness/eval.json`, or remove `validator.enabled` to turn EVALUATE off.",
    )
}

/// `doctor.ts:236-254`. An override is a copy and does not receive later prompt changes.
fn check_prompt_overrides(project_root: &Path) -> Check {
    let overridden: Vec<&str> = PROMPT_TEMPLATE_NAMES
        .iter()
        .copied()
        .filter(|name| {
            project_root
                .join(PROJECT_PROMPTS_DIR)
                .join(format!("{name}.md"))
                .exists()
        })
        .collect();
    if overridden.is_empty() {
        return Check::ok("prompts", "using bundled prompts");
    }
    Check::warn(
        "prompts",
        format!(
            "{} override(s): {}",
            overridden.len(),
            overridden.join(", ")
        ),
        format!(
            "Overrides in {PROJECT_PROMPTS_DIR}/ do not track harness updates — re-check them after upgrading."
        ),
    )
}

/// `preflight.ts:344-384` and `doctor.ts:256-282`: start the MCP server and actually navigate,
/// because every cheaper check has lied. EVALUATE's probe runs for doctor and for `run`; the
/// SMOKE-only browser check is doctor's alone.
async fn check_browser(spec: &Spec, workspace: &Path, preflight: bool) -> Option<Check> {
    spec.validators.playwright_path.as_ref()?;
    let evaluate = spec.validator_enabled() && spec.eval_paths.is_some();
    if !evaluate && preflight {
        return None;
    }
    let name = if evaluate {
        "EVALUATE browser (Playwright MCP)"
    } else {
        "SMOKE browser"
    };
    let probe = crate::harness::mcp::probe(workspace).await;
    if probe.ok {
        return Some(Check::ok(name, probe.choice.detail()));
    }
    let repair = probe.choice.repair();
    let fix = match (evaluate, probe.choice.source()) {
        (true, "project-playwright") => format!("Reinstall Chromium: {repair}"),
        (true, _) => {
            format!("Install system Chrome so SMOKE and EVALUATE share one browser: {repair}")
        }
        (false, "project-playwright") => format!("Reinstall Chromium: {repair}"),
        (false, _) => format!("Install system Chrome, or install Chromium: {repair}"),
    };
    Some(Check::fail(
        name,
        probe
            .error
            .unwrap_or_else(|| "the Playwright MCP browser could not be launched".to_string()),
        fix,
    ))
}

/// Replaces `doctor.ts` `checkChainEnv`. There are no credentials to check: on `local` the
/// chain is this process, so the only thing that can be wrong is a recipe pointing at another
/// machine; `testnet` is what hedera-harness is for.
fn check_chain(spec: &Spec) -> Option<Check> {
    let chain = spec.chain_validation.as_ref()?;
    match chain.network {
        ChainNetwork::Testnet => Some(Check::fail(
            "chain",
            "network: testnet is not supported by hanvil run",
            "Use network: local, or run this recipe with hedera-harness.",
        )),
        ChainNetwork::Local => {
            let local = chain.local.as_ref()?;
            let urls = [&local.rpc_url, &local.mirror_url, &local.grpc_url];
            if let Some(remote) = urls.iter().find(|url| !is_loopback(url)) {
                return Some(Check::fail(
                    "chain",
                    format!("chainValidation.local names {remote}, which is not this machine"),
                    "hanvil run boots the node in-process; point chainValidation.local at localhost or drop it.",
                ));
            }
            Some(Check::ok(
                "chain",
                format!(
                    "in-process hanvil at {}, {}, {}",
                    local.rpc_url, local.mirror_url, local.grpc_url
                ),
            ))
        }
    }
}

/// The host of `http://host:port/...` or `host:port` is a loopback name.
fn is_loopback(url: &str) -> bool {
    let without_scheme = url.split("://").nth(1).unwrap_or(url);
    let host = without_scheme
        .split(['/', ':'])
        .next()
        .unwrap_or("")
        .trim_matches(['[', ']']);
    matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_report_renders_symbols_indentation_and_the_footer() {
        let report = Report::from_checks(vec![
            Check::ok("git", "on PATH"),
            Check::warn("prompts", "1 override(s): generator", "line one\nline two"),
            Check::fail(
                "prd",
                "missing: /x/prd.md",
                "The recipe points at a file that does not exist.",
            ),
        ]);
        assert!(!report.passed);
        assert_eq!(
            format_report(&report),
            "hanvil doctor\n\n  ✔ git — on PATH\n  ! prompts — 1 override(s): generator\n      line one\n      line two\n  ✘ prd — missing: /x/prd.md\n      The recipe points at a file that does not exist.\n\n1 check(s) failed — `run` would not get past preflight."
        );
        let warned = Report::from_checks(vec![Check::warn("prompts", "x", "y")]);
        assert!(warned.passed);
        assert!(format_report(&warned).ends_with("Ready to run (1 warning(s))."));
        let clean = Report::from_checks(vec![Check::ok("git", "on PATH")]);
        assert!(format_report(&clean).ends_with("Ready to run."));
    }

    #[test]
    fn loopback_detection_reads_hosts_out_of_urls() {
        assert!(is_loopback("http://localhost:7546"));
        assert!(is_loopback("localhost:50211"));
        assert!(is_loopback("http://127.0.0.1:5551/api/v1"));
        assert!(!is_loopback("http://node.example:7546"));
        assert!(!is_loopback("10.0.0.5:50211"));
    }

    #[test]
    fn the_package_install_tool_follows_upstream_precedence() {
        assert_eq!(tool_from_package_manager_field("yarn@4.1.0"), Some("yarn"));
        assert_eq!(tool_from_package_manager_field("PNPM@9"), Some("pnpm"));
        assert_eq!(tool_from_package_manager_field("bun@1"), None);
    }
}
