//! The harness: `hanvil run` and its siblings. A port of `hedera-harness` `dev` @ 587a2f3 that
//! drives a coding agent against the in-process chain. Module by module it follows the
//! TypeScript layout so a reader can diff them; `docs/code-plan.md` §16 has the contract.

use std::path::PathBuf;
use std::process::ExitCode;

use crate::cli;

pub(crate) mod agent;
pub(crate) mod artifacts;
pub(crate) mod assert;
pub(crate) mod attempt;
pub(crate) mod chain;
pub(crate) mod command;
pub(crate) mod devserver;
pub(crate) mod doctor;
pub(crate) mod env;
pub(crate) mod evaluate;
pub(crate) mod findings;
pub(crate) mod git;
pub(crate) mod init;
pub(crate) mod mcp;
pub(crate) mod prompt;
pub(crate) mod run;
pub(crate) mod session;
pub(crate) mod skills;
pub(crate) mod smoke;
pub(crate) mod spec;

/// `promptTemplates.ts:13-21`.
pub(crate) const PROMPT_TEMPLATE_NAMES: [&str; 7] = [
    "generator",
    "generator-continue",
    "repair-preamble",
    "repair-eval",
    "repair-runtime",
    "repair-broad",
    "validator",
];

/// `promptTemplates.ts:25`. Whole-file overrides of the bundled prompts.
pub(crate) const PROJECT_PROMPTS_DIR: &str = ".harness/prompts";

/// `cli.ts:8`.
pub(crate) const DEFAULT_SPEC_PATH: &str = ".harness/spec.yaml";

/// Run a harness subcommand. Exit status follows `cli.ts`: 0 on success, 1 when a report did
/// not pass or anything threw (`index.ts:22-26` prints `Error: <message>`).
pub(crate) async fn dispatch(command: cli::Command, node: cli::NodeArgs) -> ExitCode {
    match command {
        cli::Command::Init(args) => match init::run(init::InitOptions {
            target_dir: args.target_dir,
            repo: args.repo,
            ref_name: args.ref_name,
            template: args.template,
            skip_install: args.skip_install,
        })
        .await
        {
            Ok(result) => {
                println!("{}", init::format_result(&result));
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        },
        cli::Command::ValidateSemantic(args) => match run::validate_semantic(args, node).await {
            Ok(evaluation) => {
                // `cli.ts:132-150`.
                let mut lines = vec![
                    "EVALUATE finished".to_string(),
                    format!("passed={}", evaluation.passed),
                    format!("findings={}", evaluation.findings.len()),
                    format!("durationMs={}", evaluation.duration_ms),
                ];
                if evaluation.is_infrastructure_failure() {
                    lines.push(format!(
                        "infrastructureFailure=true reason={}",
                        evaluation
                            .infrastructure_failure_reason
                            .as_deref()
                            .unwrap_or("")
                    ));
                }
                if let Some(summary) = evaluation
                    .verdict
                    .as_ref()
                    .map(|v| v.summary.as_str())
                    .filter(|s| !s.is_empty())
                {
                    lines.push(format!("summary={summary}"));
                }
                lines.extend(
                    evaluation
                        .findings
                        .iter()
                        .map(|f| format!("- [{}] {}", f.category.as_str(), f.message)),
                );
                println!("{}", lines.join("\n"));
                if evaluation.passed {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        },
        cli::Command::Validate(args) => match run::validate(args, node).await {
            Ok(validation) => {
                // `cli.ts:109-127`.
                let mut lines = vec![
                    "Validation finished".to_string(),
                    format!("passed={}", validation.passed),
                    format!("findings={}", validation.findings.len()),
                ];
                if let Some(gate) = &validation.playwright_gate {
                    lines.push(format!(
                        "playwrightGate={} routes={}",
                        gate.passed,
                        gate.routes.len()
                    ));
                }
                lines.extend(
                    validation
                        .findings
                        .iter()
                        .map(|f| format!("- {}", f.message)),
                );
                lines.extend(validation.command_results.iter().map(|r| {
                    format!(
                        "command {} exit={} durationMs={}",
                        r.command,
                        r.exit_code
                            .map_or_else(|| "null".to_string(), |c| c.to_string()),
                        r.duration_ms
                    )
                }));
                println!("{}", lines.join("\n"));
                if validation.passed {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Err(error) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        },
        // D13: an interrupt drops the run future and then stops every process group the run
        // started; the run directory's status.json says `interrupted`.
        cli::Command::Run(args) => match tokio::select! {
            outcome = run::run(args, node) => Some(outcome),
            _ = tokio::signal::ctrl_c() => None,
        } {
            None => {
                println!(
                    "[hanvil] interrupted — stopping the agent, the dev server and the browser"
                );
                let stopped = command::kill_all_groups().await;
                println!("[hanvil] stopped {stopped} process group(s)");
                run::cleanup_after_interrupt().await;
                ExitCode::from(130)
            }
            Some(Ok(outcome)) => {
                for line in &outcome.outro {
                    println!("{line}");
                }
                if outcome.report.passed {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::FAILURE
                }
            }
            Some(Err(error)) => {
                eprintln!("Error: {error}");
                ExitCode::FAILURE
            }
        },
        cli::Command::Doctor(args) => {
            let options = doctor::Options {
                spec_path: args
                    .spec
                    .unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC_PATH)),
                workspace: args
                    .workspace
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from(".")),
                recipe_only: args.recipe_only,
                preflight: false,
            };
            let report = doctor::run(&options).await;
            println!("{}", doctor::format_report(&report));
            if report.passed {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
    }
}
