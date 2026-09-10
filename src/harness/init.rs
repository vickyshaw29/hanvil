//! `hanvil init`: bootstrap a harness project, or adopt one that already exists. `initRunner.ts`,
//! `initSeeder.ts` and `harnessProvisioner.ts` of hedera-harness dev @ 587a2f3. A new or empty
//! target is cloned from scaffold-hbar and provisioned; a directory that already holds a project
//! is provisioned in place. The skeleton recipe is never written over one that exists.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

use crate::harness::command::{self, Execute};
use crate::harness::git;
use crate::harness::session::log_phase;

/// `initSeeder.ts:8-12`.
pub(crate) const DEFAULT_SCAFFOLD_REPO: &str = "https://github.com/hedera-dev/scaffold-hbar.git";
pub(crate) const DEFAULT_SCAFFOLD_REF: &str = "main";
const INITIAL_COMMIT_MESSAGE: &str = "Initial scaffold from scaffold-hbar";
/// scaffold-hbar keeps one template per branch under this prefix.
pub(crate) const TEMPLATE_BRANCH_PREFIX: &str = "templates/";
const GIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// `skeletons/project-harness/`, bundled flat.
const SKELETON: [(&str, &str, &str); 4] = [
    (
        ".harness/spec.yaml",
        "spec.yaml",
        include_str!("skeletons/spec.yaml"),
    ),
    (
        ".harness/prd.md",
        "prd.md",
        include_str!("skeletons/prd.md"),
    ),
    (
        ".harness/validators/static.json",
        "static.json",
        include_str!("skeletons/static.json"),
    ),
    (
        ".harness/validators/yarn.json",
        "yarn.json",
        include_str!("skeletons/yarn.json"),
    ),
];
const GITIGNORE_SNIPPET: &str = include_str!("skeletons/gitignore-snippet.txt");

/// What stops `init`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `initSeeder.ts:209`.
    #[error("Init target exists and is not a directory: {0}")]
    NotADirectory(PathBuf),
    /// `initSeeder.ts:226-232`.
    #[error("{0}")]
    NotAProject(String),
    /// `initSeeder.ts:92-102`.
    #[error("{0}")]
    Clone(String),
    /// `initSeeder.ts:255`.
    #[error("Init preflight{named} failed for command \"{command}\".")]
    Preflight {
        /// ` "name"` when the command has one.
        named: String,
        /// The command line.
        command: String,
    },
    /// A `git` step failed.
    #[error(transparent)]
    Git(#[from] git::Error),
    /// A file could not be written.
    #[error("writing {path}: {source}")]
    Write {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A command could not be spawned.
    #[error("running {command}: {source}")]
    Spawn {
        /// The command.
        command: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `initSeeder.ts:190-193`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The target does not exist.
    SeedNew,
    /// The target is an empty directory.
    SeedEmpty,
    /// The target holds a project; adopt it.
    InPlace,
}

/// `types.ts` `InitResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InitResult {
    /// `seeded` or `in-place`.
    pub(crate) mode: &'static str,
    /// The project.
    pub(crate) target_dir: PathBuf,
    /// The scaffold, when one was cloned.
    pub(crate) repo: Option<String>,
    /// Its ref.
    pub(crate) ref_name: Option<String>,
    /// HEAD after init, when known.
    pub(crate) commit_sha: Option<String>,
    /// Relative paths written.
    pub(crate) written_files: Vec<String>,
    /// Recipe files already present and left untouched.
    pub(crate) skipped_files: Vec<String>,
    /// `.gitignore` gained the snippet.
    pub(crate) gitignore_updated: bool,
    /// `package.json` gained `harness:run`.
    pub(crate) package_json_updated: bool,
    /// What to do next.
    pub(crate) next_steps: Vec<String>,
}

/// `hanvil init [DIR] [--repo URL] [--ref REF] [--template NAME] [--skip-install]`.
pub(crate) struct InitOptions {
    /// Defaults to the current directory.
    pub(crate) target_dir: Option<PathBuf>,
    /// Scaffold to clone.
    pub(crate) repo: Option<String>,
    /// Its ref.
    pub(crate) ref_name: Option<String>,
    /// Alias for a `templates/<name>` ref.
    pub(crate) template: Option<String>,
    /// Skip `yarn install` after the clone.
    pub(crate) skip_install: bool,
}

/// `initSeeder.ts:21-24`: a bare name gets the `templates/` prefix; a ref passes through.
pub(crate) fn resolve_template_ref(template: &str) -> String {
    let trimmed = template.trim();
    if trimmed.contains('/') {
        trimmed.to_string()
    } else {
        format!("{TEMPLATE_BRANCH_PREFIX}{trimmed}")
    }
}

/// `initSeeder.ts:204-233`.
pub(crate) fn detect_mode(target: &Path) -> Result<Mode, Error> {
    let metadata = match std::fs::metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Mode::SeedNew),
        Err(error) => {
            return Err(Error::Write {
                path: target.to_path_buf(),
                source: error,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(Error::NotADirectory(target.to_path_buf()));
    }
    let entries: Vec<String> = std::fs::read_dir(target)
        .map_err(|source| Error::Write {
            path: target.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    if entries.is_empty() {
        return Ok(Mode::SeedEmpty);
    }
    if entries.iter().any(|name| name == "package.json") {
        return Ok(Mode::InPlace);
    }
    Err(Error::NotAProject(format!(
        "Init target directory is not empty and does not look like a project: {}\nFound {} entr{} (e.g. {}) but no package.json.\nChoose an empty directory to scaffold into, or run init inside a project to adopt the harness there.",
        target.display(),
        entries.len(),
        if entries.len() == 1 { "y" } else { "ies" },
        entries
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// `initRunner.ts:27-88`.
pub(crate) async fn run(options: InitOptions) -> Result<InitResult, Error> {
    let target = options
        .target_dir
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    let target = std::path::absolute(&target).unwrap_or(target);
    let repo = options
        .repo
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or(DEFAULT_SCAFFOLD_REPO)
        .to_string();
    let ref_name = options
        .ref_name
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .or_else(|| {
            options
                .template
                .as_deref()
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .map(resolve_template_ref)
        })
        .unwrap_or_else(|| DEFAULT_SCAFFOLD_REF.to_string());

    let mode = detect_mode(&target)?;
    let in_place = mode == Mode::InPlace;
    log_phase(
        if in_place {
            "Adopting harness in existing project"
        } else {
            "Init started"
        },
        Some(&target.display().to_string()),
    );

    let (seed_repo, seed_ref, commit_sha) = if in_place {
        (None, None, git::head_sha(&target).await.ok())
    } else {
        let sha = seed(&target, &repo, &ref_name, options.skip_install).await?;
        (Some(repo), Some(ref_name), Some(sha))
    };

    log_phase(
        "Provisioning .harness/",
        Some(&target.display().to_string()),
    );
    let provisioned = provision(&target)?;
    if !provisioned.skipped.is_empty() {
        log_phase(
            "Kept existing recipe files",
            Some(&format!(
                "{} (not overwritten)",
                provisioned.skipped.join(", ")
            )),
        );
    }
    log_phase(
        "Init complete",
        Some(&format!("{} recipe file(s)", provisioned.written.len())),
    );
    let had_existing_recipe = provisioned
        .skipped
        .iter()
        .any(|file| file.ends_with("spec.yaml"));
    Ok(InitResult {
        mode: if in_place { "in-place" } else { "seeded" },
        next_steps: next_steps(
            &target,
            in_place,
            had_existing_recipe,
            provisioned.package_json_updated,
        ),
        target_dir: target,
        repo: seed_repo,
        ref_name: seed_ref,
        commit_sha,
        written_files: provisioned.written,
        skipped_files: provisioned.skipped,
        gitignore_updated: provisioned.gitignore_updated,
        package_json_updated: provisioned.package_json_updated,
    })
}

async fn shell(command: &str, cwd: &Path, timeout: Duration) -> Result<command::Execution, Error> {
    command::execute(Execute {
        command,
        args: &[],
        cwd,
        env: &std::collections::BTreeMap::new(),
        timeout: Some(timeout),
        shell: true,
        stream_output: true,
    })
    .await
    .map_err(|source| Error::Spawn {
        command: command.to_string(),
        source,
    })
}

/// `initSeeder.ts:68-144`: clone the scaffold, replace its git history with a fresh repository
/// and one commit, then `yarn install` unless skipped.
async fn seed(
    target: &Path,
    repo: &str,
    ref_name: &str,
    skip_install: bool,
) -> Result<String, Error> {
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|source| Error::Write {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    log_phase(
        "Cloning scaffold project",
        Some(&format!("{repo}@{ref_name} → {}", target.display())),
    );
    let parent = target.parent().unwrap_or(target);
    let clone = command::execute(Execute {
        command: "git",
        args: &[
            "clone".to_string(),
            "--branch".to_string(),
            ref_name.to_string(),
            "--single-branch".to_string(),
            repo.to_string(),
            target.to_string_lossy().into_owned(),
        ],
        cwd: parent,
        env: &std::collections::BTreeMap::new(),
        timeout: Some(GIT_TIMEOUT),
        shell: false,
        stream_output: true,
    })
    .await
    .map_err(|source| Error::Spawn {
        command: "git clone".to_string(),
        source,
    })?;
    if clone.exit_code != Some(0) {
        // A wrong template name is the likely cause, so name the real options rather than
        // leaving the user with git's "remote branch not found".
        let available = list_template_branches(repo).await;
        let mut message = format!(
            "Could not clone {repo} at ref {}.",
            serde_json::to_string(ref_name).unwrap_or_default()
        );
        if available.is_empty() {
            message.push_str(" Could not list templates from the remote.");
        } else {
            message.push_str(&format!(" Available templates: {}.", available.join(", ")));
        }
        let stderr = clone.stderr.trim();
        if !stderr.is_empty() {
            message.push(' ');
            message.push_str(stderr);
        }
        return Err(Error::Clone(message));
    }
    let scaffold_sha = git::head_sha(target).await?;
    log_phase(
        "Scaffold cloned",
        Some(&format!(
            "{} (will re-init git)",
            &scaffold_sha[..scaffold_sha.len().min(8)]
        )),
    );

    log_phase(
        "Creating fresh git repository",
        Some("discarding scaffold-hbar history and remote"),
    );
    let _ = std::fs::remove_dir_all(target.join(".git"));
    git::git(&["init", "-b", "main"], target).await?;
    git::git(&["add", "-A"], target).await?;
    git::git(
        &[
            "-c",
            "user.name=hanvil",
            "-c",
            "user.email=hanvil@local",
            "commit",
            "-m",
            INITIAL_COMMIT_MESSAGE,
        ],
        target,
    )
    .await?;
    let sha = git::head_sha(target).await?;
    log_phase(
        "Fresh git repository ready",
        Some(&format!("{} on main (no remote)", &sha[..sha.len().min(8)])),
    );

    if !skip_install {
        log_phase("Init preflight", Some("install"));
        let install = shell("yarn install", target, Duration::from_secs(300)).await?;
        if install.exit_code != Some(0) {
            return Err(Error::Preflight {
                named: " \"install\"".to_string(),
                command: "yarn install".to_string(),
            });
        }
        log_phase(
            "Init preflight finished",
            Some(&format!(
                "install exit=0 durationMs={}",
                install.duration_ms
            )),
        );
    }
    Ok(sha)
}

/// `initSeeder.ts:27-42`.
async fn list_template_branches(repo: &str) -> Vec<String> {
    let pattern = format!("refs/heads/{TEMPLATE_BRANCH_PREFIX}*");
    let Ok(output) = command::capture(
        "git",
        &["ls-remote", "--heads", repo, &pattern],
        &std::env::temp_dir(),
    )
    .await
    else {
        return Vec::new();
    };
    if !output.ok {
        return Vec::new();
    }
    let mut branches: Vec<String> = output
        .stdout
        .lines()
        .filter_map(|line| line.split("refs/heads/").nth(1))
        .map(str::trim)
        .filter_map(|branch| branch.strip_prefix(TEMPLATE_BRANCH_PREFIX))
        .map(str::to_string)
        .collect();
    branches.sort();
    branches
}

struct Provisioned {
    written: Vec<String>,
    skipped: Vec<String>,
    gitignore_updated: bool,
    package_json_updated: bool,
}

/// `harnessProvisioner.ts:25-73`.
fn provision(target: &Path) -> Result<Provisioned, Error> {
    let write = |path: &Path, content: &str| -> Result<(), Error> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(path, content).map_err(|source| Error::Write {
            path: path.to_path_buf(),
            source,
        })
    };
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for (relative, _, content) in SKELETON {
        let destination = target.join(relative);
        if destination.exists() {
            skipped.push(relative.to_string());
            continue;
        }
        write(&destination, content)?;
        written.push(relative.to_string());
    }
    let gitignore_updated = ensure_gitignore(target)?;
    let package_json_updated = ensure_package_script(target)?;
    let keep = target.join(".harness/runs/.gitkeep");
    if !keep.exists() {
        write(&keep, "")?;
        written.push(".harness/runs/.gitkeep".to_string());
    }
    Ok(Provisioned {
        written,
        skipped,
        gitignore_updated,
        package_json_updated,
    })
}

/// `harnessProvisioner.ts:83-101`.
fn ensure_gitignore(target: &Path) -> Result<bool, Error> {
    let path = target.join(".gitignore");
    let snippet = GITIGNORE_SNIPPET.trim();
    let existing = match std::fs::read_to_string(&path) {
        Ok(existing) => existing,
        Err(_) => {
            std::fs::write(&path, format!("{snippet}\n")).map_err(|source| Error::Write {
                path: path.clone(),
                source,
            })?;
            return Ok(true);
        }
    };
    if existing.contains(".harness/runs/") && existing.contains(".harness/runtime/") {
        return Ok(false);
    }
    let separator = if existing.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    std::fs::write(&path, format!("{existing}{separator}{snippet}\n")).map_err(|source| {
        Error::Write {
            path: path.clone(),
            source,
        }
    })?;
    Ok(true)
}

/// `harnessProvisioner.ts:103-130`: `scripts["harness:run"]`, when the key is free.
fn ensure_package_script(target: &Path) -> Result<bool, Error> {
    let path = target.join("package.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(false);
    };
    let Ok(mut parsed) = serde_json::from_str::<Value>(&raw) else {
        return Ok(false);
    };
    let Some(object) = parsed.as_object_mut() else {
        return Ok(false);
    };
    let scripts = object
        .entry("scripts")
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(scripts) = scripts.as_object_mut() else {
        return Ok(false);
    };
    if scripts
        .get("harness:run")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return Ok(false);
    }
    scripts.insert(
        "harness:run".to_string(),
        Value::String("hanvil run .harness/spec.yaml".to_string()),
    );
    let rendered = serde_json::to_string_pretty(&parsed).unwrap_or_default();
    std::fs::write(&path, format!("{rendered}\n")).map_err(|source| Error::Write {
        path: path.clone(),
        source,
    })?;
    Ok(true)
}

/// `initRunner.ts:99-131`.
fn next_steps(
    target: &Path,
    in_place: bool,
    had_existing_recipe: bool,
    package_json_updated: bool,
) -> Vec<String> {
    let mut steps = Vec::new();
    if !in_place {
        steps.push(format!("cd {}", target.display()));
        steps.push(
            "Optional: add your own remote (init created a fresh git repo with no origin):"
                .to_string(),
        );
        steps.push("  git remote add origin <your-repo-url>".to_string());
        steps.push("  git push -u origin main".to_string());
    }
    steps.push(if had_existing_recipe {
        "This project already had a .harness/ recipe — it was left untouched.".to_string()
    } else {
        "A starter recipe is under .harness/ — customize it before running:".to_string()
    });
    if !had_existing_recipe {
        steps.push(
            "Install the hedera-harness plugin from the marketplace to author it:".to_string(),
        );
        steps.push("  /plugin marketplace add hedera-dev/hedera-skills".to_string());
        steps.push("  /plugin install hedera-harness".to_string());
        steps.push(
            "  /create-harness-spec — turn an idea into .harness/prd.md + spec + validators"
                .to_string(),
        );
        steps.push("Or edit .harness/prd.md and .harness/spec.yaml by hand".to_string());
    }
    steps.push("hanvil doctor    # check the setup before a long run".to_string());
    steps.push(if package_json_updated {
        "yarn harness:run".to_string()
    } else {
        "hanvil run".to_string()
    });
    steps
}

/// `cli.ts:69-94`: what `init` prints.
pub(crate) fn format_result(result: &InitResult) -> String {
    let mut lines = vec![
        format!(
            "Init {}",
            if result.mode == "seeded" {
                "seeded"
            } else {
                "adopted"
            }
        ),
        format!("targetDir={}", result.target_dir.display()),
    ];
    if let (Some(repo), Some(ref_name)) = (&result.repo, &result.ref_name) {
        lines.push(format!("repo={repo}"));
        lines.push(format!("ref={ref_name}"));
    }
    if let Some(sha) = &result.commit_sha {
        lines.push(format!("commit={}", &sha[..sha.len().min(8)]));
    }
    lines.push(format!("written={}", result.written_files.join(", ")));
    if !result.skipped_files.is_empty() {
        lines.push(format!("kept={}", result.skipped_files.join(", ")));
    }
    lines.push(format!("gitignoreUpdated={}", result.gitignore_updated));
    lines.push(format!(
        "packageJsonUpdated={}",
        result.package_json_updated
    ));
    lines.push(String::new());
    lines.push("Next steps:".to_string());
    lines.extend(result.next_steps.iter().map(|step| format!("  {step}")));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_refs_and_modes_follow_upstream() {
        assert_eq!(resolve_template_ref("hedera-demo"), "templates/hedera-demo");
        assert_eq!(resolve_template_ref(" templates/x "), "templates/x");
        assert_eq!(resolve_template_ref("feat/y"), "feat/y");
        let base = std::env::temp_dir().join(format!("hanvil-init-mode-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        assert_eq!(
            detect_mode(&base.join("missing")).expect("mode"),
            Mode::SeedNew
        );
        std::fs::create_dir_all(base.join("empty")).expect("mkdir");
        assert_eq!(
            detect_mode(&base.join("empty")).expect("mode"),
            Mode::SeedEmpty
        );
        std::fs::create_dir_all(base.join("project")).expect("mkdir");
        std::fs::write(base.join("project/package.json"), "{}").expect("write");
        assert_eq!(
            detect_mode(&base.join("project")).expect("mode"),
            Mode::InPlace
        );
        std::fs::create_dir_all(base.join("junk")).expect("mkdir");
        std::fs::write(base.join("junk/notes.txt"), "").expect("write");
        let error = detect_mode(&base.join("junk")).expect_err("refused");
        assert!(
            error
                .to_string()
                .contains("Found 1 entry (e.g. notes.txt) but no package.json."),
            "{error}"
        );
        std::fs::write(base.join("file"), "").expect("write");
        assert!(matches!(
            detect_mode(&base.join("file")),
            Err(Error::NotADirectory(_))
        ));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn adopting_a_project_writes_the_skeleton_once_and_keeps_what_exists() {
        let dir = std::env::temp_dir().join(format!("hanvil-init-adopt-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(
            dir.join("package.json"),
            "{\n  \"name\": \"app\",\n  \"scripts\": {\"dev\": \"next dev\"}\n}\n",
        )
        .expect("write");
        std::fs::write(dir.join(".gitignore"), "node_modules/").expect("write");
        let result = run(InitOptions {
            target_dir: Some(dir.clone()),
            repo: None,
            ref_name: None,
            template: None,
            skip_install: true,
        })
        .await
        .expect("init");
        assert_eq!(result.mode, "in-place");
        assert_eq!(
            result.written_files,
            vec![
                ".harness/spec.yaml",
                ".harness/prd.md",
                ".harness/validators/static.json",
                ".harness/validators/yarn.json",
                ".harness/runs/.gitkeep"
            ]
        );
        assert!(result.skipped_files.is_empty());
        assert!(result.gitignore_updated && result.package_json_updated);
        assert_eq!(
            std::fs::read_to_string(dir.join(".gitignore")).expect("gitignore"),
            format!("node_modules/\n\n{}\n", GITIGNORE_SNIPPET.trim())
        );
        let package: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).expect("pkg"))
                .expect("json");
        assert_eq!(
            package["scripts"]["harness:run"],
            "hanvil run .harness/spec.yaml"
        );
        assert_eq!(package["scripts"]["dev"], "next dev");
        assert!(
            std::fs::read_to_string(dir.join(".harness/spec.yaml"))
                .expect("spec")
                .starts_with("schemaVersion: 3")
        );
        assert!(result.next_steps.iter().any(|s| s == "yarn harness:run"));
        assert!(
            result
                .next_steps
                .iter()
                .any(|s| s.starts_with("A starter recipe is under .harness/"))
        );

        // Again: nothing overwritten, nothing appended twice.
        std::fs::write(dir.join(".harness/prd.md"), "mine").expect("write");
        let again = run(InitOptions {
            target_dir: Some(dir.clone()),
            repo: None,
            ref_name: None,
            template: None,
            skip_install: true,
        })
        .await
        .expect("init");
        assert_eq!(again.written_files, Vec::<String>::new());
        assert_eq!(again.skipped_files.len(), 4);
        assert!(!again.gitignore_updated && !again.package_json_updated);
        assert_eq!(
            std::fs::read_to_string(dir.join(".harness/prd.md")).expect("prd"),
            "mine"
        );
        assert!(
            again
                .next_steps
                .iter()
                .any(|s| s.starts_with("This project already had a .harness/ recipe"))
        );
        let rendered = format_result(&again);
        assert!(
            rendered.starts_with("Init adopted\ntargetDir="),
            "{rendered}"
        );
        assert!(
            rendered.contains("kept=.harness/spec.yaml, .harness/prd.md"),
            "{rendered}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn seeding_clones_a_template_branch_into_a_fresh_repository() {
        let base = std::env::temp_dir().join(format!("hanvil-init-seed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let scaffold = base.join("scaffold");
        std::fs::create_dir_all(&scaffold).expect("mkdir");
        std::fs::write(scaffold.join("package.json"), "{\"name\": \"scaffold\"}").expect("write");
        for args in [
            vec!["init", "-q", "-b", "main"],
            vec!["config", "user.email", "s@hanvil"],
            vec!["config", "user.name", "s"],
            vec!["add", "-A"],
            vec!["commit", "-q", "--no-gpg-sign", "-m", "scaffold"],
            vec!["checkout", "-q", "-b", "templates/demo"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(&args)
                    .current_dir(&scaffold)
                    .status()
                    .expect("git")
                    .success()
            );
        }
        std::fs::write(scaffold.join("README.md"), "demo template").expect("write");
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-q", "--no-gpg-sign", "-m", "demo"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(&args)
                    .current_dir(&scaffold)
                    .status()
                    .expect("git")
                    .success()
            );
        }

        let target = base.join("app");
        let result = run(InitOptions {
            target_dir: Some(target.clone()),
            repo: Some(scaffold.to_string_lossy().into_owned()),
            ref_name: None,
            template: Some("demo".into()),
            skip_install: true,
        })
        .await
        .expect("init");
        assert_eq!(result.mode, "seeded");
        assert_eq!(result.ref_name.as_deref(), Some("templates/demo"));
        assert!(target.join("README.md").exists());
        assert!(target.join(".harness/spec.yaml").exists());
        let log = std::process::Command::new("git")
            .args(["log", "--format=%s", "--all"])
            .current_dir(&target)
            .output()
            .expect("git");
        assert_eq!(
            String::from_utf8_lossy(&log.stdout).trim(),
            INITIAL_COMMIT_MESSAGE,
            "no scaffold history"
        );
        let remotes = std::process::Command::new("git")
            .args(["remote"])
            .current_dir(&target)
            .output()
            .expect("git");
        assert_eq!(
            String::from_utf8_lossy(&remotes.stdout).trim(),
            "",
            "no origin"
        );
        assert!(result.next_steps[0].starts_with("cd "));

        let bad = run(InitOptions {
            target_dir: Some(base.join("app2")),
            repo: Some(scaffold.to_string_lossy().into_owned()),
            ref_name: None,
            template: Some("nope".into()),
            skip_install: true,
        })
        .await
        .expect_err("unknown template");
        assert!(
            bad.to_string().contains("Available templates: demo."),
            "{bad}"
        );
        let _ = std::fs::remove_dir_all(base);
    }
}
