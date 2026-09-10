//! `hanvil run`. `sessionRunner.ts`, `runCleanup.ts` and `runOutro.ts` of hedera-harness dev
//! @ 587a2f3, with the node booted in this process before anything else happens.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::cli::{NodeArgs, RunArgs, ValidateArgs};
use crate::harness::artifacts::{self, CONTEXT_DIR, LogEvent, RUNTIME_DIR, now_iso8601};
use crate::harness::attempt::{self, ChainHandle, LoopInput, RunReport, SliceReport};
use crate::harness::chain::{self, LocalChain, Signer};
use crate::harness::findings::Finding;
use crate::harness::git;
use crate::harness::prompt::{Slice, VendoredContext, VendoredSkill};
use crate::harness::session::{self, Mode, PrepareInput, Prepared, Session, log_phase};
use crate::harness::spec::{self, ChainNetwork, Loaded, Spec};
use crate::harness::{DEFAULT_SPEC_PATH, env};
use crate::node::{self, Node};
use crate::state::{self, Clock};

/// Anything that ends a run before its report.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The recipe.
    #[error(transparent)]
    Spec(#[from] spec::Error),
    /// The node could not come up.
    #[error(transparent)]
    Node(#[from] node::Error),
    /// Preparing the session.
    #[error(transparent)]
    Session(#[from] session::Error),
    /// The attempt loop.
    #[error(transparent)]
    Attempt(#[from] attempt::Error),
    /// ASSERT, from `validate`.
    #[error(transparent)]
    Assert(#[from] crate::harness::assert::Error),
    /// The SMOKE gate config, from `validate`.
    #[error(transparent)]
    Smoke(#[from] crate::harness::smoke::Error),
    /// The dev server, from `validate`.
    #[error(transparent)]
    DevServer(#[from] crate::harness::devserver::Error),
    /// The chain.
    #[error(transparent)]
    Chain(#[from] chain::Error),
    /// Run files.
    #[error(transparent)]
    Artifacts(#[from] artifacts::Error),
    /// `git`.
    #[error(transparent)]
    Git(#[from] git::Error),
    /// A PRD or eval file could not be vendored.
    #[error("vendoring {path}: {source}")]
    Vendor {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// `hanvil run` only drives the chain in this process.
    #[error(
        "chainValidation.network is \"testnet\"; hanvil run drives the in-process chain only. Use network: local, or run this recipe with hedera-harness."
    )]
    TestnetRefused,
    /// `sessionRunner.ts:366-375`.
    #[error(
        "Run completion safety check failed: branch changed unexpectedly.\nexpected={expected}\nactual={actual}"
    )]
    BranchChanged {
        /// The session's branch.
        expected: String,
        /// What `git` reports now.
        actual: String,
    },
}

/// `runCleanup.ts:12-17`.
#[derive(Debug, Clone, Default)]
pub(crate) struct Cleanup {
    /// Relative paths removed.
    pub(crate) removed_paths: Vec<String>,
    /// A harness-written MCP entry was stripped.
    pub(crate) mcp_stripped: bool,
    /// Consumer-relevant dirty paths left behind.
    pub(crate) consumer_dirty_paths: Vec<String>,
    /// No dirty paths.
    pub(crate) tree_clean: bool,
}

/// The run directory of the run in progress, so an interrupt can say so in `status.json`.
static CURRENT_RUN_DIR: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// D13: called from the Ctrl-C handler after the run future was dropped.
pub(crate) fn note_interrupted() {
    let run_dir = CURRENT_RUN_DIR.lock().ok().and_then(|guard| guard.clone());
    if let Some(run_dir) = run_dir {
        let _ = artifacts::write_json_file(
            &run_dir.join("status.json"),
            &json!({ "updatedAt": now_iso8601(), "phase": "interrupted" }),
        );
    }
}

/// `runner.ts:28-63`: ASSERT on the workspace, then the thin SMOKE gate when ASSERT is clean.
/// No agent, no chain, no branch.
pub(crate) async fn validate(
    args: ValidateArgs,
) -> Result<crate::harness::findings::ValidationResult, Error> {
    let workspace = args
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = std::path::absolute(&workspace).unwrap_or(workspace);
    let spec_path = args
        .spec
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC_PATH));
    let loaded = spec::load(&spec_path)?;
    let spec = &loaded.spec;
    let mut validation = crate::harness::assert::run(&workspace, spec, None).await?;
    let Some(playwright_path) = spec.validators.playwright_path.clone() else {
        return Ok(validation);
    };
    if !crate::harness::assert::is_ready_for_smoke(&validation.findings) {
        println!("[hanvil] Skipping Playwright gate because deterministic gates are not clean.");
        return Ok(validation);
    }
    let gate = crate::harness::smoke::load_gate_config(&playwright_path)?;
    println!("[hanvil] Running thin Playwright gate...");
    let mut dev_server = crate::harness::devserver::start(
        &workspace,
        &gate.server,
        "validate",
        &std::collections::BTreeMap::new(),
    )
    .await?;
    let output_dir = std::env::temp_dir().join(format!("hanvil-validate-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&output_dir);
    let (result, findings) = crate::harness::smoke::run_gate(
        &workspace,
        &playwright_path,
        &gate,
        &mut dev_server,
        &output_dir,
        &crate::harness::smoke::resolve_browser(),
    )
    .await;
    dev_server.stop().await;
    let _ = std::fs::remove_dir_all(&output_dir);
    validation.findings.extend(findings);
    validation.passed = validation.findings.is_empty();
    validation.playwright_gate = Some(result);
    Ok(validation)
}

/// What `hanvil run` returns to the CLI.
pub(crate) struct Outcome {
    /// The report, as written to `reports/report.json`.
    pub(crate) report: RunReport,
    /// Lines for the terminal.
    pub(crate) outro: Vec<String>,
}

/// Which port a listener gets: an explicit flag wins, else the recipe's `local.*`, else the
/// node's default. Clap cannot tell an explicit `--port 7546` from the default, so a flag that
/// equals the default defers to the recipe.
fn port_for(flag: u16, default: u16, recipe_url: Option<&str>) -> u16 {
    if flag != default {
        return flag;
    }
    recipe_url
        .and_then(|url| url.rsplit(':').next())
        .and_then(|port| port.trim_end_matches('/').parse().ok())
        .unwrap_or(default)
}

/// `hanvil run`, end to end.
pub(crate) async fn run(args: RunArgs, node_args: NodeArgs) -> Result<Outcome, Error> {
    let workspace = args
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let workspace = std::path::absolute(&workspace).unwrap_or(workspace);
    let spec_path = args
        .spec
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC_PATH));
    let loaded = spec::load(&spec_path)?;
    for warning in &loaded.warnings {
        eprintln!(
            "[hanvil] {}: {warning}",
            spec_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
    }
    let spec = &loaded.spec;
    if spec
        .chain_validation
        .as_ref()
        .is_some_and(|c| c.network == ChainNetwork::Testnet)
    {
        return Err(Error::TestnetRefused);
    }
    // CLI flag > environment > recipe.
    let max_attempts = args
        .max_attempts
        .or_else(env::max_attempts)
        .unwrap_or(spec.max_attempts);

    let node = boot_node(&node_args, spec, &workspace).await?;
    let local = LocalChain {
        rpc_url: format!("http://{}", node.rpc.local_addr),
        mirror_url: format!("http://{}", node.mirror.local_addr),
        grpc_url: node.grpc.local_addr.to_string(),
        chain_id: node.shared.read().chain_id(),
    };
    log_phase(
        "Local Hedera network",
        Some(&format!(
            "{} · {} · {} (chain id {})",
            local.rpc_url, local.mirror_url, local.grpc_url, local.chain_id
        )),
    );

    let outcome = drive(&args, &loaded, &workspace, max_attempts, &node, local).await;
    node.shutdown();
    outcome
}

/// Boot the node: `--state`-style reload of the last attempt's chain on `--continue` is done
/// in `drive`, after the session says which dump.
async fn boot_node(node_args: &NodeArgs, spec: &Spec, workspace: &Path) -> Result<Node, Error> {
    let local = spec
        .chain_validation
        .as_ref()
        .and_then(|c| c.local.as_ref());
    let mut args = node_args.clone();
    args.port = port_for(args.port, 7546, local.map(|l| l.rpc_url.as_str()));
    args.mirror_port = port_for(args.mirror_port, 5551, local.map(|l| l.mirror_url.as_str()));
    args.grpc_port = port_for(args.grpc_port, 50211, local.map(|l| l.grpc_url.as_str()));
    let clock: Arc<dyn Clock> = Arc::new(state::time::SystemClock);
    let chain = node::load_or_genesis(&args, clock.as_ref())?;
    let _ = workspace;
    Ok(Node::boot(&args, chain, clock).await?)
}

/// `sessionRunner.ts:62-404`.
async fn drive(
    args: &RunArgs,
    loaded: &Loaded,
    workspace: &Path,
    max_attempts: u64,
    node: &Node,
    local: LocalChain,
) -> Result<Outcome, Error> {
    let spec = &loaded.spec;
    log_phase(
        "Preparing harness session",
        Some(&format!("{} @ {}", spec.name, workspace.display())),
    );
    let prepared = session::prepare(PrepareInput {
        workspace,
        loaded,
        force_new: args.new,
        continue_branch: args.continue_branch.as_deref(),
        skip_baseline: false,
    })
    .await?;
    let Prepared {
        mode,
        layout,
        session,
        starting_attempt,
        cycle,
        ..
    } = prepared;
    if let Ok(mut current) = CURRENT_RUN_DIR.lock() {
        *current = Some(layout.run_directory.clone());
    }
    let is_continue = mode == Mode::Continue;
    let started = Instant::now();
    let started_at = now_iso8601();

    if is_continue {
        reload_chain(node, &session);
    }

    log_phase(
        if is_continue {
            "Run continued"
        } else {
            "Run started"
        },
        Some(&format!(
            "{} ({})",
            layout.run_directory.display(),
            session.branch
        )),
    );
    layout.write_status(json!({
        "phase": if is_continue { "continued" } else { "started" },
        "specName": spec.name,
        "runDirectory": layout.run_directory,
        "layoutMode": "in-place-run",
        "workspacePath": layout.workspace,
        "branch": session.branch,
        "baseBranch": session.base_branch,
        "sessionId": session.session_id,
        "startingAttempt": starting_attempt,
        "maxAttemptsThisCycle": max_attempts,
        "cycle": cycle,
    }))?;
    if let Some(cycle) = cycle.filter(|_| is_continue) {
        layout.append_log(&LogEvent::RunContinued {
            spec_name: spec.name.clone(),
            run_directory: layout.run_directory.clone(),
            cycle,
            starting_attempt,
            max_attempts_this_cycle: max_attempts,
        })?;
        layout.append_log(&LogEvent::CycleStarted {
            cycle,
            starting_attempt,
            max_attempts_this_cycle: max_attempts,
        })?;
    } else {
        layout.append_log(&LogEvent::RunStarted {
            spec_name: spec.name.clone(),
            run_directory: layout.run_directory.clone(),
        })?;
    }
    let mut note = vec![
        format!("Branch: {}", session.branch),
        format!(
            "Base: {} @ {}",
            session.base_branch,
            &session.base_sha[..session.base_sha.len().min(8)]
        ),
        format!("Run directory: {}", layout.run_directory.display()),
        format!("Workspace: {}", layout.workspace.display()),
        format!("Spec: {}", spec.spec_path.display()),
        "Layout: in-place-run".to_string(),
    ];
    if let Some(baseline) = &session.baseline_result {
        note.push(format!(
            "Baseline: {} ({} command(s))",
            if baseline.passed { "passed" } else { "failed" },
            baseline.commands.len()
        ));
    }
    layout.append_note(
        &match cycle {
            Some(cycle) if is_continue => format!("Run continued: {} (cycle {cycle})", spec.name),
            _ => format!("Run started: {}", spec.name),
        },
        &note.join("\n"),
    )?;
    log_phase(
        "Using in-place workspace",
        Some(&workspace.display().to_string()),
    );

    // Skills arrive with skills.rs; until then every run is `--no-skills`, and says so.
    let skills: Vec<VendoredSkill> = Vec::new();
    log_phase("Product skills", Some("skipped (--no-skills)"));

    let mut signer: Option<Signer> = None;
    let outcome = async {
        if let Some(config) = &spec.chain_validation {
            let provisioned = {
                let now = node.clock.now();
                let mut guard = node.shared.write();
                chain::provision(&mut guard, config, &layout.run_directory, now)?
            };
            layout.append_log(&LogEvent::ChainSignerProvisioned {
                account_id: provisioned.signer.account_id.clone(),
                evm_address: provisioned.signer.evm_address.clone(),
                network: provisioned.signer.network.clone(),
                reused: provisioned.reused,
                topped_up_hbar: provisioned.topped_up_hbar,
                replaced_deleted: provisioned.replaced_deleted.then_some(true),
            })?;
            log_phase(
                if provisioned.reused {
                    "Chain signer reused"
                } else {
                    "Chain signer provisioned"
                },
                Some(&format!(
                    "{} ({})",
                    provisioned.signer.account_id, provisioned.signer.evm_address
                )),
            );
            signer = Some(provisioned.signer);
        }

        let chain_handle = ChainHandle {
            shared: Arc::clone(&node.shared),
            clock: Arc::clone(&node.clock),
            local: local.clone(),
        };
        let slice_count = spec.prd_paths.len();
        let first_slice = if is_continue {
            session
                .slice_index
                .unwrap_or(0)
                .min(slice_count.saturating_sub(1))
        } else {
            0
        };
        let mut slices: Vec<SliceReport> = Vec::new();
        let mut attempt_cursor = starting_attempt;
        let mut report: Option<RunReport> = None;

        for slice_index in first_slice..slice_count {
            let (prd_path, eval_path) = spec.slice(slice_index);
            let context = vendor_context(workspace, &prd_path, eval_path.as_deref())?;
            layout.append_log(&LogEvent::ContextVendored {
                prd_path: PathBuf::from(&context.prd_relative_path),
                eval_path: context.eval_relative_path.as_ref().map(PathBuf::from),
                workspace_context_dir: workspace.join(CONTEXT_DIR),
            })?;
            if slice_count > 1 {
                log_phase(
                    &format!("Increment {}/{slice_count}", slice_index + 1),
                    Some(
                        &prd_path
                            .strip_prefix(&spec.project_root)
                            .unwrap_or(&prd_path)
                            .display()
                            .to_string(),
                    ),
                );
            }
            let slice_report = attempt::run_loop(LoopInput {
                layout: &layout,
                spec,
                max_attempts,
                is_continue: is_continue && slice_index == first_slice,
                cycle,
                starting_attempt: attempt_cursor,
                started,
                started_at: started_at.clone(),
                workspace,
                skills: &skills,
                context: &context,
                signer: signer.as_ref(),
                slice: Slice {
                    index: slice_index,
                    count: slice_count,
                },
                previous_open_finding_ids: if slice_index == first_slice {
                    session.open_finding_ids.clone().unwrap_or_default()
                } else {
                    Vec::new()
                },
                chain: &chain_handle,
            })
            .await?;
            slices.push(SliceReport {
                index: slice_index,
                prd_path: prd_path.clone(),
                eval_path: eval_path.clone(),
                passed: slice_report.passed,
                attempts: slice_report
                    .attempts_this_cycle
                    .unwrap_or(slice_report.attempts),
                open_finding_ids: slice_report.open_finding_ids.clone(),
            });
            attempt_cursor = slice_report.attempts + 1;
            let passed = slice_report.passed;
            report = Some(RunReport {
                slices: Some(slices.clone()),
                ..slice_report
            });
            session::update_session(&layout.run_directory, |s| s.slice_index = Some(slice_index))?;
            if !passed {
                if slice_count > 1 {
                    log_phase(
                        &format!("Stopping at increment {}/{slice_count}", slice_index + 1),
                        Some("later increments assume this one landed"),
                    );
                }
                break;
            }
        }

        let mut report = report.ok_or_else(|| Error::Vendor {
            path: spec.spec_path.clone(),
            source: std::io::Error::other("Run produced no report (internal error)."),
        })?;
        report.passed = slices.iter().all(|s| s.passed);
        let head = git::head_sha(workspace).await?;
        let gate_status = if report.passed {
            "passed"
        } else if report
            .evaluation
            .as_ref()
            .is_some_and(crate::harness::findings::Evaluation::is_infrastructure_failure)
        {
            "aborted"
        } else {
            "failed"
        };
        let attempts = report.attempts;
        let open = report.open_finding_ids.clone();
        let cycle_value = report.cycle.unwrap_or(session.cycle);
        session::update_session(&layout.run_directory, |s| {
            s.last_attempt = attempts;
            s.last_checkpoint_sha = head;
            s.cycle = cycle_value;
            s.open_finding_ids = Some(open);
            s.gate_status = gate_status.to_string();
        })?;
        Ok::<RunReport, Error>(report)
    }
    .await;

    // `sessionRunner.ts:313-360`: sweep and clean up whatever happened.
    if let (Some(signer), Some(config)) = (&signer, &spec.chain_validation) {
        let swept = {
            let now = node.clock.now();
            let mut guard = node.shared.write();
            chain::sweep(&mut guard, signer, config, &layout.run_directory, now)
        };
        layout.append_log(&LogEvent::ChainSignerSwept {
            account_id: signer.account_id.clone(),
            success: swept.success,
            error: swept.error.clone(),
        })?;
        if swept.success {
            log_phase("Chain signer swept", Some(&signer.account_id));
        } else {
            log_phase(
                "Chain signer sweep failed (best-effort)",
                swept.error.as_deref(),
            );
        }
    }
    let cleanup = cleanup_runtime(workspace).await?;
    log_phase(
        "Run runtime cleaned",
        Some(&if cleanup.removed_paths.is_empty() {
            "(nothing removable left)".to_string()
        } else {
            cleanup.removed_paths.join(", ")
        }),
    );
    let mut cleanup_note = vec![
        format!(
            "removed={}",
            if cleanup.removed_paths.is_empty() {
                "(none)".to_string()
            } else {
                cleanup.removed_paths.join(", ")
            }
        ),
        format!("mcpStripped={}", cleanup.mcp_stripped),
        format!("treeClean={}", cleanup.tree_clean),
    ];
    if !cleanup.consumer_dirty_paths.is_empty() {
        cleanup_note.push(format!(
            "dirty=\n{}",
            cleanup
                .consumer_dirty_paths
                .iter()
                .map(|p| format!("  - {p}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    layout.append_note("Run runtime cleanup", &cleanup_note.join("\n"))?;
    layout.write_status(json!({
        "phase": "cleanup_complete",
        "branch": session.branch,
        "baseBranch": session.base_branch,
        "treeClean": cleanup.tree_clean,
        "removedPaths": cleanup.removed_paths,
        "mcpStripped": cleanup.mcp_stripped,
    }))?;

    let report = outcome?;
    let current = git::current_branch(workspace).await;
    if current.as_deref() != Some(session.branch.as_str()) {
        return Err(Error::BranchChanged {
            expected: session.branch.clone(),
            actual: current.unwrap_or_else(|| "(detached)".to_string()),
        });
    }
    let final_session = session::read_session(&layout.run_directory).unwrap_or(session);
    // The outro's next steps repeat the path as typed, as upstream does.
    let typed_spec = args
        .spec
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SPEC_PATH));
    let outro = format_outro(&report, &final_session, &cleanup, &typed_spec);
    layout.append_note("Run outro", &outro.join("\n"))?;
    log_phase(
        &format!("Run {}", if report.passed { "passed" } else { "failed" }),
        Some(&final_session.branch),
    );
    Ok(Outcome { report, outro })
}

/// Hanvil: put the chain back where the last attempt left it, so a resumed cycle sees the
/// contracts, topics and accounts the workspace refers to.
fn reload_chain(node: &Node, session: &Session) {
    let Some(path) = &session.chain_state_path else {
        log_phase(
            "Chain is fresh",
            Some("no chain dump from the previous cycle; the repair prompt says so"),
        );
        return;
    };
    match std::fs::read_to_string(path)
        .ok()
        .and_then(|json| state::Chain::from_json(&json).ok())
    {
        Some(chain) => {
            *node.shared.write() = chain;
            log_phase("Chain reloaded", Some(&path.display().to_string()));
        }
        None => log_phase(
            "Chain is fresh",
            Some(&format!(
                "{} could not be read; the repair prompt says so",
                path.display()
            )),
        ),
    }
}

/// `contextVendor.ts:29-81`: copy the active PRD and eval into `.harness/runtime/context/`.
fn vendor_context(
    workspace: &Path,
    prd_path: &Path,
    eval_path: Option<&Path>,
) -> Result<VendoredContext, Error> {
    let vendor = |source: &Path| -> Result<String, Error> {
        std::fs::read_to_string(source).map_err(|e| Error::Vendor {
            path: source.to_path_buf(),
            source: e,
        })
    };
    let root = workspace.join(CONTEXT_DIR);
    std::fs::create_dir_all(&root).map_err(|e| Error::Vendor {
        path: root.clone(),
        source: e,
    })?;
    let prd_relative = format!("{CONTEXT_DIR}/prd.md");
    std::fs::write(workspace.join(&prd_relative), vendor(prd_path)?).map_err(|e| {
        Error::Vendor {
            path: workspace.join(&prd_relative),
            source: e,
        }
    })?;
    let eval_relative = match eval_path {
        Some(eval) => {
            let relative = format!("{CONTEXT_DIR}/eval.json");
            std::fs::write(workspace.join(&relative), vendor(eval)?).map_err(|e| {
                Error::Vendor {
                    path: workspace.join(&relative),
                    source: e,
                }
            })?;
            Some(relative)
        }
        None => None,
    };
    let manifest = json!({
        "vendoredAt": now_iso8601(),
        "prd": { "relativePath": prd_relative, "sourcePath": prd_path },
        "eval": eval_relative.as_ref().map(|relative| json!({ "relativePath": relative, "sourcePath": eval_path })),
    });
    artifacts::write_json_file(&root.join("manifest.json"), &manifest)?;
    Ok(VendoredContext {
        prd_relative_path: prd_relative,
        eval_relative_path: eval_relative,
        prd_source_path: prd_path.to_path_buf(),
        eval_source_path: eval_path.map(Path::to_path_buf),
    })
}

/// `runCleanup.ts:31-71`: remove ignored runtime dirs and every run's signer file; never
/// `.harness/runs/` itself. The MCP strip arrives with the browser stages.
async fn cleanup_runtime(workspace: &Path) -> Result<Cleanup, Error> {
    let mut removed = Vec::new();
    for relative in [
        RUNTIME_DIR,
        artifacts::ISOLATED_SKILLS_DIR,
        artifacts::ISOLATED_CONTEXT_DIR,
        artifacts::SKILL_CACHE_DIRNAME,
    ] {
        let absolute = workspace.join(relative);
        if absolute.exists() {
            let _ = std::fs::remove_dir_all(&absolute);
            removed.push(relative.to_string());
        }
    }
    if let Ok(runs) = std::fs::read_dir(workspace.join(".harness").join("runs")) {
        for entry in runs.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let signer = entry.path().join(chain::SIGNER_FILENAME);
            if signer.exists() {
                let _ = std::fs::remove_file(&signer);
                removed.push(format!(
                    ".harness/runs/{}/{}",
                    entry.file_name().to_string_lossy(),
                    chain::SIGNER_FILENAME
                ));
            }
        }
    }
    // `runCleanup.ts:73-82`: workspace MCP files any preset may have written.
    // Both files are stripped; `any` would stop at the first.
    let mut mcp_stripped = false;
    for relative in [".cursor/mcp.json", ".mcp.json"] {
        if crate::harness::mcp::strip_harness_entry(&workspace.join(relative)) {
            mcp_stripped = true;
        }
    }
    let consumer_dirty_paths = git::consumer_dirty_paths(workspace).await?;
    Ok(Cleanup {
        removed_paths: removed,
        mcp_stripped,
        tree_clean: consumer_dirty_paths.is_empty(),
        consumer_dirty_paths,
    })
}

/// `runOutro.ts:16-107`. Never implies the harness pushed, opened a PR, merged, deleted a
/// branch, or switched away.
pub(crate) fn format_outro(
    report: &RunReport,
    session: &Session,
    cleanup: &Cleanup,
    spec_path: &Path,
) -> Vec<String> {
    let infra_abort = report
        .evaluation
        .as_ref()
        .is_some_and(crate::harness::findings::Evaluation::is_infrastructure_failure);
    let status = if report.passed {
        "PASSED"
    } else if infra_abort {
        "ABORTED"
    } else {
        "FAILED"
    };
    let mut lines = vec![
        format!("Run {status}"),
        format!("branch={}", session.branch),
        format!(
            "base={} @ {}",
            session.base_branch,
            &session.base_sha[..session.base_sha.len().min(8)]
        ),
        format!("workspace={}", report.workspace_path.display()),
        format!(
            "report={}/reports/report.json",
            report.run_directory.display()
        ),
        format!("session={}/session.json", report.run_directory.display()),
        format!(
            "attempts={}/{}",
            report.attempts_this_cycle.unwrap_or(report.attempts),
            report.max_attempts
        ),
    ];
    if let Some(slices) = report.slices.as_ref().filter(|s| s.len() > 1) {
        lines.push(format!(
            "increments={}/{} delivered",
            slices.iter().filter(|s| s.passed).count(),
            slices.len()
        ));
    }
    if let Some(cycle) = report.cycle.filter(|c| *c > 0) {
        lines.push(format!("cycle={cycle}"));
    }
    let mut findings_line = format!("findings={} open", report.open_finding_ids.len());
    if !report.fixed_finding_ids.is_empty() {
        findings_line.push_str(&format!(", {} fixed", report.fixed_finding_ids.len()));
    }
    lines.push(findings_line);
    lines.push(if cleanup.removed_paths.is_empty() {
        "cleaned=(nothing removable left)".to_string()
    } else {
        format!("cleaned={}", cleanup.removed_paths.join(", "))
    });
    lines.push(if cleanup.mcp_stripped {
        "mcp=stripped harness playwright injection".to_string()
    } else {
        "mcp=unchanged".to_string()
    });
    lines.push(if cleanup.tree_clean {
        "workingTree=clean (consumer-relevant)".to_string()
    } else {
        format!(
            "workingTree=dirty ({} path(s))",
            cleanup.consumer_dirty_paths.len()
        )
    });
    lines.push(String::new());
    lines.push(
        "The harness did not push, open a PR, merge, delete a branch, or switch branches."
            .to_string(),
    );

    if !cleanup.tree_clean {
        lines.push("Remaining consumer-relevant dirty paths (not auto-committed):".to_string());
        lines.extend(
            cleanup
                .consumer_dirty_paths
                .iter()
                .take(12)
                .map(|p| format!("  - {p}")),
        );
        if cleanup.consumer_dirty_paths.len() > 12 {
            lines.push(format!(
                "  …and {} more",
                cleanup.consumer_dirty_paths.len() - 12
            ));
        }
        lines.push(String::new());
    }
    if report.passed {
        lines.extend([
            "Optional next steps (run manually):".to_string(),
            format!("  git push -u origin {}", session.branch),
            format!("  gh pr create --base {}", session.base_branch),
            String::new(),
            "Optional before merge: squash harness attempt commits.".to_string(),
        ]);
    } else {
        lines.extend([
            "You remain on the harness run branch with persisted reports.".to_string(),
            String::new(),
            "Continue (same branch, automatic session match):".to_string(),
            format!("  hanvil run {}", spec_path.display()),
            String::new(),
            "Start a fresh branch for the same spec:".to_string(),
            format!("  hanvil run {} --new", spec_path.display()),
            String::new(),
            "Inspect:".to_string(),
            format!(
                "  cat {}/reports/report.json",
                report.run_directory.display()
            ),
            "  git log --oneline".to_string(),
            String::new(),
            "Abandon (manual; not run by the harness):".to_string(),
            format!("  git checkout {}", session.base_branch),
            format!("  git branch -D {}", session.branch),
        ]);
    }
    let open: Vec<&Finding> = report
        .validation
        .findings
        .iter()
        .filter(|f| f.is_open())
        .collect();
    if !report.passed && !open.is_empty() {
        lines.push(String::new());
        lines.push("Open findings:".to_string());
        lines.extend(
            open.iter()
                .take(20)
                .map(|f| format!("- [{}] {}: {}", f.category.as_str(), f.id, f.message)),
        );
        if open.len() > 20 {
            lines.push(format!("  …and {} more", open.len() - 20));
        }
    }
    let fixed: Vec<&Finding> = report
        .validation
        .findings
        .iter()
        .filter(|f| !f.is_open())
        .collect();
    if !fixed.is_empty() {
        lines.push(String::new());
        lines.push(format!("Closed by the last attempt ({}):", fixed.len()));
        lines.extend(fixed.iter().take(10).map(|f| format!("- {}", f.id)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flag_that_equals_the_default_defers_to_the_recipe() {
        assert_eq!(port_for(7546, 7546, Some("http://localhost:7999")), 7999);
        assert_eq!(port_for(0, 7546, Some("http://localhost:7999")), 0);
        assert_eq!(port_for(7546, 7546, None), 7546);
        assert_eq!(port_for(50211, 50211, Some("localhost:50299")), 50299);
        assert_eq!(port_for(5551, 5551, Some("http://127.0.0.1:5552/")), 5552);
    }
}
