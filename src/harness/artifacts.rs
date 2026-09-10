//! Where a run writes. `runArtifacts.ts` and `runtimePaths.ts` of hedera-harness dev @ 587a2f3.
//!
//! Code stays in the workspace; prompts, logs, reports, cache and status live under
//! `.harness/runs/<timestamp>-<name>/`. The append-only `harness.log.jsonl` and the notes file
//! sit one level up so every run of a project shares them.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

/// `runtimePaths.ts:2-4`. Ignored session runtime, never committed.
pub(crate) const RUNTIME_DIR: &str = ".harness/runtime";
pub(crate) const SKILLS_DIR: &str = ".harness/runtime/skills";
pub(crate) const CONTEXT_DIR: &str = ".harness/runtime/context";
/// `runtimePaths.ts:7`. Cached hedera-skills clone.
pub(crate) const SKILL_CACHE_DIRNAME: &str = ".skill-cache";
/// `runtimePaths.ts:13-14`. Older layouts that cleanup, git and the secret scan still skip.
pub(crate) const ISOLATED_SKILLS_DIR: &str = ".harness-skills";
pub(crate) const ISOLATED_CONTEXT_DIR: &str = ".harness-context";

const LAYOUT_META_FILENAME: &str = "layout.json";
const LAYOUT_META_SCHEMA_VERSION: u64 = 1;
/// `runArtifacts.ts:6`.
const LAYOUT_MODE: &str = "in-place-run";

/// A file could not be written or read.
#[derive(Debug, thiserror::Error)]
#[error("{action} {path}: {source}")]
pub(crate) struct Error {
    /// `writing`, `reading`, `creating`.
    action: &'static str,
    /// The file or directory.
    path: PathBuf,
    /// The OS error.
    #[source]
    source: std::io::Error,
}

fn io_error<'a>(action: &'static str, path: &'a Path) -> impl FnOnce(std::io::Error) -> Error + 'a {
    move |source| Error {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// `runArtifacts.ts:19-30`.
#[derive(Debug, Clone)]
pub(crate) struct Layout {
    /// `.harness/runs/<id>`.
    pub(crate) run_directory: PathBuf,
    /// The project.
    pub(crate) workspace: PathBuf,
    /// `prompts/`.
    pub(crate) prompts_directory: PathBuf,
    /// `logs/`.
    pub(crate) logs_directory: PathBuf,
    /// `reports/`.
    pub(crate) reports_directory: PathBuf,
    /// `cache/`.
    pub(crate) cache_directory: PathBuf,
    /// `reports/report.json`.
    pub(crate) report_path: PathBuf,
    /// Shared across runs.
    pub(crate) jsonl_log_path: PathBuf,
    /// Shared across runs.
    pub(crate) notes_log_path: PathBuf,
}

impl Layout {
    /// `runArtifacts.ts:43-57`: a fresh `.harness/runs/<ISO timestamp with ':' and '.' → '-'>-<name>/`.
    pub(crate) fn create(
        workspace: &Path,
        spec_name: &str,
        jsonl_log_path: &Path,
        notes_log_path: &Path,
    ) -> Result<Self, Error> {
        let timestamp = now_iso8601().replace([':', '.'], "-");
        let run_directory = workspace
            .join(".harness")
            .join("runs")
            .join(format!("{timestamp}-{spec_name}"));
        Self::open_at(run_directory, workspace, jsonl_log_path, notes_log_path)
    }

    /// `runArtifacts.ts:60-78`: reopen an existing run directory for `--continue`.
    pub(crate) fn reopen(
        run_directory: &Path,
        jsonl_log_path: &Path,
        notes_log_path: &Path,
    ) -> Result<Self, Error> {
        let workspace = read_layout_meta(run_directory).unwrap_or_else(|| {
            // `.harness/runs/<id>` → project root.
            run_directory
                .parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        });
        std::fs::metadata(&workspace).map_err(io_error("reading", &workspace))?;
        Self::open_at(
            run_directory.to_path_buf(),
            &workspace,
            jsonl_log_path,
            notes_log_path,
        )
    }

    /// `runArtifacts.ts:206-245`.
    fn open_at(
        run_directory: PathBuf,
        workspace: &Path,
        jsonl_log_path: &Path,
        notes_log_path: &Path,
    ) -> Result<Self, Error> {
        let layout = Self {
            prompts_directory: run_directory.join("prompts"),
            logs_directory: run_directory.join("logs"),
            reports_directory: run_directory.join("reports"),
            cache_directory: run_directory.join("cache"),
            report_path: run_directory.join("reports").join("report.json"),
            run_directory,
            workspace: workspace.to_path_buf(),
            jsonl_log_path: jsonl_log_path.to_path_buf(),
            notes_log_path: notes_log_path.to_path_buf(),
        };
        for directory in [
            &layout.run_directory,
            &layout.prompts_directory,
            &layout.logs_directory,
            &layout.reports_directory,
            &layout.cache_directory,
        ] {
            std::fs::create_dir_all(directory).map_err(io_error("creating", directory))?;
        }
        for shared in [&layout.jsonl_log_path, &layout.notes_log_path] {
            if let Some(parent) = shared.parent() {
                std::fs::create_dir_all(parent).map_err(io_error("creating", parent))?;
            }
        }
        write_json_file(
            &layout.run_directory.join(LAYOUT_META_FILENAME),
            &serde_json::json!({
                "schemaVersion": LAYOUT_META_SCHEMA_VERSION,
                "mode": LAYOUT_MODE,
                "workspacePath": layout.workspace,
            }),
        )?;
        Ok(layout)
    }

    /// `session.json`.
    pub(crate) fn session_path(&self) -> PathBuf {
        self.run_directory.join("session.json")
    }

    /// `status.json`.
    pub(crate) fn status_path(&self) -> PathBuf {
        self.run_directory.join("status.json")
    }

    /// `runArtifacts.ts:196-204`: `status.json` with `updatedAt` first.
    pub(crate) fn write_status(&self, status: Value) -> Result<(), Error> {
        let mut object = serde_json::Map::new();
        object.insert("updatedAt".to_string(), Value::String(now_iso8601()));
        if let Value::Object(fields) = status {
            object.extend(fields);
        }
        write_json_file(&self.status_path(), &Value::Object(object))
    }

    /// `runArtifacts.ts:158-160`: one JSON object per line, `timestamp` added here.
    pub(crate) fn append_log(&self, event: &LogEvent) -> Result<(), Error> {
        let mut object = match serde_json::to_value(event) {
            Ok(Value::Object(object)) => object,
            _ => serde_json::Map::new(),
        };
        object.insert("timestamp".to_string(), Value::String(now_iso8601()));
        append_text(
            &self.jsonl_log_path,
            &format!("{}\n", Value::Object(object)),
        )
    }

    /// `runArtifacts.ts:162-169`.
    pub(crate) fn append_note(&self, title: &str, body: &str) -> Result<(), Error> {
        append_text(
            &self.notes_log_path,
            &format!("\n## {title}\n\n{}\n", body.trim()),
        )
    }
}

/// `runArtifacts.ts:80-98`: the workspace a run directory belongs to, when its metadata says.
pub(crate) fn read_layout_meta(run_directory: &Path) -> Option<PathBuf> {
    let raw = std::fs::read_to_string(run_directory.join(LAYOUT_META_FILENAME)).ok()?;
    let parsed: Value = serde_json::from_str(&raw).ok()?;
    if parsed.get("mode").and_then(Value::as_str) != Some(LAYOUT_MODE) {
        return None;
    }
    parsed
        .get("workspacePath")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// `runArtifacts.ts:274-298`: the newest `.harness/runs/<id>` whose metadata names this workspace.
pub(crate) fn latest_run_directory(workspace: &Path) -> Option<PathBuf> {
    let runs = workspace.join(".harness").join("runs");
    let mut matches: Vec<PathBuf> = std::fs::read_dir(runs)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|candidate| read_layout_meta(candidate).is_some_and(|ws| ws == workspace))
        .collect();
    matches.sort();
    matches.pop()
}

/// `runArtifacts.ts:142-156`: the highest N in `logs/*-attempt-N.*`.
pub(crate) fn last_attempt_number(logs_directory: &Path) -> u64 {
    std::fs::read_dir(logs_directory)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let (_, rest) = name.rsplit_once("-attempt-")?;
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let terminated = rest.len() == digits.len() || rest[digits.len()..].starts_with('.');
            if digits.is_empty() || !terminated {
                return None;
            }
            digits.parse::<u64>().ok()
        })
        .max()
        .unwrap_or(0)
}

/// `runArtifacts.ts:178-190`: persist a prompt for inspection with secrets replaced in the
/// on-disk copy only — the agent still receives the real prompt.
pub(crate) fn write_prompt_file(path: &Path, prompt: &str, secrets: &[&str]) -> Result<(), Error> {
    let mut persisted = prompt.trim().to_string();
    for secret in secrets {
        if !secret.is_empty() {
            persisted = persisted.replace(secret, "<redacted by hanvil>");
        }
    }
    std::fs::write(path, format!("{persisted}\n")).map_err(io_error("writing", path))
}

/// `runArtifacts.ts:192-194`: pretty JSON with a trailing newline.
pub(crate) fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), Error> {
    let json = serde_json::to_string_pretty(value).map_err(|source| Error {
        action: "serialising",
        path: path.to_path_buf(),
        source: std::io::Error::other(source),
    })?;
    std::fs::write(path, format!("{json}\n")).map_err(io_error("writing", path))
}

fn append_text(path: &Path, text: &str) -> Result<(), Error> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(io_error("writing", path))?;
    file.write_all(text.as_bytes())
        .map_err(io_error("writing", path))
}

/// `types.ts:340-465` plus the fork's two snapshot events and Hanvil's chain events. The
/// `type` tag and the field names are the JSON the TypeScript harness writes; CI scripts read
/// them.
#[derive(Debug, Clone, Serialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum LogEvent {
    RunStarted {
        spec_name: String,
        run_directory: PathBuf,
    },
    RunContinued {
        spec_name: String,
        run_directory: PathBuf,
        cycle: u64,
        starting_attempt: u64,
        max_attempts_this_cycle: u64,
    },
    CycleStarted {
        cycle: u64,
        starting_attempt: u64,
        max_attempts_this_cycle: u64,
    },
    ContinueStarted {
        attempt: u64,
        cycle: u64,
        prompt_path: PathBuf,
    },
    SkillsVendored {
        count: usize,
        workspace_skills_dir: PathBuf,
    },
    ContextVendored {
        prd_path: PathBuf,
        #[serde(skip_serializing_if = "Option::is_none")]
        eval_path: Option<PathBuf>,
        workspace_context_dir: PathBuf,
    },
    ChainSignerProvisioned {
        account_id: String,
        evm_address: String,
        network: String,
        reused: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        topped_up_hbar: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        replaced_deleted: Option<bool>,
    },
    ChainSignerSwept {
        account_id: String,
        success: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    ChainSnapshotTaken {
        attempt: u64,
        snapshot_id: String,
    },
    ChainSnapshotReverted {
        attempt: u64,
        snapshot_id: String,
        success: bool,
    },
    /// Hanvil: the attempt's chain was dumped for replay.
    ChainStateWritten {
        attempt: u64,
        path: PathBuf,
    },
    /// Hanvil: `chainValidation.assert` was evaluated.
    ChainAssertionsFinished {
        attempt: u64,
        passed: bool,
        finding_count: usize,
    },
    WorkspaceGitCommitted {
        attempt: u64,
        committed: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        commit_sha: Option<String>,
        message: String,
    },
    GeneratorStarted {
        attempt: u64,
        prompt_path: PathBuf,
    },
    GeneratorFinished {
        attempt: u64,
        exit_code: Option<i32>,
        duration_ms: u64,
        timed_out: bool,
    },
    ValidationFinished {
        attempt: u64,
        passed: bool,
        finding_count: usize,
        open_finding_ids: Vec<String>,
        fixed_finding_ids: Vec<String>,
        introduced_finding_ids: Vec<String>,
    },
    ValidatorStarted {
        attempt: u64,
        prompt_path: PathBuf,
        server_url: String,
    },
    ValidatorFinished {
        attempt: u64,
        passed: bool,
        finding_count: usize,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        infrastructure_failure: Option<bool>,
        #[serde(skip_serializing_if = "Option::is_none")]
        infrastructure_failure_reason: Option<String>,
    },
    ValidatorInfraAborted {
        attempt: u64,
        reason: String,
    },
    RepairStarted {
        attempt: u64,
        prompt_path: PathBuf,
    },
    RunFinished {
        passed: bool,
        attempts: u64,
        report_path: PathBuf,
    },
}

/// `new Date().toISOString()`: UTC with milliseconds.
pub(crate) fn now_iso8601() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    iso8601(now.as_secs(), now.subsec_millis())
}

/// Civil date from a Unix timestamp; Howard Hinnant's algorithm, no `chrono`.
pub(crate) fn iso8601(secs: u64, millis: u32) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hanvil-artifacts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn timestamps_are_iso8601() {
        assert_eq!(iso8601(0, 0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso8601(1_788_000_000, 7), "2026-08-29T10:40:00.007Z");
    }

    #[test]
    fn a_layout_creates_its_directories_and_metadata() {
        let ws = scratch("layout");
        let jsonl = ws.join(".harness/runs/harness.log.jsonl");
        let notes = ws.join(".harness/runs/harness-notes.md");
        let layout = Layout::create(&ws, "my-feature", &jsonl, &notes).expect("layout");
        assert!(layout.run_directory.starts_with(ws.join(".harness/runs")));
        let name = layout
            .run_directory
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        assert!(name.ends_with("-my-feature"), "{name}");
        assert!(
            !name.contains(':') && !name[..name.len() - 11].contains('.'),
            "{name}"
        );
        for directory in ["prompts", "logs", "reports", "cache"] {
            assert!(layout.run_directory.join(directory).is_dir());
        }
        assert_eq!(read_layout_meta(&layout.run_directory), Some(ws.clone()));
        assert_eq!(
            latest_run_directory(&ws),
            Some(layout.run_directory.clone())
        );
        let reopened = Layout::reopen(&layout.run_directory, &jsonl, &notes).expect("reopen");
        assert_eq!(reopened.workspace, ws);
        let _ = std::fs::remove_dir_all(ws);
    }

    #[test]
    fn logs_status_notes_and_prompts_are_written_in_upstream_shapes() {
        let ws = scratch("writes");
        let jsonl = ws.join("harness.log.jsonl");
        let notes = ws.join("harness-notes.md");
        let layout = Layout::create(&ws, "t", &jsonl, &notes).expect("layout");

        layout
            .append_log(&LogEvent::GeneratorFinished {
                attempt: 2,
                exit_code: None,
                duration_ms: 5,
                timed_out: true,
            })
            .expect("log");
        layout
            .append_log(&LogEvent::ChainSignerProvisioned {
                account_id: "0.0.1032".into(),
                evm_address: "0xab".into(),
                network: "local".into(),
                reused: false,
                topped_up_hbar: None,
                replaced_deleted: None,
            })
            .expect("log");
        let lines: Vec<Value> = std::fs::read_to_string(&jsonl)
            .expect("jsonl")
            .lines()
            .map(|line| serde_json::from_str(line).expect("json line"))
            .collect();
        assert_eq!(lines[0]["type"], "generator_finished");
        assert_eq!(lines[0]["exitCode"], Value::Null);
        assert_eq!(lines[0]["timedOut"], true);
        assert!(
            lines[0]["timestamp"]
                .as_str()
                .is_some_and(|t| t.ends_with('Z'))
        );
        assert_eq!(lines[1]["type"], "chain_signer_provisioned");
        assert_eq!(lines[1]["network"], "local");
        assert!(lines[1].get("toppedUpHbar").is_none());

        layout
            .write_status(serde_json::json!({"phase": "validated", "attempt": 1}))
            .expect("status");
        let status = std::fs::read_to_string(layout.status_path()).expect("status");
        let parsed: Value = serde_json::from_str(&status).expect("status json");
        assert_eq!(parsed["phase"], "validated");
        assert_eq!(parsed["attempt"], 1);
        assert!(
            parsed["updatedAt"]
                .as_str()
                .is_some_and(|t| t.ends_with('Z'))
        );
        assert!(
            status.starts_with("{\n  \"") && status.contains("\n  \"updatedAt\": \""),
            "{status}"
        );
        assert!(status.ends_with("}\n"));

        layout
            .append_note("Attempt 1 validation", "  body  \n")
            .expect("note");
        assert_eq!(
            std::fs::read_to_string(&notes).expect("notes"),
            "\n## Attempt 1 validation\n\nbody\n"
        );

        let prompt = layout.prompts_directory.join("validator-attempt-1.txt");
        write_prompt_file(&prompt, "  key 0xdead and again 0xdead  ", &["0xdead", ""])
            .expect("prompt");
        assert_eq!(
            std::fs::read_to_string(&prompt).expect("prompt"),
            "key <redacted by hanvil> and again <redacted by hanvil>\n"
        );

        for name in [
            "generator-attempt-3.log",
            "validation-attempt-12.json",
            "x-attempt-9z.log",
            "other.txt",
        ] {
            std::fs::write(layout.logs_directory.join(name), "").expect("touch");
        }
        assert_eq!(last_attempt_number(&layout.logs_directory), 12);
        assert_eq!(last_attempt_number(&ws.join("missing")), 0);
        let _ = std::fs::remove_dir_all(ws);
    }
}
