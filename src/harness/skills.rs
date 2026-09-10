//! Product skills for the generator. `skillProvider.ts` and `skillRepoCache.ts` of hedera-harness
//! dev @ 587a2f3: clone `hedera-skills` once into `.skill-cache/`, fetch and check out the ref on
//! every run, and vendor each product plugin's `SKILL.md` (with its `references/`) into the
//! ignored runtime under the workspace. `--no-skills` skips all of it; CI runs that way.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use sha2::Digest as _;

use crate::harness::artifacts::{self, SKILL_CACHE_DIRNAME, now_iso8601};
use crate::harness::command::{self, Execute};
use crate::harness::prompt::VendoredSkill;

/// `skillProvider.ts:7-8`.
pub(crate) const DEFAULT_REPO: &str = "https://github.com/hedera-dev/hedera-skills.git";
pub(crate) const DEFAULT_REF: &str = "master";
/// `skillProvider.ts:11-16`: the plugins offered to the generator. Authoring, CLI, hackathon
/// and agent-kit stay marketplace skills.
pub(crate) const PRODUCT_PLUGINS: [&str; 4] = [
    "native-services-js",
    "system-contracts",
    "cross-chain",
    "dev-intelligence",
];
const REFERENCES_DIRNAME: &str = "references";
/// `skillRepoCache.ts:8`.
const GIT_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// What stops skills from being vendored. Upstream throws in the same places.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A `git` step failed.
    #[error("{0}")]
    Git(String),
    /// `skillProvider.ts:49`.
    #[error(
        "Skills checkout at {0} has no plugins/ directory. Check HARNESS_SKILLS_REPO / HARNESS_SKILLS_REF."
    )]
    NoPlugins(PathBuf),
    /// `skillProvider.ts:75-81`.
    #[error(
        "No product skills found under {0}. Looked in: native-services-js, system-contracts, cross-chain, dev-intelligence. Authoring, CLI, hackathon, and agent-kit plugins are not offered to the generator."
    )]
    NoSkills(PathBuf),
    /// `skillRepoCache.ts:78-81`.
    #[error(
        "Unable to resolve skill repo ref {ref_name:?} in {checkout}. Check HARNESS_SKILLS_REF (default master)."
    )]
    UnresolvedRef {
        /// The ref asked for.
        ref_name: String,
        /// The cached clone.
        checkout: PathBuf,
    },
    /// A file could not be vendored.
    #[error(transparent)]
    Artifacts(#[from] artifacts::Error),
    /// A copy failed.
    #[error("copying {path}: {source}")]
    Copy {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `skillProvider.ts:29-45`.
pub(crate) async fn provide(
    project_root: &Path,
    workspace: &Path,
    skills_dir: &str,
    repo: Option<&str>,
    ref_name: Option<&str>,
) -> Result<Vec<VendoredSkill>, Error> {
    let repo = repo
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .or_else(crate::harness::env::skills_repo)
        .unwrap_or_else(|| DEFAULT_REPO.to_string());
    let ref_name = ref_name
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .or_else(crate::harness::env::skills_ref)
        .unwrap_or_else(|| DEFAULT_REF.to_string());
    let checkout = ensure_checkout(project_root, &repo, &ref_name).await?;
    let sources = discover_product_skills(&checkout)?;
    vendor(workspace, skills_dir, &sources)
}

async fn git(args: &[&str], cwd: &Path) -> Result<command::Execution, Error> {
    let owned: Vec<String> = args.iter().map(|a| (*a).to_string()).collect();
    command::execute(Execute {
        command: "git",
        args: &owned,
        cwd,
        env: &std::collections::BTreeMap::new(),
        timeout: Some(GIT_TIMEOUT),
        shell: false,
        stream_output: false,
    })
    .await
    .map_err(|e| Error::Git(format!("running git {}: {e}", args.join(" "))))
}

async fn git_or_fail(args: &[&str], cwd: &Path) -> Result<String, Error> {
    let execution = git(args, cwd).await?;
    if execution.exit_code != Some(0) {
        return Err(Error::Git(execution.describe_failure()));
    }
    Ok(execution.stdout)
}

/// `skillRepoCache.ts:11-48`: `<projectRoot>/.skill-cache/<slug>-<sha12>/`, cloned once with
/// `--no-checkout`, then fetched and detached at the resolved ref on every run.
pub(crate) async fn ensure_checkout(
    project_root: &Path,
    repo: &str,
    ref_name: &str,
) -> Result<PathBuf, Error> {
    let cache_root = project_root.join(SKILL_CACHE_DIRNAME);
    std::fs::create_dir_all(&cache_root).map_err(|source| Error::Copy {
        path: cache_root.clone(),
        source,
    })?;
    let checkout = cache_root.join(cache_key(repo));
    if !checkout.join(".git").exists() {
        git_or_fail(
            &["clone", "--no-checkout", repo, &checkout.to_string_lossy()],
            &cache_root,
        )
        .await?;
    }
    git_or_fail(&["fetch", "--tags", "--prune", "origin"], &checkout).await?;
    let sha = resolve_commit(&checkout, ref_name).await?;
    git_or_fail(&["checkout", "--detach", "--force", &sha], &checkout).await?;
    Ok(checkout)
}

/// `skillRepoCache.ts:50-61`.
pub(crate) fn cache_key(repo: &str) -> String {
    let normalised = repo.trim().to_lowercase();
    let normalised = normalised
        .strip_suffix(".git")
        .unwrap_or(&normalised)
        .to_string();
    let hash = hex::encode(sha2::Sha256::digest(normalised.as_bytes()));
    let stripped = normalised
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("git@");
    let mut slug = String::new();
    let mut pending = false;
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            if pending && !slug.is_empty() {
                slug.push('-');
            }
            pending = false;
            slug.push(ch);
        } else {
            pending = true;
        }
    }
    let slug: String = slug.trim_matches('-').chars().take(48).collect();
    format!(
        "{}-{}",
        if slug.is_empty() { "repo" } else { &slug },
        &hash[..12]
    )
}

/// `skillRepoCache.ts:63-82`.
async fn resolve_commit(checkout: &Path, ref_name: &str) -> Result<String, Error> {
    let candidates = [
        ref_name.to_string(),
        format!("origin/{ref_name}"),
        format!("refs/heads/{ref_name}"),
        format!("refs/remotes/origin/{ref_name}"),
        format!("refs/tags/{ref_name}"),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for candidate in candidates {
        if !seen.insert(candidate.clone()) {
            continue;
        }
        let spec = format!("{candidate}^{{commit}}");
        let execution = git(&["rev-parse", &spec], checkout).await?;
        if execution.exit_code == Some(0) && !execution.stdout.trim().is_empty() {
            return Ok(execution.stdout.trim().to_string());
        }
    }
    Err(Error::UnresolvedRef {
        ref_name: ref_name.to_string(),
        checkout: checkout.to_path_buf(),
    })
}

/// `skillProvider.ts:47-84`: every `plugins/<product>/skills/*/SKILL.md`, sorted.
pub(crate) fn discover_product_skills(checkout: &Path) -> Result<Vec<PathBuf>, Error> {
    let plugins = checkout.join("plugins");
    if !plugins.is_dir() {
        return Err(Error::NoPlugins(checkout.to_path_buf()));
    }
    let mut found = Vec::new();
    for plugin in PRODUCT_PLUGINS {
        let skills = plugins.join(plugin).join("skills");
        let Ok(entries) = std::fs::read_dir(&skills) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let skill = entry.path().join("SKILL.md");
            if skill.is_file() {
                found.push(skill);
            }
        }
    }
    found.sort();
    if found.is_empty() {
        return Err(Error::NoSkills(plugins));
    }
    Ok(found)
}

/// `skillProvider.ts:86-140`: copy each `SKILL.md` (and its `references/`) under
/// `<workspace>/<skills_dir>/<slug>/`, and write `manifest.json`.
pub(crate) fn vendor(
    workspace: &Path,
    skills_dir: &str,
    sources: &[PathBuf],
) -> Result<Vec<VendoredSkill>, Error> {
    let skills_dir = skills_dir.trim_matches('/').to_string();
    let root = workspace.join(&skills_dir);
    std::fs::create_dir_all(&root).map_err(|source| Error::Copy {
        path: root.clone(),
        source,
    })?;
    let mut used = std::collections::BTreeSet::new();
    let mut vendored = Vec::new();
    for source in sources {
        let content = std::fs::read_to_string(source).map_err(|e| Error::Copy {
            path: source.clone(),
            source: e,
        })?;
        let name = front_matter(&content, "name").unwrap_or_else(|| {
            source
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "skill".to_string())
        });
        let description = front_matter(&content, "description").unwrap_or_else(|| {
            "Use this skill when relevant to the template being built.".to_string()
        });
        let slug = unique_slug(&slugify(&name), &mut used);
        let relative = format!("{skills_dir}/{slug}/SKILL.md");
        let destination = workspace.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Copy {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&destination, &content).map_err(|source| Error::Copy {
            path: destination.clone(),
            source,
        })?;
        let mut skill = VendoredSkill {
            name,
            relative_path: relative,
            description,
            references_path: None,
        };
        let references = source
            .parent()
            .map(|dir| dir.join(REFERENCES_DIRNAME))
            .filter(|dir| dir.is_dir());
        if let Some(references) = references
            && let Some(parent) = destination.parent()
        {
            copy_dir(&references, &parent.join(REFERENCES_DIRNAME))?;
            skill.references_path = Some(format!("{skills_dir}/{slug}/{REFERENCES_DIRNAME}"));
        }
        vendored.push(skill);
    }
    let manifest = json!({
        "vendoredAt": now_iso8601(),
        "skills": vendored.iter().zip(sources).map(|(skill, source)| {
            let mut entry = json!({
                "name": skill.name,
                "relativePath": skill.relative_path,
                "sourcePath": source,
            });
            if let Some(references) = &skill.references_path {
                entry["referencesPath"] = json!(references);
            }
            entry
        }).collect::<Vec<_>>(),
    });
    artifacts::write_json_file(&root.join("manifest.json"), &manifest)?;
    Ok(vendored)
}

/// `^name:\s*(.+)$` (multiline) over the SKILL.md front matter.
fn front_matter(content: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}:");
    content
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// `skillProvider.ts:142-150`.
fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut pending = false;
    for ch in value.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            if pending && !slug.is_empty() {
                slug.push('-');
            }
            pending = false;
            slug.push(ch);
        } else {
            pending = true;
        }
    }
    let slug: String = slug.chars().take(64).collect();
    let slug = slug.trim_matches('-').to_string();
    if slug.is_empty() {
        "skill".to_string()
    } else {
        slug
    }
}

/// `skillProvider.ts:152-161`.
fn unique_slug(base: &str, used: &mut std::collections::BTreeSet<String>) -> String {
    let mut candidate = base.to_string();
    let mut index = 2;
    while used.contains(&candidate) {
        candidate = format!("{base}-{index}");
        index += 1;
    }
    used.insert(candidate.clone());
    candidate
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), Error> {
    std::fs::create_dir_all(to).map_err(|source| Error::Copy {
        path: to.to_path_buf(),
        source,
    })?;
    for entry in std::fs::read_dir(from)
        .map_err(|source| Error::Copy {
            path: from.to_path_buf(),
            source,
        })?
        .filter_map(Result::ok)
    {
        let target = to.join(entry.file_name());
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target).map_err(|source| Error::Copy {
                path: entry.path(),
                source,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_and_slugs_follow_upstream() {
        let key = cache_key("https://github.com/hedera-dev/hedera-skills.git");
        assert!(
            key.starts_with("github-com-hedera-dev-hedera-skills-"),
            "{key}"
        );
        assert_eq!(key.len(), "github-com-hedera-dev-hedera-skills-".len() + 12);
        assert_eq!(
            cache_key("https://github.com/hedera-dev/hedera-skills.git"),
            cache_key("HTTPS://github.com/hedera-dev/hedera-skills")
        );
        assert!(cache_key("git@github.com:x/y.git").starts_with("github-com-x-y-"));
        assert_eq!(slugify("HTS System Contract!"), "hts-system-contract");
        let mut used = std::collections::BTreeSet::new();
        assert_eq!(unique_slug("a", &mut used), "a");
        assert_eq!(unique_slug("a", &mut used), "a-2");
        assert_eq!(unique_slug("a", &mut used), "a-3");
        assert_eq!(
            front_matter(
                "---\nname: HTS\ndescription:  Use it \n---\n",
                "description"
            )
            .as_deref(),
            Some("Use it")
        );
    }

    /// A local skills repository with one product plugin and one plugin that is not offered.
    async fn skills_repo(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hanvil-skills-src-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skill = dir.join("plugins/system-contracts/skills/hts-system-contract");
        std::fs::create_dir_all(skill.join("references")).expect("mkdir");
        std::fs::write(
            skill.join("SKILL.md"),
            "---\nname: HTS System Contract\ndescription: Tokens through 0x167.\n---\nBody.\n",
        )
        .expect("write");
        std::fs::write(skill.join("references/abi.md"), "abi").expect("write");
        let other = dir.join("plugins/system-contracts/skills/hss-system-contract");
        std::fs::create_dir_all(&other).expect("mkdir");
        std::fs::write(
            other.join("SKILL.md"),
            "---\nname: HTS System Contract\n---\n",
        )
        .expect("write");
        let excluded = dir.join("plugins/hackathon-helper/skills/hackathon-prd");
        std::fs::create_dir_all(&excluded).expect("mkdir");
        std::fs::write(excluded.join("SKILL.md"), "---\nname: PRD\n---\n").expect("write");
        for args in [
            vec!["init", "-q", "-b", "master"],
            vec!["config", "user.email", "s@hanvil"],
            vec!["config", "user.name", "s"],
            vec!["add", "-A"],
            vec!["commit", "-q", "--no-gpg-sign", "-m", "skills"],
        ] {
            assert!(
                std::process::Command::new("git")
                    .args(&args)
                    .current_dir(&dir)
                    .status()
                    .expect("git")
                    .success()
            );
        }
        dir
    }

    #[tokio::test]
    async fn skills_are_cloned_cached_and_vendored_with_their_references() {
        let source = skills_repo("vendor").await;
        let project =
            std::env::temp_dir().join(format!("hanvil-skills-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&project);
        std::fs::create_dir_all(&project).expect("mkdir");
        let repo_url = source.to_string_lossy().into_owned();

        let vendored = provide(
            &project,
            &project,
            ".harness/runtime/skills",
            Some(&repo_url),
            Some("master"),
        )
        .await
        .expect("vendored");
        assert_eq!(vendored.len(), 2, "the hackathon plugin is not offered");
        // Sources are sorted by path: hss-… comes before hts-…, and both are named the same.
        assert_eq!(vendored[0].name, "HTS System Contract");
        assert_eq!(
            vendored[0].relative_path,
            ".harness/runtime/skills/hts-system-contract/SKILL.md"
        );
        assert_eq!(
            vendored[0].description,
            "Use this skill when relevant to the template being built."
        );
        assert_eq!(vendored[0].references_path, None);
        assert_eq!(
            vendored[1].relative_path, ".harness/runtime/skills/hts-system-contract-2/SKILL.md",
            "same name, unique slug"
        );
        assert_eq!(vendored[1].description, "Tokens through 0x167.");
        assert_eq!(
            vendored[1].references_path.as_deref(),
            Some(".harness/runtime/skills/hts-system-contract-2/references")
        );
        assert!(
            project
                .join(".harness/runtime/skills/hts-system-contract-2/references/abi.md")
                .exists()
        );
        let manifest: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".harness/runtime/skills/manifest.json"))
                .expect("manifest"),
        )
        .expect("json");
        assert_eq!(manifest["skills"].as_array().map(Vec::len), Some(2));
        assert!(manifest["skills"][0].get("referencesPath").is_none());
        assert!(manifest["skills"][1]["referencesPath"].is_string());
        let cache = project.join(SKILL_CACHE_DIRNAME);
        assert!(cache.is_dir());

        // A second run reuses the clone and only fetches.
        let again = provide(
            &project,
            &project,
            ".harness/runtime/skills",
            Some(&repo_url),
            Some("master"),
        )
        .await
        .expect("vendored again");
        assert_eq!(again.len(), 2);
        let missing = provide(
            &project,
            &project,
            ".harness/runtime/skills",
            Some(&repo_url),
            Some("nope"),
        )
        .await
        .expect_err("unknown ref");
        assert!(
            missing
                .to_string()
                .starts_with("Unable to resolve skill repo ref \"nope\""),
            "{missing}"
        );
        let _ = std::fs::remove_dir_all(project);
        let _ = std::fs::remove_dir_all(source);
    }
}
