//! The harness: `hanvil run` and its siblings. A port of `hedera-harness` `dev` @ 587a2f3 that
//! drives a coding agent against the in-process chain. Module by module it follows the
//! TypeScript layout so a reader can diff them; `docs/code-plan.md` §16 has the contract.

// Removed with the commit that lands `hanvil run` (Fri 2026-09-11): until the attempt loop reads
// them, most recipe fields have no consumer. Tracked in plan.md §15.
#![allow(dead_code)]

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
pub(crate) mod mcp;
pub(crate) mod prompt;
pub(crate) mod run;
pub(crate) mod session;
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
        cli::Command::Validate(args) => match run::validate(args).await {
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
                run::note_interrupted();
                println!("[hanvil] stopped {stopped} process group(s)");
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
