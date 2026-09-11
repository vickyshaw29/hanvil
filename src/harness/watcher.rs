//! Watching the workspace while the generator agent works. `workspaceWatcher.ts` of
//! hedera-harness dev @ 587a2f3.
//!
//! Upstream uses Node's recursive `fs.watch`. This polls the tree instead: the crates that wrap
//! inotify and FSEvents sit outside the licence set in `.claude/CLAUDE.md` §4, and a dependency
//! is a lot to carry for one log file. The consequence is written down rather than hidden — a
//! file created and deleted between two polls is never seen, where `fs.watch` would have caught
//! it. Anything still on disk when the agent stops is recorded, because `stop` scans once more
//! before it writes the total.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use crate::harness::artifacts::{
    self, ISOLATED_CONTEXT_DIR, ISOLATED_SKILLS_DIR, SKILL_CACHE_DIRNAME, now_iso8601,
};

/// `workspaceWatcher.ts:5-15`.
const IGNORED_SEGMENTS: [&str; 9] = [
    "node_modules",
    ".next",
    ".git",
    "dist",
    "artifacts",
    "cache",
    ISOLATED_SKILLS_DIR,
    ISOLATED_CONTEXT_DIR,
    SKILL_CACHE_DIRNAME,
];

/// `workspaceWatcher.ts:28-32`.
const HEADER: &str =
    "# workspace activity log\n# file changes while the generator agent is running\n";

/// How often the tree is walked. A generator runs for minutes and the ignored directories prune
/// everything expensive, so this is cheap; it is the resolution of the console feedback, not of
/// the log, which `stop` completes.
const POLL: Duration = Duration::from_millis(500);

/// Called with each `FILE <path>` summary, so the caller can keep `status.json` current.
/// `attemptStages.ts:105-108`.
pub(crate) type ChangeSink = Arc<dyn Fn(&str) + Send + Sync>;

/// What a file looked like at the last walk.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// `workspaceWatcher.ts:68-78`. The path is relative to the workspace root and separated by
/// `/` on every platform.
fn should_ignore(relative: &str) -> bool {
    let normalized = relative.replace('\\', "/");
    if normalized == ".harness"
        || normalized.starts_with(".harness/")
        || normalized.contains("/.harness/")
    {
        return true;
    }
    normalized
        .split('/')
        .any(|segment| IGNORED_SEGMENTS.contains(&segment))
}

/// Every file under `workspace` that is not ignored, keyed by its relative path. Unreadable
/// entries are skipped: a watcher that fails a run because one directory is not listable would
/// be worse than one that misses a line. Symlinks are stamped, never descended into, so a link
/// pointing at an ancestor cannot loop.
fn scan(workspace: &Path) -> BTreeMap<String, Stamp> {
    let mut found = BTreeMap::new();
    let mut queue = vec![(workspace.to_path_buf(), String::new())];
    while let Some((directory, prefix)) = queue.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if should_ignore(&relative) {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                queue.push((entry.path(), relative));
            } else if let Ok(metadata) = entry.metadata() {
                found.insert(
                    relative,
                    Stamp {
                        modified: metadata.modified().ok(),
                        len: metadata.len(),
                    },
                );
            }
        }
    }
    found
}

/// Paths that appeared, vanished, or changed length or mtime, in path order.
fn changes(previous: &BTreeMap<String, Stamp>, current: &BTreeMap<String, Stamp>) -> Vec<String> {
    let mut changed: Vec<String> = current
        .iter()
        .filter(|(relative, stamp)| previous.get(*relative) != Some(stamp))
        .map(|(relative, _)| relative.clone())
        .collect();
    changed.extend(
        previous
            .keys()
            .filter(|relative| !current.contains_key(*relative))
            .cloned(),
    );
    changed.sort();
    changed.dedup();
    changed
}

/// The log handle and the running count, shared by the polling task and `stop`.
struct State {
    previous: BTreeMap<String, Stamp>,
    changes: u64,
    log: std::fs::File,
    path: PathBuf,
    on_change: Option<ChangeSink>,
}

impl State {
    /// `workspaceWatcher.ts:52-66`: one line per change, the first twenty on the console and
    /// every twenty-fifth after that.
    fn record(&mut self, current: BTreeMap<String, Stamp>) {
        for relative in changes(&self.previous, &current) {
            self.changes += 1;
            let summary = format!("FILE {relative}");
            self.append(&format!("{} {summary}\n", now_iso8601()));
            if self.changes <= 20 || self.changes.is_multiple_of(25) {
                println!("[hanvil:workspace] {summary}");
            }
            if let Some(sink) = &self.on_change {
                sink(&summary);
            }
        }
        self.previous = current;
    }

    fn append(&mut self, line: &str) {
        if self.log.write_all(line.as_bytes()).is_err() {
            eprintln!("[hanvil] could not append to {}", self.path.display());
        }
    }
}

fn with_lock<T, R>(mutex: &Mutex<T>, f: impl FnOnce(&mut T) -> R) -> R {
    match mutex.lock() {
        Ok(mut guard) => f(&mut guard),
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

/// `workspaceWatcher.ts:17-45`.
pub(crate) struct WorkspaceWatcher {
    workspace: PathBuf,
    state: Arc<Mutex<State>>,
    task: tokio::task::JoinHandle<()>,
}

impl WorkspaceWatcher {
    /// Truncate the log, write its header, take the baseline walk, and start polling.
    pub(crate) fn start(
        workspace: &Path,
        log_path: &Path,
        on_change: Option<ChangeSink>,
    ) -> Result<Self, artifacts::Error> {
        let log = artifacts::create_log(log_path, HEADER)?;
        let state = Arc::new(Mutex::new(State {
            previous: scan(workspace),
            changes: 0,
            log,
            path: log_path.to_path_buf(),
            on_change,
        }));
        let task = tokio::spawn({
            let workspace = workspace.to_path_buf();
            let state = Arc::clone(&state);
            async move {
                loop {
                    tokio::time::sleep(POLL).await;
                    // The same runtime answers the app's JSON-RPC calls; a walk of a large
                    // workspace has no business sitting on one of its worker threads.
                    let walk = {
                        let workspace = workspace.clone();
                        tokio::task::spawn_blocking(move || scan(&workspace))
                    };
                    if let Ok(current) = walk.await {
                        with_lock(&state, |state| state.record(current));
                    }
                }
            }
        });
        Ok(Self {
            workspace: workspace.to_path_buf(),
            state,
            task,
        })
    }

    /// `workspaceWatcher.ts:26-35`. Both of the task's suspension points — the sleep and the
    /// walk — sit outside the lock, and `record` never awaits, so an abort cannot cut a
    /// half-written line. The last walk happens here instead, ordered before the total.
    pub(crate) async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
        let current = scan(&self.workspace);
        with_lock(&self.state, |state| {
            state.record(current);
            let total = state.changes;
            state.append(&format!(
                "{} WATCHER stopped totalChanges={total}\n",
                now_iso8601()
            ));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignored_segments_match_upstream_at_every_depth() {
        for ignored in [
            "node_modules/react/index.js",
            "app/node_modules/x",
            ".git/HEAD",
            ".next/build",
            "dist/main.js",
            "artifacts/Counter.json",
            "cache/solidity-files-cache.json",
            ".harness",
            ".harness/spec.yaml",
            "nested/.harness/runs/status.json",
            ".harness-skills/a",
            ".harness-context/prd.md",
            ".skill-cache/repo",
            "a\\node_modules\\b",
        ] {
            assert!(should_ignore(ignored), "{ignored} should be ignored");
        }
        for kept in [
            "generated.txt",
            "src/app/page.tsx",
            "scripts/seed.js",
            "harness.md",
            "my-artifacts/x",
            "cached/x",
        ] {
            assert!(!should_ignore(kept), "{kept} should be recorded");
        }
    }

    #[test]
    fn a_scan_prunes_the_ignored_directories_and_keeps_the_rest() {
        let root = std::env::temp_dir().join(format!("hanvil-watch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).expect("src");
        std::fs::create_dir_all(root.join("node_modules/react")).expect("node_modules");
        std::fs::create_dir_all(root.join(".harness/runs")).expect(".harness");
        std::fs::write(root.join("generated.txt"), "ok").expect("write");
        std::fs::write(root.join("src/page.tsx"), "ok").expect("write");
        std::fs::write(root.join("node_modules/react/index.js"), "ok").expect("write");
        std::fs::write(root.join(".harness/runs/status.json"), "{}").expect("write");

        let found = scan(&root);
        assert_eq!(
            found.keys().cloned().collect::<Vec<_>>(),
            vec!["generated.txt".to_string(), "src/page.tsx".to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_diff_reports_additions_removals_and_rewrites_in_path_order() {
        let stamp = |len| Stamp {
            modified: Some(SystemTime::UNIX_EPOCH),
            len,
        };
        let previous = BTreeMap::from([
            ("kept.txt".to_string(), stamp(1)),
            ("rewritten.txt".to_string(), stamp(1)),
            ("removed.txt".to_string(), stamp(1)),
        ]);
        let current = BTreeMap::from([
            ("kept.txt".to_string(), stamp(1)),
            ("rewritten.txt".to_string(), stamp(2)),
            ("added.txt".to_string(), stamp(1)),
        ]);
        assert_eq!(
            changes(&previous, &current),
            vec![
                "added.txt".to_string(),
                "removed.txt".to_string(),
                "rewritten.txt".to_string()
            ]
        );
        assert!(changes(&current, &current).is_empty());
    }

    #[tokio::test]
    async fn a_file_written_while_the_watcher_runs_reaches_the_log_and_the_sink() {
        let root = std::env::temp_dir().join(format!("hanvil-watch-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");
        // The log lives outside the tree being walked, as it does in a run: the run directory
        // is under `.harness/`, which the ignore rules prune.
        let log_path = root.join("workspace-attempt-1.activity.log");
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink: ChangeSink = {
            let seen = Arc::clone(&seen);
            Arc::new(move |summary: &str| {
                with_lock(&seen, |lines| lines.push(summary.to_string()));
            })
        };

        let watcher =
            WorkspaceWatcher::start(&workspace, &log_path, Some(sink)).expect("watcher starts");
        std::fs::write(workspace.join("generated.txt"), "ok").expect("write");
        watcher.stop().await;

        let log = std::fs::read_to_string(&log_path).expect("log");
        assert!(log.starts_with(HEADER), "{log}");
        assert!(log.contains(" FILE generated.txt\n"), "{log}");
        assert!(
            log.trim_end().ends_with("WATCHER stopped totalChanges=1"),
            "{log}"
        );
        assert!(
            with_lock(&seen, |lines| lines
                .contains(&"FILE generated.txt".to_string())),
            "the sink never heard about the file"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
