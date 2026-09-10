//! The repository the harness works in. `harnessGit.ts` and `branchDetection.ts` of
//! hedera-harness dev @ 587a2f3. Everything goes through the `git` binary, as upstream does.
//!
//! The harness never pushes, opens a PR, merges, deletes a branch, switches away, or stashes.
//! Checkpoint commits stage explicit paths — never `git add -A` — and refuse runtime and
//! secret paths.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;

use rand_core::RngCore as _;
use regex::Regex;

use crate::harness::artifacts::{ISOLATED_CONTEXT_DIR, ISOLATED_SKILLS_DIR, SKILL_CACHE_DIRNAME};
use crate::harness::findings::Finding;

/// `branchDetection.ts:2`.
pub(crate) const RUN_BRANCH_PREFIX: &str = "harness/run-";

/// `harnessGit.ts:22-36`. Paths that must never be committed and must not block a continue.
const RUNTIME_PATH_PREFIXES: [&str; 13] = [
    ".harness/runs/",
    ".harness/cache/",
    ".harness/runtime/",
    ".harness-skills/",
    ".harness-context/",
    ".skill-cache/",
    "node_modules/",
    "dist/",
    "build/",
    "coverage/",
    ".next/",
    "playwright-report/",
    "test-results/",
];

/// `harnessGit.ts:38-53`.
const RUNTIME_PATH_NAMES: [&str; 14] = [
    ".harness/runs",
    ".harness/cache",
    ".harness/runtime",
    ISOLATED_SKILLS_DIR,
    ISOLATED_CONTEXT_DIR,
    SKILL_CACHE_DIRNAME,
    "node_modules",
    "dist",
    "build",
    "coverage",
    ".next",
    "playwright-report",
    "test-results",
    "chain-signer.json",
];

/// `harnessGit.ts:56-66`. Secret and credential paths a checkpoint never stages.
static SECRET_PATH_MARKERS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?i)^\.env(\.|$)",
        r"(?i)(^|/)\.env(\.|$)",
        r"(?i)(^|/)secrets?/",
        r"(?i)(^|/)chain-signer\.json$",
        r"(?i)\.pem$",
        r"(?i)\.key$",
        r"(?i)(^|/)credentials\.json$",
        r"(?i)(^|/)service-account.*\.json$",
        r"(?i)(^|/)\.cursor/mcp\.json$",
    ]
    .iter()
    .filter_map(|pattern| Regex::new(pattern).ok())
    .collect()
});

/// A `git` invocation failed or the repository is not what a run needs.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `harnessGit.ts:155`.
    #[error("Not a git repository: {0}")]
    NotARepository(PathBuf),
    /// `command.ts:199-205` for a `git` command.
    #[error("Command \"git {args}\" {reason}")]
    Command {
        /// Joined arguments.
        args: String,
        /// `exited with code N: stderr` or `exited with code N.`
        reason: String,
    },
    /// The binary could not be started.
    #[error("running git: {0}")]
    Spawn(#[source] std::io::Error),
    /// `harnessGit.ts:273-279`.
    #[error("{0}")]
    DirtyTree(String),
    /// `harnessGit.ts:347`.
    #[error("Harness checkpoint refused unsafe path: {0}")]
    UnsafePath(String),
    /// `harnessGit.ts:393-398`.
    #[error("{0}")]
    UnsafeIndex(String),
}

/// `harnessGit.ts:68-73`. One `git status --porcelain=v1` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    /// XY status codes, e.g. ` M`, `??`.
    pub(crate) code: String,
    /// The path, or the destination of a rename.
    pub(crate) path: String,
    /// The source of a rename or copy.
    pub(crate) orig_path: Option<String>,
}

/// `harnessGit.ts:75-82`.
#[derive(Debug, Clone)]
pub(crate) struct Snapshot {
    /// `git rev-parse --show-toplevel`.
    pub(crate) repository_root: PathBuf,
    /// `git branch --show-current`, `None` when detached or unborn.
    pub(crate) branch: Option<String>,
    /// `git rev-parse HEAD`.
    pub(crate) head_sha: String,
    /// `git symbolic-ref -q HEAD` failed.
    pub(crate) detached: bool,
    /// `merge`, `rebase`, `cherry-pick`, `revert`, `bisect`.
    pub(crate) in_progress_operation: Option<&'static str>,
    /// The working tree.
    pub(crate) entries: Vec<Entry>,
}

/// Run `git` and return trimmed stdout. Non-zero exit is an error carrying stderr.
pub(crate) async fn git(args: &[&str], cwd: &Path) -> Result<String, Error> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(Error::Spawn)?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let code = output
        .status
        .code()
        .map_or_else(|| "null".to_string(), |c| c.to_string());
    Err(Error::Command {
        args: args.join(" "),
        reason: if stderr.is_empty() {
            format!("exited with code {code}.")
        } else {
            format!("exited with code {code}: {stderr}")
        },
    })
}

/// `git` where a non-zero exit is an answer, not an error.
async fn git_ok(args: &[&str], cwd: &Path) -> Option<String> {
    git(args, cwd).await.ok()
}

/// `harnessGit.ts:148-158`.
pub(crate) async fn repository_root(cwd: &Path) -> Result<PathBuf, Error> {
    match git_ok(&["rev-parse", "--show-toplevel"], cwd).await {
        Some(root) if !root.trim().is_empty() => Ok(PathBuf::from(root.trim())),
        _ => Err(Error::NotARepository(cwd.to_path_buf())),
    }
}

/// `harnessGit.ts:177-184`.
pub(crate) async fn head_sha(cwd: &Path) -> Result<String, Error> {
    Ok(git(&["rev-parse", "HEAD"], cwd).await?.trim().to_string())
}

/// `harnessGit.ts:186-197`.
pub(crate) async fn current_branch(cwd: &Path) -> Option<String> {
    git_ok(&["branch", "--show-current"], cwd)
        .await
        .map(|branch| branch.trim().to_string())
        .filter(|branch| !branch.is_empty())
}

/// `harnessGit.ts:199-206`.
pub(crate) async fn is_detached_head(cwd: &Path) -> bool {
    git_ok(&["symbolic-ref", "-q", "HEAD"], cwd).await.is_none()
}

/// `harnessGit.ts:208-238`.
pub(crate) async fn in_progress_operation(cwd: &Path) -> Option<&'static str> {
    let git_dir = git_ok(&["rev-parse", "--git-dir"], cwd)
        .await
        .map(|dir| cwd.join(dir.trim()))?;
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

/// `harnessGit.ts:240-262`.
pub(crate) async fn working_tree_entries(cwd: &Path) -> Result<Vec<Entry>, Error> {
    let stdout = git(&["status", "--porcelain=v1", "-uall"], cwd).await?;
    Ok(parse_porcelain(&stdout))
}

/// The porcelain v1 lines: two status characters, a space, then the path; renames and copies
/// carry `from -> to`.
pub(crate) fn parse_porcelain(stdout: &str) -> Vec<Entry> {
    stdout
        .lines()
        .filter(|line| !line.trim().is_empty() && line.len() >= 3)
        .map(|line| {
            let code = line[..2].to_string();
            let rest = &line[3..];
            if (code.starts_with('R') || code.starts_with('C'))
                && let Some((from, to)) = rest.split_once(" -> ")
                && !from.is_empty()
                && !to.is_empty()
            {
                return Entry {
                    code,
                    path: to.to_string(),
                    orig_path: Some(from.to_string()),
                };
            }
            Entry {
                code,
                path: rest.to_string(),
                orig_path: None,
            }
        })
        .collect()
}

/// `harnessGit.ts:160-175`.
pub(crate) async fn read_snapshot(cwd: &Path) -> Result<Snapshot, Error> {
    let root = repository_root(cwd).await?;
    Ok(Snapshot {
        head_sha: head_sha(&root).await?,
        branch: current_branch(&root).await,
        detached: is_detached_head(&root).await,
        in_progress_operation: in_progress_operation(&root).await,
        entries: working_tree_entries(&root).await?,
        repository_root: root,
    })
}

fn normalise(relative: &str) -> String {
    let forward = relative.replace('\\', "/");
    forward
        .strip_prefix("./")
        .map_or(forward.clone(), str::to_string)
}

/// `harnessGit.ts:84-92`.
pub(crate) fn is_runtime_path(relative: &str) -> bool {
    let normalised = normalise(relative);
    RUNTIME_PATH_NAMES.contains(&normalised.as_str())
        || RUNTIME_PATH_PREFIXES.iter().any(|prefix| {
            normalised == prefix[..prefix.len() - 1] || normalised.starts_with(prefix)
        })
}

/// `harnessGit.ts:94-97`.
pub(crate) fn is_secret_path(relative: &str) -> bool {
    let normalised = normalise(relative);
    SECRET_PATH_MARKERS
        .iter()
        .any(|pattern| pattern.is_match(&normalised))
}

/// `harnessGit.ts:99-109`: drop runtime paths and harness-injected MCP churn.
pub(crate) fn filter_relevant(entries: &[Entry]) -> Vec<Entry> {
    entries
        .iter()
        .filter(|entry| {
            !is_runtime_path(&entry.path)
                && !entry.orig_path.as_deref().is_some_and(is_runtime_path)
                && entry.path != ".cursor/mcp.json"
                && !entry.path.ends_with("/.cursor/mcp.json")
        })
        .cloned()
        .collect()
}

/// `harnessGit.ts:112-126`: what a checkpoint may stage, and the secrets it left out.
pub(crate) fn filter_commitable(entries: &[Entry]) -> (Vec<Entry>, Vec<Entry>) {
    filter_relevant(entries).into_iter().partition(|entry| {
        !is_secret_path(&entry.path) && !entry.orig_path.as_deref().is_some_and(is_secret_path)
    })
}

/// `harnessGit.ts:264-280`.
pub(crate) async fn assert_clean_for_run_start(cwd: &Path) -> Result<(), Error> {
    let entries = filter_relevant(&working_tree_entries(cwd).await?);
    if entries.is_empty() {
        return Ok(());
    }
    let mut preview = entries
        .iter()
        .take(12)
        .map(|entry| format!("{} {}", entry.code, entry.path))
        .collect::<Vec<_>>()
        .join("\n");
    if entries.len() > 12 {
        preview.push_str(&format!("\n...and {} more", entries.len() - 12));
    }
    Err(Error::DirtyTree(format!(
        "Harness run requires a completely clean working tree (no auto-stash).\nCommit or discard local changes, then re-run.\n{preview}"
    )))
}

/// `branchDetection.ts:4-12`.
pub(crate) fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for ch in value.trim().to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(ch);
        } else {
            pending_dash = true;
        }
    }
    let slug: String = slug.chars().take(48).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "feature".to_string()
    } else {
        slug
    }
}

/// `branchDetection.ts:16-21`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedBranch {
    /// Between the prefix and the trailing id.
    pub(crate) spec_slug: String,
    /// Hex.
    pub(crate) short_id: String,
}

/// `branchDetection.ts:43-45`.
pub(crate) fn is_harness_branch(branch: Option<&str>) -> bool {
    branch.is_some_and(|b| b.starts_with(RUN_BRANCH_PREFIX))
}

/// `branchDetection.ts:51-67`: `harness/run-<slug>-<hex>`.
pub(crate) fn parse_harness_branch(branch: Option<&str>) -> Option<ParsedBranch> {
    let rest = branch?.strip_prefix(RUN_BRANCH_PREFIX)?;
    let last_dash = rest.rfind('-')?;
    if last_dash == 0 || last_dash == rest.len() - 1 {
        return None;
    }
    let short_id = &rest[last_dash + 1..];
    let spec_slug = &rest[..last_dash];
    if spec_slug.is_empty() || !short_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(ParsedBranch {
        spec_slug: spec_slug.to_string(),
        short_id: short_id.to_string(),
    })
}

/// `branchDetection.ts:36-40`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BranchDecision {
    /// Resume on the current or named harness branch.
    Continue {
        /// Why.
        reason: String,
    },
    /// Create a fresh harness branch.
    New {
        /// Why.
        reason: String,
    },
}

/// `branchDetection.ts:85-121`.
pub(crate) fn decide_branch_action(
    current_branch: Option<&str>,
    spec_name: &str,
    force_new: bool,
    continue_branch: Option<&str>,
) -> BranchDecision {
    if let Some(explicit) = continue_branch.map(str::trim).filter(|b| !b.is_empty()) {
        return BranchDecision::Continue {
            reason: format!(
                "explicit --continue {}",
                serde_json::to_string(explicit).unwrap_or_default()
            ),
        };
    }
    if force_new {
        return BranchDecision::New {
            reason: "explicit --new".to_string(),
        };
    }
    let Some(parsed) = parse_harness_branch(current_branch) else {
        return BranchDecision::New {
            reason: match current_branch {
                Some(branch) => format!(
                    "current branch {} is not a harness branch",
                    serde_json::to_string(branch).unwrap_or_default()
                ),
                None => "no current branch".to_string(),
            },
        };
    };
    let expected = slugify(spec_name);
    if parsed.spec_slug == expected {
        BranchDecision::Continue {
            reason: format!(
                "current harness branch matches spec slug {}",
                serde_json::to_string(&expected).unwrap_or_default()
            ),
        }
    } else {
        BranchDecision::New {
            reason: format!(
                "current harness branch slug {} differs from spec {}",
                serde_json::to_string(&parsed.spec_slug).unwrap_or_default(),
                serde_json::to_string(&expected).unwrap_or_default()
            ),
        }
    }
}

/// `branchDetection.ts:123-125`.
pub(crate) fn run_branch_name(spec_slug: &str, short_id: &str) -> String {
    format!("{RUN_BRANCH_PREFIX}{}-{short_id}", slugify(spec_slug))
}

/// `randomBytes(3).toString("hex")`.
fn short_id() -> String {
    let mut bytes = [0u8; 3];
    rand_core::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// `harnessGit.ts:286-299`: create and check out `harness/run-<slug>-<id>` from HEAD. The
/// caller has already asserted a clean tree on an attached branch.
pub(crate) async fn create_and_checkout_branch(
    cwd: &Path,
    spec_slug: &str,
) -> Result<(String, String), Error> {
    let branch = run_branch_name(spec_slug, &short_id());
    git(&["checkout", "-b", &branch], cwd).await?;
    let sha = head_sha(cwd).await?;
    Ok((branch, sha))
}

/// `git checkout <branch>`.
pub(crate) async fn checkout(cwd: &Path, branch: &str) -> Result<(), Error> {
    git(&["checkout", branch], cwd).await.map(|_| ())
}

/// `harnessGit.ts:128-146`.
pub(crate) fn attempt_commit_message(
    attempt: u64,
    passed: bool,
    findings: &[Finding],
) -> (String, String) {
    let subject = format!(
        "harness: run attempt {attempt} {}",
        if passed { "passed" } else { "failed" }
    );
    let ids: Vec<&str> = findings
        .iter()
        .map(|finding| finding.id.as_str())
        .filter(|id| !id.is_empty())
        .take(30)
        .collect();
    let mut body = vec![format!("{} finding(s).", findings.len())];
    if !ids.is_empty() {
        body.push(format!("Finding IDs: {}", ids.join(", ")));
    }
    if findings.len() > 30 {
        body.push(format!("…and {} more", findings.len() - 30));
    }
    body.push(String::new());
    body.push("Created by hanvil run. Optional: squash attempt commits before merge.".to_string());
    (subject, body.join("\n"))
}

/// `harnessGit.ts:301-306`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checkpoint {
    /// A commit was made.
    pub(crate) committed: bool,
    /// Its sha.
    pub(crate) commit_sha: Option<String>,
    /// The subject.
    pub(crate) message: String,
    /// Secret paths that were dirty and left unstaged.
    pub(crate) skipped_secrets: Vec<String>,
}

/// `harnessGit.ts:313-374`: commit consumer-relevant changes for an attempt with the consumer
/// repo's own identity and hooks.
pub(crate) async fn commit_attempt(
    workspace: &Path,
    attempt: u64,
    passed: bool,
    findings: &[Finding],
) -> Result<Checkpoint, Error> {
    let (subject, body) = attempt_commit_message(attempt, passed, findings);
    let (commitable, skipped) = filter_commitable(&working_tree_entries(workspace).await?);
    let skipped_secrets: Vec<String> = skipped.into_iter().map(|entry| entry.path).collect();
    if commitable.is_empty() {
        return Ok(Checkpoint {
            committed: false,
            commit_sha: None,
            message: subject,
            skipped_secrets,
        });
    }
    for entry in &commitable {
        if Path::new(&entry.path).is_absolute()
            || entry.path.contains('\0')
            || entry.path.starts_with("..")
        {
            return Err(Error::UnsafePath(
                serde_json::to_string(&entry.path).unwrap_or_default(),
            ));
        }
    }
    let mut add: Vec<&str> = vec!["add", "--"];
    add.extend(commitable.iter().map(|entry| entry.path.as_str()));
    git(&add, workspace).await?;
    assert_index_safe(workspace).await?;
    git(&["commit", "-m", &subject, "-m", &body], workspace).await?;
    Ok(Checkpoint {
        committed: true,
        commit_sha: Some(head_sha(workspace).await?),
        message: subject,
        skipped_secrets,
    })
}

/// `harnessGit.ts:376-400`: the index must not hold runtime or secret paths. Best-effort
/// unstage, then fail hard.
async fn assert_index_safe(cwd: &Path) -> Result<(), Error> {
    let staged = git(&["diff", "--cached", "--name-only", "-z"], cwd).await?;
    let unsafe_paths: Vec<&str> = staged
        .split('\0')
        .map(str::trim)
        .filter(|path| !path.is_empty() && (is_runtime_path(path) || is_secret_path(path)))
        .collect();
    if unsafe_paths.is_empty() {
        return Ok(());
    }
    let mut reset: Vec<&str> = vec!["reset", "HEAD", "--"];
    reset.extend(unsafe_paths.iter().copied());
    let _ = git_ok(&reset, cwd).await;
    let mut lines =
        vec!["Harness checkpoint aborted: staged files included runtime/secret paths.".to_string()];
    lines.extend(unsafe_paths.iter().map(|path| format!("- {path}")));
    Err(Error::UnsafeIndex(lines.join("\n")))
}

/// `harnessGit.ts:403-409`: consumer-relevant dirty paths after runtime filtering; secrets
/// left uncommitted still count as not clean.
pub(crate) async fn consumer_dirty_paths(cwd: &Path) -> Result<Vec<String>, Error> {
    let (commitable, skipped) = filter_commitable(&working_tree_entries(cwd).await?);
    Ok(commitable
        .into_iter()
        .chain(skipped)
        .map(|entry| entry.path)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::findings::Category;

    #[test]
    fn slugs_and_branch_names_follow_upstream() {
        assert_eq!(slugify("  My Feature: v2!  "), "my-feature-v2");
        assert_eq!(slugify("---"), "feature");
        assert_eq!(slugify(&"a".repeat(60)).len(), 48);
        assert_eq!(
            run_branch_name("My Feature", "ab12cd"),
            "harness/run-my-feature-ab12cd"
        );
        assert!(is_harness_branch(Some("harness/run-x-1")));
        assert!(!is_harness_branch(Some("main")));
        assert_eq!(
            parse_harness_branch(Some("harness/run-my-feature-ab12cd")),
            Some(ParsedBranch {
                spec_slug: "my-feature".into(),
                short_id: "ab12cd".into()
            })
        );
        assert_eq!(parse_harness_branch(Some("harness/run-nodash")), None);
        assert_eq!(parse_harness_branch(Some("harness/run-x-zz")), None);
        assert_eq!(parse_harness_branch(Some("harness/run-x-")), None);
        assert_eq!(parse_harness_branch(Some("main")), None);
    }

    #[test]
    fn the_branch_decision_table_is_upstreams() {
        let cont = |reason: &str| BranchDecision::Continue {
            reason: reason.to_string(),
        };
        let new = |reason: &str| BranchDecision::New {
            reason: reason.to_string(),
        };
        assert_eq!(
            decide_branch_action(Some("main"), "x", false, Some(" harness/run-x-1 ")),
            cont("explicit --continue \"harness/run-x-1\"")
        );
        assert_eq!(
            decide_branch_action(Some("harness/run-x-1"), "x", true, None),
            new("explicit --new")
        );
        assert_eq!(
            decide_branch_action(Some("main"), "x", false, None),
            new("current branch \"main\" is not a harness branch")
        );
        assert_eq!(
            decide_branch_action(None, "x", false, None),
            new("no current branch")
        );
        assert_eq!(
            decide_branch_action(Some("harness/run-my-feature-ab"), "My Feature", false, None),
            cont("current harness branch matches spec slug \"my-feature\"")
        );
        assert_eq!(
            decide_branch_action(Some("harness/run-other-ab"), "My Feature", false, None),
            new("current harness branch slug \"other\" differs from spec \"my-feature\"")
        );
    }

    #[test]
    fn every_secret_marker_compiles() {
        // A pattern the regex crate cannot build is silently absent from the list, which
        // would let a secret through; the count pins all nine.
        assert_eq!(SECRET_PATH_MARKERS.len(), 9);
    }

    #[test]
    fn runtime_and_secret_paths_are_recognised() {
        for path in [
            ".harness/runs/x/y",
            ".harness/runs",
            "./node_modules/a",
            "dist",
            ".skill-cache/z",
            "chain-signer.json",
        ] {
            assert!(is_runtime_path(path), "{path}");
        }
        for path in ["src/a.ts", ".harness/spec.yaml", "distro/x", "builder"] {
            assert!(!is_runtime_path(path), "{path}");
        }
        for path in [
            ".env",
            ".env.local",
            "packages/app/.env",
            "secrets/k",
            "a/secret/x",
            ".harness/runs/x/chain-signer.json",
            "k.PEM",
            "id.key",
            "credentials.json",
            "svc/service-account-1.json",
            ".cursor/mcp.json",
        ] {
            assert!(is_secret_path(path), "{path}");
        }
        for path in ["env", "src/.environment", "keys.ts", "secretsauce.md"] {
            assert!(!is_secret_path(path), "{path}");
        }
    }

    #[test]
    fn porcelain_lines_parse_including_renames() {
        let entries =
            parse_porcelain(" M src/a.ts\n?? new.txt\nR  old.ts -> new.ts\n\nC  x -> y\n");
        assert_eq!(entries.len(), 4);
        assert_eq!(
            entries[0],
            Entry {
                code: " M".into(),
                path: "src/a.ts".into(),
                orig_path: None
            }
        );
        assert_eq!(entries[1].code, "??");
        assert_eq!(
            entries[2],
            Entry {
                code: "R ".into(),
                path: "new.ts".into(),
                orig_path: Some("old.ts".into())
            }
        );
        assert_eq!(entries[3].orig_path.as_deref(), Some("x"));
        let (commitable, secrets) = filter_commitable(&parse_porcelain(
            "?? src/a.ts\n?? .env\n?? .harness/runs/r/x\n?? .cursor/mcp.json\n?? id.key\n",
        ));
        assert_eq!(
            commitable
                .iter()
                .map(|e| e.path.as_str())
                .collect::<Vec<_>>(),
            ["src/a.ts"]
        );
        assert_eq!(
            secrets.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            [".env", "id.key"]
        );
    }

    #[test]
    fn the_commit_message_is_upstreams() {
        let findings: Vec<Finding> = (1..=32)
            .map(|i| Finding::new(format!("f{i}"), Category::Files, "m"))
            .collect();
        let (subject, body) = attempt_commit_message(3, false, &findings);
        assert_eq!(subject, "harness: run attempt 3 failed");
        assert!(body.starts_with("32 finding(s).\nFinding IDs: f1, f2, "));
        assert!(body.contains(", f30\n…and 2 more\n\nCreated by hanvil run."));
        let (subject, body) = attempt_commit_message(1, true, &[]);
        assert_eq!(subject, "harness: run attempt 1 passed");
        assert_eq!(
            body,
            "0 finding(s).\n\nCreated by hanvil run. Optional: squash attempt commits before merge."
        );
    }

    async fn temp_repo(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hanvil-git-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "t@hanvil"],
            vec!["config", "user.name", "t"],
            vec![
                "commit",
                "-q",
                "--allow-empty",
                "--no-gpg-sign",
                "-m",
                "root",
            ],
        ] {
            git(&args, &dir).await.expect("git");
        }
        dir
    }

    #[tokio::test]
    async fn a_checkpoint_stages_only_what_it_may_and_names_the_secrets_it_left() {
        let repo = temp_repo("checkpoint").await;
        let snapshot = read_snapshot(&repo).await.expect("snapshot");
        assert_eq!(snapshot.branch.as_deref(), Some("main"));
        assert!(!snapshot.detached);
        assert_eq!(snapshot.in_progress_operation, None);
        assert!(snapshot.entries.is_empty());
        assert_clean_for_run_start(&repo).await.expect("clean");

        std::fs::write(repo.join("a.txt"), "a").expect("write");
        std::fs::write(repo.join(".env"), "SECRET=1").expect("write");
        std::fs::create_dir_all(repo.join(".harness/runs/r")).expect("mkdir");
        std::fs::write(repo.join(".harness/runs/r/chain-signer.json"), "{}").expect("write");
        let error = assert_clean_for_run_start(&repo).await.expect_err("dirty");
        assert!(error.to_string().starts_with("Harness run requires a completely clean working tree (no auto-stash).\nCommit or discard local changes, then re-run.\n?? .env\n?? a.txt"), "{error}");

        let (branch, sha) = create_and_checkout_branch(&repo, "My Feature")
            .await
            .expect("branch");
        assert!(branch.starts_with("harness/run-my-feature-"));
        assert_eq!(sha.len(), 40);
        assert_eq!(
            current_branch(&repo).await.as_deref(),
            Some(branch.as_str())
        );

        let checkpoint = commit_attempt(&repo, 1, true, &[]).await.expect("commit");
        assert!(checkpoint.committed);
        assert_eq!(checkpoint.message, "harness: run attempt 1 passed");
        assert_eq!(checkpoint.skipped_secrets, vec![".env"]);
        let shown = git(&["show", "--stat", "--format=%s", "HEAD"], &repo)
            .await
            .expect("show");
        assert!(
            shown.contains("a.txt") && !shown.contains(".env") && !shown.contains("chain-signer"),
            "{shown}"
        );
        assert_eq!(
            consumer_dirty_paths(&repo).await.expect("dirty"),
            vec![".env"]
        );

        let again = commit_attempt(&repo, 2, false, &[]).await.expect("no-op");
        assert!(!again.committed);
        assert_eq!(again.commit_sha, None);
        let _ = std::fs::remove_dir_all(repo);
    }
}
