//! Start or continue a run on a harness branch. `session.ts` of hedera-harness dev @ 587a2f3:
//! preflight, the branch decision, the clean-tree rule, `session.json`, and the baseline
//! commands that prove the app was healthy before the agent touched it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::harness::artifacts::{self, Layout, now_iso8601};
use crate::harness::command::{self, Execute};
use crate::harness::doctor::{self, Status};
use crate::harness::git;
use crate::harness::spec::{Loaded, Spec};

/// `session.ts:28-29`.
pub(crate) const SESSION_SCHEMA_VERSION: u64 = 1;
pub(crate) const SESSION_FILENAME: &str = "session.json";

/// `session.ts:34-43`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineResult {
    /// Every command exited zero.
    pub(crate) passed: bool,
    /// In order, up to and including the first failure.
    pub(crate) commands: Vec<BaselineCommand>,
}

/// One baseline command as run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BaselineCommand {
    /// `name`, or the command line when unnamed.
    pub(crate) name: String,
    /// As run.
    pub(crate) command: String,
    /// `None` when killed.
    pub(crate) exit_code: Option<i32>,
    /// Wall time.
    pub(crate) duration_ms: u64,
    /// The harness stopped it.
    pub(crate) timed_out: bool,
}

/// `session.ts:45-76`, plus `chainStatePath` so `--continue` can reload the chain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Session {
    /// Always 1.
    pub(crate) schema_version: u64,
    /// The run directory's name.
    pub(crate) session_id: String,
    /// `.harness/runs/<id>`.
    pub(crate) run_directory: PathBuf,
    /// The project.
    pub(crate) workspace_path: PathBuf,
    /// The recipe.
    pub(crate) spec_path: PathBuf,
    /// `spec.name`, as upstream stores it.
    pub(crate) spec_slug: String,
    /// `harness/run-<slug>-<id>`.
    pub(crate) branch: String,
    /// Where the harness branch was cut from.
    pub(crate) base_branch: String,
    /// Its sha.
    pub(crate) base_sha: String,
    /// `git rev-parse --show-toplevel`.
    pub(crate) repository_root: PathBuf,
    /// ISO 8601.
    pub(crate) started_at: String,
    /// ISO 8601.
    pub(crate) updated_at: String,
    /// `--continue` cycles so far; 0 for a fresh run.
    pub(crate) cycle: u64,
    /// Highest attempt number reached.
    pub(crate) last_attempt: u64,
    /// HEAD after the last checkpoint; `--continue` refuses when HEAD moved.
    pub(crate) last_checkpoint_sha: String,
    /// `pending`, `passed`, `failed`, `aborted`.
    pub(crate) gate_status: String,
    /// Baseline outcome, when it ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) baseline_result: Option<BaselineResult>,
    /// Increment the last cycle stopped on, zero-based.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) slice_index: Option<usize>,
    /// Finding ids still failing when the last cycle stopped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) open_finding_ids: Option<Vec<String>>,
    /// Hanvil: the chain dump the last attempt left, reloaded by `--continue`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) chain_state_path: Option<PathBuf>,
}

/// `session.ts:100-108`: a refusal with a stable code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct SessionError {
    /// `preflight-failed`, `baseline-failed`, `conflicting-flags`, …
    pub(crate) code: String,
    /// What to tell the user.
    pub(crate) message: String,
}

fn refuse(code: &str, message: impl Into<String>) -> Error {
    Error::Session(SessionError {
        code: code.to_string(),
        message: message.into(),
    })
}

/// Anything that stops a session from being prepared.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A refusal.
    #[error(transparent)]
    Session(#[from] SessionError),
    /// `git` failed.
    #[error(transparent)]
    Git(#[from] git::Error),
    /// A run file could not be written.
    #[error(transparent)]
    Artifacts(#[from] artifacts::Error),
    /// A baseline command could not be spawned.
    #[error("running baseline command {name}: {source}")]
    Spawn {
        /// The command's name.
        name: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `session.ts:31`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// A fresh harness branch.
    Start,
    /// Resuming on an existing one.
    Continue,
}

/// `session.ts:91-98`.
pub(crate) struct Prepared {
    /// Start or continue.
    pub(crate) mode: Mode,
    /// Where this run writes.
    pub(crate) layout: Layout,
    /// The persisted session.
    pub(crate) session: Session,
    /// The repository after preparation.
    pub(crate) snapshot: git::Snapshot,
    /// 1 for a start; `lastAttempt + 1` for a continue.
    pub(crate) starting_attempt: u64,
    /// `None` for a start.
    pub(crate) cycle: Option<u64>,
}

/// What `prepare` needs to decide.
pub(crate) struct PrepareInput<'a> {
    /// The project.
    pub(crate) workspace: &'a Path,
    /// The recipe.
    pub(crate) loaded: &'a Loaded,
    /// `--new`.
    pub(crate) force_new: bool,
    /// `--continue <branch>`.
    pub(crate) continue_branch: Option<&'a str>,
    /// Tests: skip `baseline.commands`.
    pub(crate) skip_baseline: bool,
}

/// `session.ts:110-112`.
pub(crate) fn session_path(run_directory: &Path) -> PathBuf {
    run_directory.join(SESSION_FILENAME)
}

/// `session.ts:114-130`.
pub(crate) fn read_session(run_directory: &Path) -> Option<Session> {
    let raw = std::fs::read_to_string(session_path(run_directory)).ok()?;
    let session: Session = serde_json::from_str(&raw).ok()?;
    (session.schema_version == SESSION_SCHEMA_VERSION).then_some(session)
}

/// `session.ts:132-138`.
pub(crate) fn write_session(session: &Session) -> Result<(), artifacts::Error> {
    let stamped = Session {
        updated_at: now_iso8601(),
        ..session.clone()
    };
    artifacts::write_json_file(&session_path(&session.run_directory), &stamped)
}

/// `session.ts:140-158`.
pub(crate) fn update_session(
    run_directory: &Path,
    patch: impl FnOnce(&mut Session),
) -> Result<Session, Error> {
    let Some(mut session) = read_session(run_directory) else {
        return Err(refuse(
            "session-missing",
            format!(
                "Cannot update harness session: missing {}",
                session_path(run_directory).display()
            ),
        ));
    };
    patch(&mut session);
    session.updated_at = now_iso8601();
    write_session(&session)?;
    Ok(session)
}

/// `session.ts:580-592`.
pub(crate) fn record_checkpoint(
    run_directory: &Path,
    attempt: u64,
    checkpoint_sha: &str,
    gate_status: &str,
) -> Result<Session, Error> {
    update_session(run_directory, |session| {
        session.last_attempt = attempt;
        session.last_checkpoint_sha = checkpoint_sha.to_string();
        session.gate_status = gate_status.to_string();
    })
}

/// `attemptLoop.ts` `logPhase`.
pub(crate) fn log_phase(title: &str, detail: Option<&str>) {
    match detail {
        Some(detail) => println!("[hanvil] {title} — {detail}"),
        None => println!("[hanvil] {title}"),
    }
}

/// `session.ts:168-218`: preflight, then the branch decision, then start or continue.
pub(crate) async fn prepare(input: PrepareInput<'_>) -> Result<Prepared, Error> {
    let workspace = input.workspace;
    let spec = &input.loaded.spec;
    if std::fs::metadata(workspace).is_err() {
        return Err(refuse(
            "missing-recipe-file",
            format!(
                "Harness run preflight failed: required workspace path does not exist: {}",
                workspace.display()
            ),
        ));
    }
    if input.force_new && input.continue_branch.is_some() {
        return Err(refuse(
            "conflicting-flags",
            "Cannot pass both --new and --continue.",
        ));
    }
    if let Some(branch) = input
        .continue_branch
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        checkout_continue_branch(workspace, branch).await?;
    }

    let snapshot = git::read_snapshot(workspace).await?;
    assert_preflight(spec, workspace).await?;

    let decision = git::decide_branch_action(
        snapshot.branch.as_deref(),
        &spec.name,
        input.force_new,
        input.continue_branch,
    );
    match decision {
        git::BranchDecision::Continue { .. } => {
            continue_session(workspace, input.loaded, snapshot).await
        }
        git::BranchDecision::New { .. } => {
            start_session(workspace, input.loaded, snapshot, input.skip_baseline).await
        }
    }
}

/// `session.ts:462-495` over the doctor's checks: the first failure stops the run.
async fn assert_preflight(spec: &Spec, workspace: &Path) -> Result<(), Error> {
    let report = doctor::run(&doctor::Options {
        spec_path: spec.spec_path.clone(),
        workspace: workspace.to_path_buf(),
        recipe_only: false,
    })
    .await;
    if let Some(failed) = report
        .checks
        .iter()
        .find(|check| check.status == Status::Fail)
    {
        let mut message = format!(
            "Harness run preflight failed: {} — {}",
            failed.name, failed.detail
        );
        if let Some(fix) = &failed.fix {
            message.push('\n');
            message.push_str(fix);
        }
        return Err(refuse("preflight-failed", message));
    }
    Ok(())
}

/// `session.ts:220-255`.
async fn checkout_continue_branch(workspace: &Path, branch: &str) -> Result<(), Error> {
    if !git::is_harness_branch(Some(branch)) {
        return Err(refuse(
            "invalid-continue-branch",
            format!(
                "--continue expects a harness branch (harness/run-*). Got {}.",
                serde_json::to_string(branch).unwrap_or_default()
            ),
        ));
    }
    if git::parse_harness_branch(Some(branch)).is_none() {
        return Err(refuse(
            "invalid-continue-branch",
            format!(
                "Unable to parse harness branch name {}.",
                serde_json::to_string(branch).unwrap_or_default()
            ),
        ));
    }
    git::assert_clean_for_run_start(workspace).await?;
    git::checkout(workspace, branch).await.map_err(|error| {
        refuse(
            "continue-checkout-failed",
            format!(
                "Failed to checkout continue branch {}.\n{error}",
                serde_json::to_string(branch).unwrap_or_default()
            ),
        )
    })
}

/// `session.ts:257-339`.
async fn start_session(
    workspace: &Path,
    loaded: &Loaded,
    snapshot: git::Snapshot,
    skip_baseline: bool,
) -> Result<Prepared, Error> {
    let spec = &loaded.spec;
    let Some(base_branch) = snapshot.branch.clone() else {
        return Err(refuse(
            "detached-or-unborn",
            "Harness run start requires an attached branch name (not detached HEAD).",
        ));
    };
    git::assert_clean_for_run_start(workspace).await?;

    log_phase(
        "Creating harness branch",
        Some(&format!("from {base_branch}")),
    );
    let (branch, head_sha) = git::create_and_checkout_branch(workspace, &spec.name).await?;
    log_phase("Harness branch ready", Some(&branch));
    let layout = Layout::create(
        workspace,
        &spec.name,
        &spec.jsonl_log_path,
        &spec.notes_log_path,
    )?;
    log_phase(
        "Run artifacts directory",
        Some(&layout.run_directory.display().to_string()),
    );

    let started_at = now_iso8601();
    let mut session = Session {
        schema_version: SESSION_SCHEMA_VERSION,
        session_id: layout
            .run_directory
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        run_directory: layout.run_directory.clone(),
        workspace_path: workspace.to_path_buf(),
        spec_path: spec.spec_path.clone(),
        spec_slug: spec.name.clone(),
        branch,
        base_branch,
        base_sha: snapshot.head_sha.clone(),
        repository_root: snapshot.repository_root.clone(),
        started_at: started_at.clone(),
        updated_at: started_at,
        cycle: 0,
        last_attempt: 0,
        last_checkpoint_sha: head_sha,
        gate_status: "pending".to_string(),
        baseline_result: None,
        slice_index: None,
        open_finding_ids: None,
        chain_state_path: None,
    };

    if !skip_baseline {
        log_phase(
            "Running host baseline health checks",
            Some(&workspace.display().to_string()),
        );
        let baseline = run_baseline(workspace, spec).await?;
        let passed = baseline.passed;
        session.baseline_result = Some(baseline);
        if !passed {
            write_session(&session)?;
            let mut lines = vec![
                "Harness baseline health commands failed before generation.".to_string(),
                "These check the existing app (not the run acceptance gates).".to_string(),
            ];
            if let Some(result) = &session.baseline_result {
                for command in result.commands.iter().filter(|c| c.exit_code != Some(0)) {
                    lines.push(format!(
                        "- {}: exit {} ({})",
                        command.name,
                        command
                            .exit_code
                            .map_or_else(|| "null".to_string(), |c| c.to_string()),
                        command.command
                    ));
                }
            }
            return Err(refuse("baseline-failed", lines.join("\n")));
        }
    }
    write_session(&session)?;
    let snapshot = git::read_snapshot(workspace).await?;
    Ok(Prepared {
        mode: Mode::Start,
        layout,
        session,
        snapshot,
        starting_attempt: 1,
        cycle: None,
    })
}

/// `session.ts:341-420`.
async fn continue_session(
    workspace: &Path,
    loaded: &Loaded,
    snapshot: git::Snapshot,
) -> Result<Prepared, Error> {
    let spec = &loaded.spec;
    let branch = snapshot.branch.clone().unwrap_or_default();
    let Some(session) = find_matching_session(
        workspace,
        &branch,
        &spec.name,
        &spec.spec_path,
        &snapshot.repository_root,
    ) else {
        return Err(refuse(
            "unknown-harness-branch",
            format!(
                "Current branch {} looks like a harness run branch, but no matching local session metadata was found under .harness/runs/*/session.json. Refuse to create a nested branch. Use --new from a clean tree to start a fresh run, checkout a normal branch, or restore the matching session metadata before continuing.",
                serde_json::to_string(&branch).unwrap_or_default()
            ),
        ));
    };

    let dirty = git::filter_relevant(&snapshot.entries);
    if !dirty.is_empty() {
        let listed: Vec<String> = dirty
            .iter()
            .map(|entry| format!("{} {}", entry.code, entry.path))
            .collect();
        let mut preview = listed
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n");
        if listed.len() > 20 {
            preview.push_str(&format!("\n...and {} more", listed.len() - 20));
        }
        return Err(refuse(
            "interrupted-dirty",
            format!(
                "Harness session interrupted with uncommitted consumer changes.\nBranch: {branch}\nHEAD: {}\nLast checkpoint: {}\nSession: {}\n\nThe harness will not auto-commit potentially user-authored changes.\nRecover manually, then re-run on this branch:\n  git status\n  git diff\n  # discard unintended files, or commit intentional work\n  # then: hanvil run <spec>\n\nDirty paths:\n{preview}",
                snapshot.head_sha,
                session.last_checkpoint_sha,
                session_path(&session.run_directory).display()
            ),
        ));
    }
    if snapshot.head_sha != session.last_checkpoint_sha {
        return Err(refuse(
            "checkpoint-mismatch",
            format!(
                "Harness continue refused: HEAD does not match the session lastCheckpointSha.\nHEAD={}\nlastCheckpointSha={}\nIf you intentionally added commits, update or abandon the session before re-running.\nSession: {}",
                snapshot.head_sha,
                session.last_checkpoint_sha,
                session_path(&session.run_directory).display()
            ),
        ));
    }

    let layout = Layout::reopen(
        &session.run_directory,
        &spec.jsonl_log_path,
        &spec.notes_log_path,
    )?;
    let cycle = session.cycle + 1;
    let starting_attempt = session.last_attempt + 1;
    let spec_path = spec.spec_path.clone();
    let updated = update_session(&session.run_directory, |session| {
        session.cycle = cycle;
        session.spec_path = spec_path;
        session.gate_status = "pending".to_string();
    })?;
    Ok(Prepared {
        mode: Mode::Continue,
        layout,
        session: updated,
        snapshot,
        starting_attempt,
        cycle: Some(cycle),
    })
}

/// `session.ts:422-459`: the newest session on this branch for this spec.
pub(crate) fn find_matching_session(
    workspace: &Path,
    branch: &str,
    spec_slug: &str,
    spec_path: &Path,
    repository_root: &Path,
) -> Option<Session> {
    let runs = workspace.join(".harness").join("runs");
    let mut matches: Vec<Session> = std::fs::read_dir(runs)
        .ok()?
        .filter_map(Result::ok)
        .filter_map(|entry| read_session(&entry.path()))
        .filter(|session| session.branch == branch && session.spec_slug == spec_slug)
        .filter(|session| session.repository_root == repository_root)
        .filter(|session| session.workspace_path == workspace)
        .filter(|session| {
            // The spec may move; same slug, branch and repo are required, the exact path or
            // at least its basename preferred.
            session.spec_path == spec_path || session.spec_path.file_name() == spec_path.file_name()
        })
        .collect();
    matches.sort_by(|a, b| b.started_at.cmp(&a.started_at));
    matches.into_iter().next()
}

/// `session.ts:508-544`: stops at the first non-zero exit; output streams to the terminal.
async fn run_baseline(workspace: &Path, spec: &Spec) -> Result<BaselineResult, Error> {
    let mut commands = Vec::new();
    for entry in &spec.baseline {
        let name = entry
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&entry.command)
            .to_string();
        log_phase("Baseline command", Some(&name));
        let result = command::execute(Execute {
            command: &entry.command,
            args: &[],
            cwd: workspace,
            env: &std::collections::BTreeMap::new(),
            timeout: entry.timeout_ms.map(std::time::Duration::from_millis),
            shell: true,
            stream_output: true,
        })
        .await
        .map_err(|source| Error::Spawn {
            name: name.clone(),
            source,
        })?;
        log_phase(
            "Baseline command finished",
            Some(&format!(
                "{name} exit={} durationMs={}",
                result
                    .exit_code
                    .map_or_else(|| "null".to_string(), |c| c.to_string()),
                result.duration_ms
            )),
        );
        let failed = result.exit_code != Some(0);
        commands.push(BaselineCommand {
            name,
            command: entry.command.clone(),
            exit_code: result.exit_code,
            duration_ms: result.duration_ms,
            timed_out: result.timed_out,
        });
        if failed {
            return Ok(BaselineResult {
                passed: false,
                commands,
            });
        }
    }
    Ok(BaselineResult {
        passed: true,
        commands,
    })
}
