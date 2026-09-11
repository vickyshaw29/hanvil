//! One increment, attempt by attempt. `attemptLoop.ts`, `attemptStages.ts` and
//! `attemptReporting.ts` of hedera-harness dev @ 587a2f3, with CHAIN as a stage of its own:
//!
//! ```text
//! GENERATE -> ASSERT -> CHAIN -> SMOKE -> EVALUATE
//! ```
//!
//! Each stage may short-circuit the rest; ASSERT is cheap and deterministic, so a failing build
//! never pays for a chain deploy, a dev server or an evaluator. Before GENERATE the chain is
//! snapshotted; after a failed attempt with budget left it is reverted, so the repair starts on
//! the chain the attempt started on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::harness::agent::{self, Progress, Provider, RunInput};
use crate::harness::artifacts::{
    self, Layout, LogEvent, now_iso8601, write_json_file, write_prompt_file,
};
use crate::harness::assert;
use crate::harness::chain::{self, LocalChain, Signer};
use crate::harness::command::{self, Execute};
use crate::harness::devserver;
use crate::harness::env;
use crate::harness::evaluate::{self, EvaluationInput};
use crate::harness::findings::{
    self, Category, Delta, Finding, ValidationResult, apply_status, compute_delta, format_delta,
    truncate_details,
};
use crate::harness::git;
use crate::harness::ledger::{self, Ledger};
use crate::harness::mcp;
use crate::harness::prompt::{self, ChainContext, Slice, VendoredContext, VendoredSkill};
use crate::harness::session::{self, log_phase};
use crate::harness::smoke;
use crate::harness::spec::McpDelivery;
use crate::harness::spec::Spec;
use crate::serve::Shared;

/// `attemptStages.ts:38`, plus CHAIN.
const STAGE_NAMES: [&str; 5] = ["GENERATE", "ASSERT", "CHAIN", "SMOKE", "EVALUATE"];

/// `attemptStages.ts:57-61`.
pub(crate) fn log_stage(stage: &str, detail: Option<&str>) {
    let index = STAGE_NAMES
        .iter()
        .position(|name| *name == stage)
        .map_or(0, |i| i + 1);
    match detail {
        Some(detail) => println!(
            "[hanvil] Stage {index}/{} {stage} — {detail}",
            STAGE_NAMES.len()
        ),
        None => println!("[hanvil] Stage {index}/{} {stage}", STAGE_NAMES.len()),
    }
}

/// What stops the loop, as opposed to a failed attempt.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A run file could not be written.
    #[error(transparent)]
    Artifacts(#[from] artifacts::Error),
    /// A prompt could not be built.
    #[error(transparent)]
    Prompt(#[from] prompt::Error),
    /// The agent could not be configured or started.
    #[error(transparent)]
    Agent(#[from] agent::Error),
    /// ASSERT could not run.
    #[error(transparent)]
    Assert(#[from] assert::Error),
    /// A checkpoint could not be made.
    #[error(transparent)]
    Git(#[from] git::Error),
    /// `session.json` could not be updated.
    #[error(transparent)]
    Session(#[from] session::Error),
    /// A chain deploy command could not be spawned.
    #[error("running chain deploy command {name}: {source}")]
    Spawn {
        /// `deploy.commands[].name`.
        name: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The SMOKE gate config.
    #[error(transparent)]
    Smoke(#[from] smoke::Error),
    /// The dev server.
    #[error(transparent)]
    DevServer(#[from] devserver::Error),
    /// The browser MCP config or workspace file.
    #[error(transparent)]
    Mcp(#[from] mcp::Error),
}

/// `types.ts:301-312`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SliceReport {
    /// Zero-based position in `prd:`.
    pub(crate) index: usize,
    /// The PRD delivered.
    pub(crate) prd_path: PathBuf,
    /// Its eval checklist, when EVALUATE is configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) eval_path: Option<PathBuf>,
    /// Delivered.
    pub(crate) passed: bool,
    /// Attempts consumed by this increment alone.
    pub(crate) attempts: u64,
    /// Ids still failing.
    pub(crate) open_finding_ids: Vec<String>,
}

/// `types.ts:314-338`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RunReport {
    /// `spec.name`.
    pub(crate) spec_name: String,
    /// The recipe.
    pub(crate) spec_path: PathBuf,
    /// `.harness/runs/<id>`.
    pub(crate) run_directory: PathBuf,
    /// The project.
    pub(crate) workspace_path: PathBuf,
    /// Attempts in the project so far.
    pub(crate) attempts: u64,
    /// Budget per increment.
    pub(crate) max_attempts: u64,
    /// Set for a `--continue` cycle, 1-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) cycle: Option<u64>,
    /// Attempts consumed in this kick only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attempts_this_cycle: Option<u64>,
    /// Every gate that ran is clean.
    pub(crate) passed: bool,
    /// Ids still failing when the run stopped.
    pub(crate) open_finding_ids: Vec<String>,
    /// Ids the final attempt closed.
    pub(crate) fixed_finding_ids: Vec<String>,
    /// One per increment attempted this kick.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slices: Option<Vec<SliceReport>>,
    /// ISO 8601.
    pub(crate) started_at: String,
    /// ISO 8601.
    pub(crate) finished_at: String,
    /// Wall time.
    pub(crate) duration_ms: u64,
    /// The last attempt's validation.
    pub(crate) validation: ValidationResult,
    /// Its evaluation, when EVALUATE ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) evaluation: Option<findings::Evaluation>,
    /// Hanvil: what the final attempt did on the chain, in four numbers. Absent when CHAIN did
    /// not run. The rows themselves are `logs/chain-ledger-attempt-N.json`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) chain_ledger: Option<ledger::Summary>,
}

/// The node under the run, as the loop sees it.
pub(crate) struct ChainHandle {
    /// The one chain.
    pub(crate) shared: Shared,
    /// Endpoints for prompts and env.
    pub(crate) local: LocalChain,
}

/// `attemptLoop.ts:70-97`.
pub(crate) struct LoopInput<'a> {
    /// Where to write.
    pub(crate) layout: &'a Layout,
    /// The recipe.
    pub(crate) spec: &'a Spec,
    /// Attempt budget for this increment.
    pub(crate) max_attempts: u64,
    /// This kick is a `--continue`.
    pub(crate) is_continue: bool,
    /// The continue cycle, when it is one.
    pub(crate) cycle: Option<u64>,
    /// Attempt number to start from.
    pub(crate) starting_attempt: u64,
    /// When the kick began, for the report.
    pub(crate) started: Instant,
    /// ISO 8601 of the same instant.
    pub(crate) started_at: String,
    /// The project.
    pub(crate) workspace: &'a Path,
    /// Vendored skills for the prompt.
    pub(crate) skills: &'a [VendoredSkill],
    /// Vendored PRD and eval for this increment.
    pub(crate) context: &'a VendoredContext,
    /// The funded signer, when CHAIN is on.
    pub(crate) signer: Option<&'a Signer>,
    /// Position in `prd:`.
    pub(crate) slice: Slice,
    /// Ids open when the previous cycle stopped.
    pub(crate) previous_open_finding_ids: Vec<String>,
    /// The node.
    pub(crate) chain: &'a ChainHandle,
}

/// `attemptReporting.ts:21-27`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AttemptKind {
    Generate,
    Continue,
    Repair,
}

/// `attemptLoop.ts:118-250`.
pub(crate) async fn run_loop(input: LoopInput<'_>) -> Result<RunReport, Error> {
    let LoopInput {
        layout,
        spec,
        max_attempts,
        is_continue,
        cycle,
        workspace,
        skills,
        context,
        signer,
        slice,
        chain,
        ..
    } = input;
    let agent_timeout = env::agent_timeout();
    let chain_context = ChainContext {
        local: &chain.local,
        signer,
    };

    let mut attempts = input.starting_attempt.saturating_sub(1);
    let mut attempts_this_cycle = 0;
    let mut validation = ValidationResult::not_run_yet();
    // Refusals this attempt made that the one before it made too; set after each validation.
    let mut repeated;
    let mut latest_prompt = if is_continue {
        prompt::build_continue_prompt(
            spec,
            cycle.unwrap_or(1),
            skills,
            Some(context),
            Some(slice),
            Some(&chain_context),
        )?
    } else {
        prompt::build_session_prompt(
            spec,
            1,
            skills,
            Some(context),
            Some(slice),
            Some(&chain_context),
        )?
    };
    let mut open_finding_ids = input.previous_open_finding_ids.clone();
    let mut previous_findings: Vec<Finding> = Vec::new();
    let mut delta = Delta {
        open: open_finding_ids.clone(),
        ..Delta::default()
    };
    let snapshot_per_attempt = spec
        .chain_validation
        .as_ref()
        .is_some_and(|c| c.snapshot_per_attempt);

    while attempts_this_cycle < max_attempts {
        attempts += 1;
        attempts_this_cycle += 1;

        let kind = attempt_kind(is_continue, attempts, attempts_this_cycle);
        let choice = agent::select_model(
            spec.agent,
            attempts_this_cycle == 1,
            delta.fixed.len() as u64,
            attempts_this_cycle > 1,
        );
        announce_attempt(
            layout,
            kind,
            attempts,
            cycle,
            attempts_this_cycle,
            &latest_prompt,
            &choice,
        )?;

        // Snapshot before the agent runs, so a failed attempt can be undone under the repair.
        let (snapshot_id, mark, snapshot_micros) = {
            let mut guard = chain.shared.write();
            let mark = chain::Mark::of(&guard);
            let started = Instant::now();
            let id = snapshot_per_attempt.then(|| guard.snapshot());
            (id, mark, micros_since(started))
        };
        if let Some(id) = snapshot_id {
            layout.append_log(&LogEvent::ChainSnapshotTaken {
                attempt: attempts,
                snapshot_id: chain::snapshot_id(id),
                duration_micros: snapshot_micros,
            })?;
            log_phase(
                "Chain snapshot taken",
                Some(&format!(
                    "attempt {attempts} — {} — {snapshot_micros} µs",
                    chain::snapshot_id(id)
                )),
            );
        }

        let mut generator = spec.generator.clone();
        if let Some(timeout) = agent_timeout {
            generator.timeout_ms = Some(timeout.as_millis() as u64);
        }
        let generator = agent::with_model(&generator, spec.agent.model_flag(), &choice.model);
        let generate = run_generate_stage(
            layout,
            spec,
            workspace,
            attempts,
            Provider::new(generator, spec.agent)?,
            &latest_prompt,
            &chain.local,
            signer,
        )
        .await?;

        let previous_ledger = validation.chain_ledger.take();
        validation = run_validation_stages(
            layout, spec, workspace, attempts, chain, signer, &mark, context, generate,
        )
        .await?;

        // A repair that fixes the symptom and leaves the cause sends the same refused
        // transaction again. Nothing else in the run would say so.
        repeated = match (&validation.chain_ledger, &previous_ledger) {
            (Some(now), Some(before)) => now.repeated_from(before),
            _ => None,
        };
        if let Some(repeated) = &repeated {
            log_phase(
                "Chain refusals repeated",
                Some(&format!(
                    "{repeated} — the last attempt was refused these too"
                )),
            );
        }

        delta = compute_delta(&open_finding_ids, &validation.findings);
        validation.findings = apply_status(
            std::mem::take(&mut validation.findings),
            &delta,
            &previous_findings,
        );
        previous_findings = validation.findings.clone();
        open_finding_ids = delta.open.clone();

        record_attempt_result(layout, attempts, &validation, &delta)?;
        write_state_dump(layout, chain, attempts)?;

        let infra_abort = validation
            .evaluation
            .as_ref()
            .is_some_and(findings::Evaluation::is_infrastructure_failure);
        if infra_abort {
            abort_on_infrastructure_failure(layout, attempts, &validation)?;
            checkpoint(layout, workspace, attempts, &validation).await?;
            break;
        }

        layout.append_note(
            &format!("Attempt {attempts} validation"),
            &if validation.passed {
                if validation.evaluation.is_some() {
                    "Deterministic, Playwright gate, and evaluate checklist passed.".to_string()
                } else {
                    "Deterministic validation passed.".to_string()
                }
            } else {
                let mut lines = vec![format_delta(&delta)];
                lines.extend(
                    validation
                        .findings
                        .iter()
                        .filter(|f| f.is_open())
                        .map(|f| format!("- [{}] {}", f.category.as_str(), f.message)),
                );
                lines.join("\n")
            },
        )?;

        checkpoint(layout, workspace, attempts, &validation).await?;

        if validation.passed {
            break;
        }

        if attempts_this_cycle < max_attempts {
            let reverted = match snapshot_id {
                Some(id) => {
                    let started = Instant::now();
                    let success = chain.shared.write().revert(id);
                    let duration_micros = micros_since(started);
                    layout.append_log(&LogEvent::ChainSnapshotReverted {
                        attempt: attempts,
                        snapshot_id: chain::snapshot_id(id),
                        success,
                        duration_micros,
                    })?;
                    log_phase(
                        if success {
                            "Chain reverted"
                        } else {
                            "Chain revert refused"
                        },
                        Some(&format!(
                            "attempt {} starts on the state attempt {attempts} started on — {duration_micros} µs",
                            attempts + 1
                        )),
                    );
                    success
                }
                None => false,
            };
            let open: Vec<Finding> = validation
                .findings
                .iter()
                .filter(|f| f.is_open())
                .cloned()
                .collect();
            latest_prompt = prompt::build_repair_prompt(
                spec,
                &open,
                attempts + 1,
                Some(context),
                reverted,
                validation.chain_ledger.as_ref(),
                repeated.as_deref(),
            )?;
        }
    }

    finish_run(FinishInput {
        layout,
        spec,
        workspace,
        attempts,
        attempts_this_cycle,
        max_attempts,
        is_continue,
        cycle,
        started: input.started,
        started_at: input.started_at,
        validation,
        delta,
    })
}

/// `attemptReporting.ts:23-27`.
fn attempt_kind(is_continue: bool, attempt: u64, attempts_this_cycle: u64) -> AttemptKind {
    if !is_continue && attempt == 1 {
        AttemptKind::Generate
    } else if is_continue && attempts_this_cycle == 1 {
        AttemptKind::Continue
    } else {
        AttemptKind::Repair
    }
}

/// `attemptReporting.ts:29-85`.
fn announce_attempt(
    layout: &Layout,
    kind: AttemptKind,
    attempt: u64,
    cycle: Option<u64>,
    attempts_this_cycle: u64,
    prompt: &str,
    model: &agent::ModelChoice,
) -> Result<(), Error> {
    let cycle_number = cycle.unwrap_or(0);
    let file_name = match kind {
        AttemptKind::Generate => format!("generator-attempt-{attempt}.txt"),
        AttemptKind::Continue => format!("continue-cycle-{cycle_number}-attempt-{attempt}.txt"),
        AttemptKind::Repair => format!("repair-attempt-{attempt}.txt"),
    };
    let prompt_path = layout.prompts_directory.join(file_name);
    write_prompt_file(&prompt_path, prompt, &[])?;
    layout.append_log(&match kind {
        AttemptKind::Continue => LogEvent::ContinueStarted {
            attempt,
            cycle: cycle_number,
            prompt_path: prompt_path.clone(),
        },
        AttemptKind::Generate => LogEvent::GeneratorStarted {
            attempt,
            prompt_path: prompt_path.clone(),
        },
        AttemptKind::Repair => LogEvent::RepairStarted {
            attempt,
            prompt_path: prompt_path.clone(),
        },
    })?;
    layout.write_status(json!({
        "phase": if kind == AttemptKind::Repair { "repair_running" } else { "generator_running" },
        "stage": "GENERATE",
        "attempt": attempt,
        "cycle": cycle,
        "attemptsThisCycle": attempts_this_cycle,
        "promptPath": prompt_path,
    }))?;
    let model_note = format!(
        " [{}{}]",
        model.model,
        if model.reason == "escalated" {
            ", escalated — last attempt fixed nothing"
        } else {
            ""
        }
    );
    let detail = match kind {
        AttemptKind::Continue => format!("continue cycle {cycle_number}, attempt {attempt}"),
        AttemptKind::Repair => format!("repair, attempt {attempt}"),
        AttemptKind::Generate => format!("attempt {attempt}"),
    };
    log_stage("GENERATE", Some(&format!("{detail}{model_note}")));
    Ok(())
}

/// `attemptStages.ts:69-173`: a non-zero exit becomes a finding rather than an error, so the
/// attempt still runs ASSERT and reports deterministic context alongside the agent failure.
#[allow(clippy::too_many_arguments)]
async fn run_generate_stage(
    layout: &Layout,
    spec: &Spec,
    workspace: &Path,
    attempt: u64,
    provider: Provider,
    prompt: &str,
    local: &LocalChain,
    signer: Option<&Signer>,
) -> Result<Option<Finding>, Error> {
    let started = Instant::now();
    let log_path = layout
        .logs_directory
        .join(format!("generator-attempt-{attempt}.log"));
    let activity_path = layout
        .logs_directory
        .join(format!("generator-attempt-{attempt}.activity.log"));
    let status_layout = layout.clone();
    let activity_for_status = activity_path.clone();
    let on_progress: agent::ProgressSink = Arc::new(move |progress: &Progress| {
        let _ = status_layout.write_status(json!({
            "phase": "generator_running",
            "stage": "GENERATE",
            "attempt": attempt,
            "elapsedSeconds": started.elapsed().as_secs(),
            "lastActivity": progress.last_activity,
            "toolCallsStarted": progress.tool_calls_started,
            "toolCallsCompleted": progress.tool_calls_completed,
            "sessionId": progress.session_id,
            "activityLogPath": activity_for_status,
        }));
    });

    // The agent sees the network and the signer through the environment (upstream: a URL in
    // the prompt, nothing else).
    let mut extra_env = local.env();
    if let Some(signer) = signer {
        let expose = spec
            .chain_validation
            .as_ref()
            .map(|c| c.expose_env_vars.clone())
            .unwrap_or_default();
        extra_env.extend(chain::deploy_env(signer, &expose));
    }
    let provider = provider.with_env(extra_env);

    let result = match provider
        .run(RunInput {
            prompt,
            workspace,
            log_path: Some(&log_path),
            activity_log_path: Some(&activity_path),
            timeout: spec.generator.timeout_ms.map(Duration::from_millis),
            on_progress: Some(on_progress),
        })
        .await
    {
        Ok(result) => result,
        // `attemptStages.ts:133-143`: a spawn failure is exit 127, not a crash.
        Err(error) => agent::RunResult {
            exit_code: Some(127),
            stdout: String::new(),
            stderr: error.to_string(),
            duration_ms: 0,
            timed_out: false,
            signal: None,
        },
    };
    layout.append_log(&LogEvent::GeneratorFinished {
        attempt,
        exit_code: result.exit_code,
        duration_ms: result.duration_ms,
        timed_out: result.timed_out,
    })?;
    if result.exit_code == Some(0) {
        return Ok(None);
    }
    let output = if result.stderr.is_empty() {
        result.stdout.clone()
    } else {
        result.stderr.clone()
    };
    Ok(Some(
        Finding::new(
            if result.timed_out {
                format!("generator-timeout:{attempt}")
            } else {
                format!("generator-exit:{attempt}")
            },
            Category::Agent,
            if result.timed_out {
                format!(
                    "Generator agent timed out after {}s",
                    (result.duration_ms as f64 / 1000.0).round() as u64
                )
            } else {
                format!(
                    "Generator agent exited with code {}",
                    result
                        .exit_code
                        .map_or_else(|| "null".to_string(), |c| c.to_string())
                )
            },
        )
        .with_details(truncate_details(&output)),
    ))
}

/// `attemptStages.ts:288-369`, with CHAIN between ASSERT and SMOKE.
#[allow(clippy::too_many_arguments)]
async fn run_validation_stages(
    layout: &Layout,
    spec: &Spec,
    workspace: &Path,
    attempt: u64,
    chain: &ChainHandle,
    signer: Option<&Signer>,
    mark: &chain::Mark,
    context: &VendoredContext,
    generate_finding: Option<Finding>,
) -> Result<ValidationResult, Error> {
    log_stage("ASSERT", None);
    let install_cache = layout.cache_directory.join("install-fingerprint.txt");
    let mut validation = assert::run(workspace, spec, Some(&install_cache)).await?;
    // `attemptStages.ts:377-387`: a GENERATE finding is recorded but must not fail ASSERT.
    if let Some(finding) = generate_finding {
        validation.findings.insert(0, finding);
    }
    if !assert::is_ready_for_smoke(&validation.findings) {
        log_stage("CHAIN", Some("skipped — deterministic gates are not clean"));
        validation.passed = false;
        return Ok(validation);
    }

    if let Some(config) = &spec.chain_validation {
        log_stage("CHAIN", None);
        // On a network Hanvil does not own, the in-process chain is not the one the app used.
        // A ledger built from it would read "the attempt sent no transactions", which is true of
        // that chain and a lie about the run.
        let owns_the_chain = config.network == crate::harness::spec::ChainNetwork::Local;
        let deploy_findings = run_chain_deploy(spec, workspace, signer, &chain.local).await?;
        if !deploy_findings.is_empty() && !owns_the_chain {
            log_stage("SMOKE", Some("skipped — chain deploy failed"));
            validation.findings.extend(deploy_findings);
            validation.passed = false;
            return Ok(validation);
        }
        if !deploy_findings.is_empty() {
            // The ledger is most wanted exactly here: a deploy command that failed usually
            // failed because the chain refused something, and returning before building it
            // would hide that.
            let ledger = Ledger::since(&chain.shared.read(), mark);
            report_ledger(layout, &ledger, attempt, true)?;
            validation.chain_ledger = Some(ledger);
            log_stage("SMOKE", Some("skipped — chain deploy failed"));
            validation.findings.extend(deploy_findings);
            validation.passed = false;
            return Ok(validation);
        }
        // Everything past the deploy commands reads the chain this process owns. On
        // testnet it owns none, and the loader has already refused the recipe keys
        // that would ask for it, so CHAIN ends with the deploy.
        if owns_the_chain {
            // The flat block, then every phase. Assertion indices run on across all of them so a
            // finding id never moves when a phase is added, and the ledger is rebuilt before each
            // set: a `rejections` assertion in one phase must not see the next phase's refusals,
            // which have not happened yet.
            advance_chain_time(chain, config.advance_time_seconds);
            let mut index_offset = 0;
            let mut assertions = &config.assertions;
            let mut phase_label: Option<String> = None;
            let mut phases = config.phases.iter().enumerate();
            let ledger = loop {
                let (ledger, findings) = {
                    let guard = chain.shared.read();
                    let ledger = Ledger::since(&guard, mark);
                    let findings = chain::run_assertions(
                        &guard,
                        assertions,
                        signer,
                        mark,
                        &ledger,
                        index_offset,
                    );
                    (ledger, findings)
                };
                report_ledger(layout, &ledger, attempt, true)?;
                layout.append_log(&LogEvent::ChainAssertionsFinished {
                    attempt,
                    passed: findings.is_empty(),
                    finding_count: findings.len(),
                })?;
                if !assertions.is_empty() {
                    let of_phase = phase_label
                        .as_deref()
                        .map(|name| format!(" — phase {name}"))
                        .unwrap_or_default();
                    log_phase(
                        "Chain assertions",
                        Some(&format!(
                            "{} of {} passed{of_phase}",
                            assertions.len() - findings.len(),
                            assertions.len()
                        )),
                    );
                }
                index_offset += assertions.len();
                if !findings.is_empty() {
                    validation.chain_ledger = Some(ledger);
                    log_stage("SMOKE", Some("skipped — chain assertions failed"));
                    validation.findings.extend(findings);
                    validation.passed = false;
                    return Ok(validation);
                }

                let Some((number, phase)) = phases.next() else {
                    break ledger;
                };
                let label = phase
                    .name
                    .clone()
                    .unwrap_or_else(|| (number + 1).to_string());
                log_phase("Chain phase", Some(&label));
                // The clock moves before the commands run: `increase_time` shifts the offset and
                // `block.timestamp` only follows on the next mined block, so a command that must see
                // the later time has to come after the advance.
                advance_chain_time(chain, phase.advance_time_seconds);
                let deploy_findings =
                    run_deploy_commands(&phase.deploy, spec, workspace, signer, &chain.local)
                        .await?;
                if !deploy_findings.is_empty() {
                    let ledger = Ledger::since(&chain.shared.read(), mark);
                    report_ledger(layout, &ledger, attempt, true)?;
                    validation.chain_ledger = Some(ledger);
                    log_stage("SMOKE", Some("skipped — chain deploy failed"));
                    validation.findings.extend(deploy_findings);
                    validation.passed = false;
                    return Ok(validation);
                }
                assertions = &phase.assertions;
                phase_label = Some(label);
            };

            validation.chain_ledger = Some(ledger);
        }
    }

    let Some(playwright_path) = spec.validators.playwright_path.clone() else {
        return Ok(validation);
    };
    run_browser_stages(
        layout,
        spec,
        workspace,
        attempt,
        chain,
        signer,
        mark,
        context,
        &playwright_path,
        validation,
    )
    .await
}

/// `attemptStages.ts:321-368`: one dev server per attempt, borrowed by SMOKE and EVALUATE and
/// stopped whatever happens — after EVALUATE, not before it (the upstream bug this ordering
/// pins). The MCP delivery wraps EVALUATE only.
#[allow(clippy::too_many_arguments)]
async fn run_browser_stages(
    layout: &Layout,
    spec: &Spec,
    workspace: &Path,
    attempt: u64,
    chain: &ChainHandle,
    signer: Option<&Signer>,
    mark: &chain::Mark,
    context: &VendoredContext,
    playwright_path: &Path,
    mut validation: ValidationResult,
) -> Result<ValidationResult, Error> {
    let gate = smoke::load_gate_config(playwright_path)?;
    let run_evaluate = spec.validator_enabled() && spec.eval_paths.is_some();
    let mut env = chain.local.env();
    if let Some(signer) = signer {
        let expose = spec
            .chain_validation
            .as_ref()
            .map(|c| c.expose_env_vars.clone())
            .unwrap_or_default();
        env.extend(chain::deploy_env(signer, &expose));
    }
    log_stage("SMOKE", Some("booting dev server"));
    let mut dev_server = devserver::start(workspace, &gate.server, "runtime", &env).await?;
    let choice = smoke::resolve_browser();
    let output_dir = smoke::output_dir(&layout.run_directory);
    let _ = std::fs::create_dir_all(&output_dir);

    let (gate_result, smoke_findings) = smoke::run_gate(
        workspace,
        playwright_path,
        &gate,
        &mut dev_server,
        &output_dir,
        &choice,
    )
    .await;
    validation.findings.extend(smoke_findings);
    validation.playwright_gate = Some(gate_result);
    validation.passed = validation
        .findings
        .iter()
        .all(|f| f.category == Category::Agent);

    let outcome: Result<ValidationResult, Error> = async {
        if !validation.passed {
            log_stage("EVALUATE", Some("skipped — smoke gate failed"));
            return Ok(validation);
        }
        if !run_evaluate {
            return Ok(validation);
        }
        let (extra_args, workspace_file) = match spec.agent.mcp() {
            McpDelivery::ConfigFlag(flag) => {
                let config_path = mcp::write_config(&layout.run_directory, &choice)?;
                (
                    vec![
                        flag.to_string(),
                        config_path.display().to_string(),
                        "--strict-mcp-config".to_string(),
                    ],
                    None,
                )
            }
            McpDelivery::WorkspaceFile(relative) => (
                Vec::new(),
                Some(mcp::WorkspaceFile::install(
                    workspace,
                    relative,
                    &choice,
                    &output_dir,
                )?),
            ),
        };
        // Rebuilt here: the app kept working the chain while the browser gate drove it, and
        // the validator is judging what the app did, not what CHAIN saw.
        let ledger = Ledger::since(&chain.shared.read(), mark);
        report_ledger(layout, &ledger, attempt, false)?;
        let evaluation = run_evaluate_stage(
            layout,
            spec,
            workspace,
            attempt,
            &dev_server.url,
            signer,
            context,
            &extra_args,
            &chain.local,
            env.clone(),
            Some(&ledger),
        )
        .await?;
        validation.chain_ledger = Some(ledger);
        if let Some(file) = workspace_file {
            file.restore();
        }
        if !evaluation.passed {
            validation.passed = false;
            validation.findings.extend(evaluation.findings.clone());
        }
        validation.evaluation = Some(evaluation);
        Ok(validation)
    }
    .await;
    dev_server.stop().await;
    outcome
}

/// `attemptStages.ts:231-279`.
#[allow(clippy::too_many_arguments)]
async fn run_evaluate_stage(
    layout: &Layout,
    spec: &Spec,
    workspace: &Path,
    attempt: u64,
    server_url: &str,
    signer: Option<&Signer>,
    context: &VendoredContext,
    extra_args: &[String],
    local: &LocalChain,
    env: std::collections::BTreeMap<String, String>,
    ledger: Option<&Ledger>,
) -> Result<findings::Evaluation, Error> {
    let prompt_path = layout
        .prompts_directory
        .join(format!("validator-attempt-{attempt}.txt"));
    layout.append_log(&LogEvent::ValidatorStarted {
        attempt,
        prompt_path,
        server_url: server_url.to_string(),
    })?;
    log_stage("EVALUATE", Some(server_url));
    let evaluation = evaluate::run(EvaluationInput {
        workspace,
        spec,
        attempt,
        layout,
        server_url,
        signer,
        eval_relative_path: context.eval_relative_path.as_deref(),
        extra_args,
        mirror_base_url: &local.mirror_url,
        env,
        chain_ledger: ledger,
    })
    .await;
    write_json_file(
        &layout
            .logs_directory
            .join(format!("evaluation-attempt-{attempt}.json")),
        &evaluation,
    )?;
    layout.append_log(&LogEvent::ValidatorFinished {
        attempt,
        passed: evaluation.passed,
        finding_count: evaluation.findings.len(),
        duration_ms: evaluation.duration_ms,
        infrastructure_failure: evaluation.infrastructure_failure,
        infrastructure_failure_reason: evaluation.infrastructure_failure_reason.clone(),
    })?;
    Ok(evaluation)
}

/// `attemptStages.ts:196-228`.
async fn run_chain_deploy(
    spec: &Spec,
    workspace: &Path,
    signer: Option<&Signer>,
    local: &LocalChain,
) -> Result<Vec<Finding>, Error> {
    let Some(config) = &spec.chain_validation else {
        return Ok(Vec::new());
    };
    run_deploy_commands(&config.deploy, spec, workspace, signer, local).await
}

/// `evm_increaseTime` under the lock, with the console line. A no-op at zero.
fn advance_chain_time(chain: &ChainHandle, seconds: u64) {
    if seconds == 0 {
        return;
    }
    chain.shared.write().increase_time(seconds);
    log_phase("Chain time advanced", Some(&format!("{seconds} s")));
}

/// The deploy commands of the flat block or of one phase, with the same environment either way.
async fn run_deploy_commands(
    commands: &[crate::harness::spec::CommandSpec],
    spec: &Spec,
    workspace: &Path,
    signer: Option<&Signer>,
    local: &LocalChain,
) -> Result<Vec<Finding>, Error> {
    let Some(config) = &spec.chain_validation else {
        return Ok(Vec::new());
    };
    let Some(signer) = signer else {
        return Ok(Vec::new());
    };
    let mut env = local.env();
    env.extend(chain::deploy_env(signer, &config.expose_env_vars));
    let mut findings = Vec::new();
    for command in commands {
        let name = command
            .name
            .clone()
            .unwrap_or_else(|| command.command.clone());
        log_phase(
            "Chain deploy",
            Some(&format!("{name} — {}", command.command)),
        );
        let result = command::execute(Execute {
            command: &command.command,
            args: &[],
            cwd: workspace,
            env: &env,
            timeout: command.timeout_ms.map(Duration::from_millis),
            shell: true,
            stream_output: false,
        })
        .await
        .map_err(|source| Error::Spawn {
            name: name.clone(),
            source,
        })?;
        if result.exit_code != Some(0) {
            let output = if result.stderr.is_empty() {
                result.stdout
            } else {
                result.stderr
            };
            findings.push(
                Finding::new(
                    format!("chain-deploy:{name}"),
                    Category::Commands,
                    format!("Chain deploy command failed: {name}"),
                )
                .with_details(truncate_details(&output)),
            );
        }
    }
    Ok(findings)
}

/// `attemptReporting.ts:87-143`.
fn record_attempt_result(
    layout: &Layout,
    attempt: u64,
    validation: &ValidationResult,
    delta: &Delta,
) -> Result<(), Error> {
    write_json_file(
        &layout
            .logs_directory
            .join(format!("validation-attempt-{attempt}.json")),
        validation,
    )?;
    if let Some(gate) = &validation.playwright_gate {
        write_json_file(
            &layout
                .logs_directory
                .join(format!("playwright-gate-attempt-{attempt}.json")),
            gate,
        )?;
    }
    layout.append_log(&LogEvent::ValidationFinished {
        attempt,
        passed: validation.passed,
        finding_count: delta.open.len(),
        open_finding_ids: delta.open.clone(),
        fixed_finding_ids: delta.fixed.clone(),
        introduced_finding_ids: delta.introduced.clone(),
    })?;
    layout.write_status(json!({
        "phase": "validated",
        "attempt": attempt,
        "passed": validation.passed,
        "findingCount": delta.open.len(),
        "openFindingIds": delta.open,
        "fixedFindingIds": delta.fixed,
        "evaluationPassed": validation.evaluation.as_ref().map(|e| e.passed),
        "infrastructureFailure": validation.evaluation.as_ref().is_some_and(findings::Evaluation::is_infrastructure_failure),
    }))?;
    let summary = match &validation.evaluation {
        Some(evaluation) if evaluation.passed => evaluation.verdict.as_ref().map_or_else(
            || "evaluate checklist passed".to_string(),
            |v| v.summary.clone(),
        ),
        Some(evaluation) if evaluation.is_infrastructure_failure() => format!(
            "infrastructure: {}",
            evaluation
                .infrastructure_failure_reason
                .as_deref()
                .unwrap_or("")
        ),
        Some(_) => format_delta(delta),
        None if validation.passed => match &validation.playwright_gate {
            Some(gate) => format!("playwright gate passed ({} routes)", gate.routes.len()),
            None => "deterministic gates passed".to_string(),
        },
        None => format_delta(delta),
    };
    println!(
        "[hanvil] Attempt {attempt} {} — {summary}",
        if validation.passed {
            "PASSED"
        } else {
            "FAILED"
        }
    );
    Ok(())
}

/// Hanvil: the chain ledger for the attempt — every transaction it caused, in consensus order,
/// including the ones the node refused before consensus and which therefore left no record.
/// Written to `logs/chain-ledger-attempt-N.json` beside the state dump.
///
/// Printed once, at CHAIN, because that is the ledger the assertions were evaluated against.
/// It is rewritten before EVALUATE so the file also holds what the app did while the browser
/// gate drove it, which is what the validator and the next repair prompt are given.
fn report_ledger(layout: &Layout, ledger: &Ledger, attempt: u64, print: bool) -> Result<(), Error> {
    if ledger.is_empty() {
        if print {
            log_phase(
                "Chain ledger",
                Some(&format!(
                    "attempt {attempt} — the attempt sent no transactions"
                )),
            );
        }
        return Ok(());
    }
    if print {
        log_phase(
            "Chain ledger",
            Some(&format!("attempt {attempt} — {}", ledger.summary())),
        );
        for line in ledger.table().lines() {
            println!("  {line}");
        }
    }
    let path = layout
        .logs_directory
        .join(format!("chain-ledger-attempt-{attempt}.json"));
    // A failed write is reported and does not stop the attempt, as with the state dump: the
    // ledger is evidence about the run, not part of it.
    match serde_json::to_string_pretty(ledger) {
        Ok(json) => {
            if let Err(error) = std::fs::write(&path, format!("{json}\n")) {
                log_phase(
                    "Chain ledger not written",
                    Some(&format!("{}: {error}", path.display())),
                );
            }
        }
        Err(error) => log_phase("Chain ledger not written", Some(&error.to_string())),
    }
    Ok(())
}

/// Hanvil: dump the chain the attempt left, so `hanvil --state` replays it and `--continue`
/// reloads it. Serialised under a read lock; written outside it.
fn write_state_dump(layout: &Layout, chain: &ChainHandle, attempt: u64) -> Result<(), Error> {
    let started = Instant::now();
    let json = chain.shared.read().to_json();
    let json = match json {
        Ok(json) => json,
        Err(error) => {
            log_phase("Chain state not written", Some(&error.to_string()));
            return Ok(());
        }
    };
    let path = layout
        .logs_directory
        .join(format!("chain-state-attempt-{attempt}.json"));
    let bytes = json.len() as u64;
    if let Err(error) = std::fs::write(&path, json) {
        log_phase(
            "Chain state not written",
            Some(&format!("{}: {error}", path.display())),
        );
        return Ok(());
    }
    let duration_micros = micros_since(started);
    layout.append_log(&LogEvent::ChainStateWritten {
        attempt,
        path: path.clone(),
        bytes,
        duration_micros,
    })?;
    session::update_session(&layout.run_directory, |session| {
        session.chain_state_path = Some(path);
    })?;
    log_phase(
        "Chain state written",
        Some(&format!(
            "attempt {attempt} — {bytes} bytes — {duration_micros} µs"
        )),
    );
    Ok(())
}

/// Whole microseconds since `started`; saturates rather than truncating a u128.
pub(crate) fn micros_since(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX)
}

/// `attemptReporting.ts:145-173`.
fn abort_on_infrastructure_failure(
    layout: &Layout,
    attempt: u64,
    validation: &ValidationResult,
) -> Result<(), Error> {
    let reason = validation
        .evaluation
        .as_ref()
        .and_then(|e| e.infrastructure_failure_reason.clone())
        .unwrap_or_else(|| "evaluation infrastructure failure".to_string());
    layout.append_log(&LogEvent::ValidatorInfraAborted {
        attempt,
        reason: reason.clone(),
    })?;
    let mut lines = vec![
        "Repair loop aborted: failure is harness/agent tooling, not the generated app.".to_string(),
        reason.clone(),
    ];
    if let Some(evaluation) = &validation.evaluation {
        lines.extend(
            evaluation
                .findings
                .iter()
                .map(|f| format!("- [{}] {}", f.category.as_str(), f.message)),
        );
    }
    layout.append_note(
        &format!("Attempt {attempt} evaluation infrastructure abort"),
        &lines.join("\n"),
    )?;
    log_phase(
        "Aborting repair loop after evaluation infrastructure failure",
        Some(&reason),
    );
    Ok(())
}

/// `attemptReporting.ts:175-202` and `sessionRunner.ts:197-219`: commit, record the sha in
/// `session.json`, note any secrets left unstaged.
async fn checkpoint(
    layout: &Layout,
    workspace: &Path,
    attempt: u64,
    validation: &ValidationResult,
) -> Result<(), Error> {
    let commit =
        git::commit_attempt(workspace, attempt, validation.passed, &validation.findings).await?;
    let sha = match &commit.commit_sha {
        Some(sha) => sha.clone(),
        None => git::head_sha(workspace).await?,
    };
    session::record_checkpoint(
        &layout.run_directory,
        attempt,
        &sha,
        if validation.passed {
            "passed"
        } else {
            "failed"
        },
    )?;
    if !commit.skipped_secrets.is_empty() {
        layout.append_note(
            &format!("Attempt {attempt} checkpoint skipped secrets"),
            &commit
                .skipped_secrets
                .iter()
                .map(|path| format!("- {path}"))
                .collect::<Vec<_>>()
                .join("\n"),
        )?;
    }
    layout.append_log(&LogEvent::WorkspaceGitCommitted {
        attempt,
        committed: commit.committed,
        commit_sha: commit.commit_sha.clone(),
        message: commit.message.clone(),
    })?;
    match &commit.commit_sha {
        Some(sha) if commit.committed => log_phase(
            "Workspace committed",
            Some(&format!(
                "{} @ {}",
                commit.message,
                &sha[..sha.len().min(8)]
            )),
        ),
        _ => log_phase(
            "Workspace unchanged",
            Some("no git commit needed for this attempt"),
        ),
    }
    Ok(())
}

struct FinishInput<'a> {
    layout: &'a Layout,
    spec: &'a Spec,
    workspace: &'a Path,
    attempts: u64,
    attempts_this_cycle: u64,
    max_attempts: u64,
    is_continue: bool,
    cycle: Option<u64>,
    started: Instant,
    started_at: String,
    validation: ValidationResult,
    delta: Delta,
}

/// `attemptReporting.ts:204-282`.
fn finish_run(input: FinishInput<'_>) -> Result<RunReport, Error> {
    let FinishInput {
        layout,
        spec,
        workspace,
        attempts,
        attempts_this_cycle,
        max_attempts,
        is_continue,
        cycle,
        started,
        started_at,
        validation,
        delta,
    } = input;
    let finished_at = now_iso8601();
    let report = RunReport {
        spec_name: spec.name.clone(),
        spec_path: spec.spec_path.clone(),
        run_directory: layout.run_directory.clone(),
        workspace_path: workspace.to_path_buf(),
        attempts,
        max_attempts,
        cycle,
        attempts_this_cycle: Some(attempts_this_cycle),
        passed: validation.passed,
        open_finding_ids: delta.open.clone(),
        fixed_finding_ids: delta.fixed.clone(),
        slices: None,
        started_at,
        finished_at: finished_at.clone(),
        duration_ms: started.elapsed().as_millis() as u64,
        evaluation: validation.evaluation.clone(),
        chain_ledger: validation.chain_ledger.as_ref().map(Ledger::counts),
        validation,
    };
    write_json_file(&layout.report_path, &report)?;
    if let (true, Some(cycle)) = (is_continue, cycle) {
        write_json_file(
            &layout.reports_directory.join(format!("cycle-{cycle}.json")),
            &report,
        )?;
    }
    layout.append_log(&LogEvent::RunFinished {
        passed: report.passed,
        attempts: report.attempts,
        report_path: layout.report_path.clone(),
    })?;
    let mut note = vec![
        format!(
            "{} after {} attempt(s) this kick.",
            if report.passed { "Passed" } else { "Failed" },
            attempts_this_cycle
        ),
        format!("Findings: {}", format_delta(&delta)),
        format!("Report: {}", layout.report_path.display()),
    ];
    if is_continue {
        note.push(format!("Total attempts in project: {}", report.attempts));
    }
    layout.append_note(
        &match cycle {
            Some(cycle) if is_continue => {
                format!("Run continued finished: {} (cycle {cycle})", spec.name)
            }
            _ => format!("Run finished: {}", spec.name),
        },
        &note.join("\n"),
    )?;
    layout.write_status(json!({
        "phase": "finished",
        "passed": report.passed,
        "attempts": report.attempts,
        "openFindingIds": delta.open,
        "reportPath": layout.report_path,
    }))?;
    log_phase(
        &format!(
            "Run finished: {}",
            if report.passed { "PASSED" } else { "FAILED" }
        ),
        Some(&format!(
            "{} — {}",
            format_delta(&delta),
            layout.report_path.display()
        )),
    );
    Ok(report)
}
