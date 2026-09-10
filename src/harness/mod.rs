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
pub(crate) mod chain;
pub(crate) mod command;
pub(crate) mod doctor;
pub(crate) mod env;
pub(crate) mod findings;
pub(crate) mod git;
pub(crate) mod prompt;
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
/// not pass.
pub(crate) async fn dispatch(command: cli::Command) -> ExitCode {
    match command {
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
