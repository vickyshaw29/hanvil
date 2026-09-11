//! ASSERT: the deterministic gates. `validation/index.ts` and `validation/installFingerprint.ts`
//! of hedera-harness dev @ 587a2f3. Required and forbidden files, the static validator's JSON,
//! file and text assertions, the secret scan, and the commands that must exit zero. Finding ids
//! and messages are upstream's; a recipe's findings read the same under both harnesses.

use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::Digest as _;

use crate::harness::artifacts::{ISOLATED_CONTEXT_DIR, ISOLATED_SKILLS_DIR, SKILL_CACHE_DIRNAME};
use crate::harness::command::{self, Execute, Execution};
use crate::harness::findings::{Category, Finding, ValidationResult, truncate_details};
use crate::harness::spec::{SecretPattern, SecretScan, Spec};

/// `validation/index.ts:75-87`. Directories the secret scan never walks. Harness runtime is
/// excluded deliberately: `.harness/runs/<id>/` holds the signer, and scanning it would report
/// the harness's own material as a finding against the app.
const SCAN_SKIP_DIRS: [&str; 11] = [
    "node_modules",
    ".next",
    ".git",
    "dist",
    "artifacts",
    "cache",
    ".harness",
    ISOLATED_CONTEXT_DIR,
    ISOLATED_SKILLS_DIR,
    ".harness-semantic",
    SKILL_CACHE_DIRNAME,
];

/// `installFingerprint.ts:6`.
const FINGERPRINT_FILES: [&str; 2] = ["yarn.lock", "package.json"];

/// What stops ASSERT from running at all. Upstream throws in the same places: a validator
/// config that cannot be read or parsed is a recipe problem, not a finding against the app.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// A validator config could not be read.
    #[error("reading {path}: {source}")]
    Read {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A validator config is not JSON.
    #[error("parsing {path}: {source}")]
    Parse {
        /// The file.
        path: PathBuf,
        /// The parser's error.
        #[source]
        source: serde_json::Error,
    },
    /// A secret pattern is not a regular expression this harness can compile.
    #[error("secret pattern {name:?} does not compile: {source}")]
    Pattern {
        /// `secretScan.patterns[].name`.
        name: String,
        /// The regex error.
        #[source]
        source: regex::Error,
    },
    /// A validator command could not be spawned.
    #[error("running validation command {name}: {source}")]
    Spawn {
        /// `commands[].name`.
        name: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `validation/index.ts:30-34`. Whether ASSERT left the workspace clean enough to pay for a
/// SMOKE boot: agent findings are ignored so a failed GENERATE still gets deterministic
/// context; every other finding blocks the boot.
pub(crate) fn is_ready_for_smoke(findings: &[Finding]) -> bool {
    findings
        .iter()
        .all(|finding| finding.category == Category::Agent)
}

/// `validation/index.ts:89-115`.
pub(crate) async fn run(
    workspace: &Path,
    spec: &Spec,
    install_cache_path: Option<&Path>,
) -> Result<ValidationResult, Error> {
    let mut findings = Vec::new();
    findings.extend(required_files(workspace, &spec.required_files));
    findings.extend(forbidden_files(workspace, &spec.forbidden_files));
    findings.extend(static_config(workspace, &spec.validators.static_path)?);
    findings.extend(scan_secrets(workspace, &spec.secret_scan)?);
    let (command_findings, command_results) = commands(
        workspace,
        &spec.validators.commands_path,
        install_cache_path,
    )
    .await?;
    findings.extend(command_findings);
    Ok(ValidationResult {
        passed: findings.is_empty(),
        findings,
        command_results,
        playwright_gate: None,
        evaluation: None,
        chain_ledger: None,
    })
}

/// `validation/index.ts:117-131`.
fn required_files(workspace: &Path, required: &[String]) -> Vec<Finding> {
    required
        .iter()
        .filter(|relative| !workspace.join(relative).exists())
        .map(|relative| {
            Finding::new(
                format!("required-file:{relative}"),
                Category::Files,
                format!("Required file is missing: {relative}"),
            )
        })
        .collect()
}

/// `validation/index.ts:133-150`.
fn forbidden_files(workspace: &Path, forbidden: &[String]) -> Vec<Finding> {
    forbidden
        .iter()
        .filter(|relative| workspace.join(relative).exists())
        .map(|relative| {
            Finding::new(
                format!("forbidden-file:{relative}"),
                Category::Files,
                format!("Forbidden file or directory exists: {relative}"),
            )
        })
        .collect()
}

fn read_json_config(path: &Path) -> Result<Value, Error> {
    let raw = std::fs::read_to_string(path).map_err(|source| Error::Read {
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_str(&raw).map_err(|source| Error::Parse {
        path: path.to_path_buf(),
        source,
    })
}

fn string_items(value: Option<&Value>) -> Vec<&str> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

/// `validation/index.ts:152-234`: `validators/static.json`.
fn static_config(workspace: &Path, static_path: &Path) -> Result<Vec<Finding>, Error> {
    let config = read_json_config(static_path)?;
    let mut findings = Vec::new();

    for assertion in config
        .get("jsonAssertions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(file), Some(dotted)) = (
            assertion.get("file").and_then(Value::as_str),
            assertion.get("path").and_then(Value::as_str),
        ) else {
            continue;
        };
        let path = workspace.join(file);
        if !path.exists() {
            continue;
        }
        // A generated file with broken JSON is an app defect, not a harness crash.
        let content = match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(|e| e.to_string()))
        {
            Ok(content) => content,
            Err(message) => {
                findings.push(
                    Finding::new(
                        format!("json-parse:{file}"),
                        Category::Static,
                        format!("Could not parse {file} as JSON"),
                    )
                    .with_details(message),
                );
                continue;
            }
        };
        let actual = get_by_path(&content, dotted);
        let expected = assertion.get("equals");
        if !values_equal(actual, expected) {
            findings.push(
                Finding::new(
                    format!("json:{file}:{dotted}"),
                    Category::Static,
                    format!("JSON assertion failed in {file} at {dotted}"),
                )
                .with_details(format!(
                    "Expected {} but found {}",
                    stringify(expected),
                    stringify(actual)
                )),
            );
        }
    }

    let file_assertions = config.get("fileAssertions");
    for relative in string_items(file_assertions.and_then(|f| f.get("required"))) {
        if !workspace.join(relative).exists() {
            findings.push(Finding::new(
                format!("static-required:{relative}"),
                Category::Static,
                format!("Static validator requires file: {relative}"),
            ));
        }
    }
    for relative in string_items(file_assertions.and_then(|f| f.get("forbidden"))) {
        if workspace.join(relative).exists() {
            findings.push(Finding::new(
                format!("static-forbidden:{relative}"),
                Category::Static,
                format!("Static validator forbids path: {relative}"),
            ));
        }
    }

    for assertion in config
        .get("textAssertions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(file) = assertion.get("file").and_then(Value::as_str) else {
            continue;
        };
        let path = workspace.join(file);
        let Ok(content) = std::fs::read_to_string(&path) else {
            findings.push(Finding::new(
                format!("text-missing:{file}"),
                Category::Static,
                format!("Text assertion file missing: {file}"),
            ));
            continue;
        };
        for needle in string_items(assertion.get("contains")) {
            if !content.contains(needle) {
                findings.push(Finding::new(
                    format!("text:{file}:{needle}"),
                    Category::Static,
                    format!("Expected {file} to contain \"{needle}\""),
                ));
            }
        }
    }

    if let Some(scan) = config.get("secretScan").and_then(Value::as_object) {
        let fail_on_files = string_items(scan.get("failOnFiles"))
            .into_iter()
            .map(str::to_string)
            .collect();
        let patterns = scan
            .get("patterns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|entry| {
                Some(SecretPattern {
                    name: entry.get("name")?.as_str()?.to_string(),
                    pattern: entry.get("pattern")?.as_str()?.to_string(),
                    allow_in: string_items(entry.get("allowIn"))
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                })
            })
            .collect();
        findings.extend(scan_secrets(
            workspace,
            &SecretScan {
                fail_on_files,
                patterns,
            },
        )?);
    }

    Ok(findings)
}

/// `validation/index.ts:367-374`: dot-separated lookup that bails on arrays and scalars.
fn get_by_path<'a>(value: &'a Value, dotted: &str) -> Option<&'a Value> {
    dotted
        .split('.')
        .try_fold(value, |current, segment| current.as_object()?.get(segment))
}

/// `validation/index.ts:376-393`: deep equality, accepting a scalar where the spec expects a
/// single-item array (common generator drift).
fn values_equal(actual: Option<&Value>, expected: Option<&Value>) -> bool {
    if actual == expected {
        return true;
    }
    match expected.and_then(Value::as_array) {
        Some(items) if items.len() == 1 => {
            actual == Some(&items[0])
                || actual
                    .and_then(Value::as_array)
                    .is_some_and(|a| a.len() == 1 && a[0] == items[0])
        }
        _ => false,
    }
}

/// `JSON.stringify` of a possibly-missing value: `undefined` when absent.
fn stringify(value: Option<&Value>) -> String {
    value.map_or_else(|| "undefined".to_string(), Value::to_string)
}

/// `validation/index.ts:241-277`.
fn scan_secrets(workspace: &Path, scan: &SecretScan) -> Result<Vec<Finding>, Error> {
    let mut findings = Vec::new();
    for relative in &scan.fail_on_files {
        if workspace.join(relative).exists() {
            findings.push(Finding::new(
                format!("secret-file:{relative}"),
                Category::Secret,
                format!("Secret scan forbids file: {relative}"),
            ));
        }
    }
    let compiled = scan
        .patterns
        .iter()
        .map(|pattern| {
            // `new RegExp(pattern, "m")`.
            regex::Regex::new(&format!("(?m){}", pattern.pattern))
                .map(|regex| (pattern, regex))
                .map_err(|source| Error::Pattern {
                    name: pattern.name.clone(),
                    source,
                })
        })
        .collect::<Result<Vec<_>, Error>>()?;
    for relative in collect_text_files(workspace) {
        let Ok(content) = std::fs::read_to_string(workspace.join(&relative)) else {
            continue;
        };
        for (pattern, regex) in &compiled {
            if pattern.allow_in.contains(&relative) {
                continue;
            }
            if regex.is_match(&content) {
                findings.push(Finding::new(
                    format!("secret-pattern:{}:{relative}", pattern.name),
                    Category::Secret,
                    format!("Secret pattern \"{}\" matched in {relative}", pattern.name),
                ));
            }
        }
    }
    Ok(findings)
}

/// `validation/index.ts:338-365`: source-like files, skip directories pruned, sorted for
/// stable output.
fn collect_text_files(workspace: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![PathBuf::new()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(workspace.join(&current)) else {
            continue;
        };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let relative = if current.as_os_str().is_empty() {
                PathBuf::from(&name)
            } else {
                current.join(&name)
            };
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                if !SCAN_SKIP_DIRS.contains(&name.as_str()) {
                    pending.push(relative);
                }
            } else if is_text_file(&name) {
                found.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    found.sort();
    found
}

/// `/\.(ts|tsx|js|jsx|json|md|yaml|yml|env\.example)$/i`.
fn is_text_file(name: &str) -> bool {
    let lower = name.to_lowercase();
    [
        ".ts",
        ".tsx",
        ".js",
        ".jsx",
        ".json",
        ".md",
        ".yaml",
        ".yml",
        ".env.example",
    ]
    .iter()
    .any(|suffix| lower.ends_with(suffix))
}

/// `validation/index.ts:279-336`: `validators/yarn.json`.
async fn commands(
    workspace: &Path,
    commands_path: &Path,
    install_cache_path: Option<&Path>,
) -> Result<(Vec<Finding>, Vec<Execution>), Error> {
    let config = read_json_config(commands_path)?;
    let mut findings = Vec::new();
    let mut results = Vec::new();
    for entry in config
        .get("commands")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(name), Some(command)) = (
            entry.get("name").and_then(Value::as_str),
            entry.get("command").and_then(Value::as_str),
        ) else {
            continue;
        };
        let timeout = entry
            .get("timeoutMs")
            .and_then(Value::as_u64)
            .map(std::time::Duration::from_millis);

        if name == "install"
            && let Some(cache) = install_cache_path
        {
            let current = install_fingerprint(workspace);
            let cached = std::fs::read_to_string(cache)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            if cached.as_deref() == Some(current.as_str()) {
                println!("[hanvil] Skipping yarn install (dependency fingerprint unchanged).");
                results.push(Execution {
                    command: command.to_string(),
                    args: Vec::new(),
                    exit_code: Some(0),
                    stdout: "skipped: dependency fingerprint unchanged".to_string(),
                    stderr: String::new(),
                    duration_ms: 0,
                    timed_out: false,
                    signal: None,
                    skipped: Some(true),
                    skip_reason: Some("fingerprint-unchanged".to_string()),
                });
                continue;
            }
        }

        let result = command::execute(Execute {
            command,
            args: &[],
            cwd: workspace,
            env: &std::collections::BTreeMap::new(),
            timeout,
            shell: true,
            stream_output: false,
        })
        .await
        .map_err(|source| Error::Spawn {
            name: name.to_string(),
            source,
        })?;
        let failed = result.exit_code != Some(0);
        let output = if result.stderr.is_empty() {
            result.stdout.clone()
        } else {
            result.stderr.clone()
        };
        results.push(result);
        if failed {
            findings.push(
                Finding::new(
                    format!("command:{name}"),
                    Category::Commands,
                    format!("Validation command failed: {name}"),
                )
                .with_details(truncate_details(&output)),
            );
            continue;
        }
        if name == "install"
            && let Some(cache) = install_cache_path
        {
            if let Some(parent) = cache.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(cache, format!("{}\n", install_fingerprint(workspace)));
        }
    }
    Ok((findings, results))
}

/// `installFingerprint.ts:8-39`: sha256 over `path\0content` of `yarn.lock`, `package.json`
/// and every `packages/*/package.json` that exists, in sorted path order.
pub(crate) fn install_fingerprint(workspace: &Path) -> String {
    let mut relative_paths: Vec<String> = FINGERPRINT_FILES
        .iter()
        .filter(|file| workspace.join(file).exists())
        .map(|file| (*file).to_string())
        .collect();
    if let Ok(entries) = std::fs::read_dir(workspace.join("packages")) {
        let mut packages: Vec<String> = entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .collect();
        packages.sort();
        for package in packages {
            let relative = format!("packages/{package}/package.json");
            if workspace.join(&relative).exists() {
                relative_paths.push(relative);
            }
        }
    }
    relative_paths.sort();
    let mut hasher = sha2::Sha256::new();
    for relative in relative_paths {
        let content = std::fs::read(workspace.join(&relative)).unwrap_or_default();
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        hasher.update(&content);
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hanvil-assert-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".harness/validators")).expect("mkdir");
        dir
    }

    fn write(dir: &Path, relative: &str, content: &str) {
        let path = dir.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir");
        }
        std::fs::write(path, content).expect("write");
    }

    fn spec_for(dir: &Path, extra: &str) -> Spec {
        let yaml = format!(
            "schemaVersion: 3\nname: t\nbaseline:\n  commands:\n    - name: install\n      command: \"true\"\n{extra}"
        );
        crate::harness::spec::parse(&yaml, &dir.join(".harness/spec.yaml"))
            .expect("spec")
            .spec
    }

    fn ids(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.id.as_str()).collect()
    }

    #[test]
    fn json_path_lookup_and_equality_follow_upstream() {
        let value: Value = serde_json::json!({"a": {"b": [1]}, "c": "x", "d": 1});
        assert_eq!(get_by_path(&value, "a.b"), Some(&serde_json::json!([1])));
        assert_eq!(get_by_path(&value, "a.b.0"), None);
        assert_eq!(get_by_path(&value, "missing.x"), None);
        assert!(values_equal(
            Some(&serde_json::json!(1)),
            Some(&serde_json::json!([1]))
        ));
        assert!(values_equal(
            Some(&serde_json::json!([1])),
            Some(&serde_json::json!([1]))
        ));
        assert!(!values_equal(
            Some(&serde_json::json!(2)),
            Some(&serde_json::json!([1]))
        ));
        assert!(values_equal(None, None));
        assert!(!values_equal(None, Some(&serde_json::json!("x"))));
        assert_eq!(stringify(None), "undefined");
        assert_eq!(
            stringify(Some(&serde_json::json!({"k": [1, "v"]}))),
            r#"{"k":[1,"v"]}"#
        );
    }

    #[test]
    fn the_install_fingerprint_covers_lockfile_manifest_and_packages() {
        let dir = workspace("fingerprint");
        assert_eq!(install_fingerprint(&dir), install_fingerprint(&dir));
        let empty = install_fingerprint(&dir);
        write(&dir, "package.json", "{}");
        let with_manifest = install_fingerprint(&dir);
        assert_ne!(empty, with_manifest);
        write(&dir, "packages/app/package.json", "{}");
        assert_ne!(with_manifest, install_fingerprint(&dir));
        write(&dir, "packages/app/index.js", "// not fingerprinted");
        let stable = install_fingerprint(&dir);
        write(&dir, "packages/app/index.js", "// still not");
        assert_eq!(stable, install_fingerprint(&dir));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn every_gate_reports_with_upstream_ids_and_messages() {
        let dir = workspace("gates");
        write(
            &dir,
            "package.json",
            r#"{"name": "app", "keywords": ["one"]}"#,
        );
        write(&dir, "broken.json", "{not json");
        write(&dir, "README.md", "hello world");
        write(
            &dir,
            "src/config.ts",
            "OPERATOR_KEY=0xabcdef0123456789abcdef0123456789abcdef01\n",
        );
        write(
            &dir,
            "docs/allowed.md",
            "PRIVATE_KEY=0xabcdef0123456789abcdef0123456789abcdef01",
        );
        write(
            &dir,
            "node_modules/x/leak.js",
            "PRIVATE_KEY=0xabcdef0123456789abcdef0123456789abcdef01",
        );
        write(&dir, ".env", "x=1");
        write(
            &dir,
            ".harness/validators/static.json",
            r#"{
              "jsonAssertions": [
                {"file": "package.json", "path": "name", "equals": "app"},
                {"file": "package.json", "path": "keywords", "equals": "one"},
                {"file": "package.json", "path": "missing.deep", "equals": "x"},
                {"file": "absent.json", "path": "a", "equals": 1},
                {"file": "broken.json", "path": "a", "equals": 1}
              ],
              "fileAssertions": {"required": ["README.md", "AGENTS.md"], "forbidden": ["src", "nope"]},
              "textAssertions": [
                {"file": "README.md", "contains": ["hello", "absent needle"]},
                {"file": "MISSING.md", "contains": ["x"]}
              ],
              "secretScan": {"patterns": [{"name": "hello-word", "pattern": "^hello", "allowIn": []}]}
            }"#,
        );
        write(
            &dir,
            ".harness/validators/yarn.json",
            r#"{"commands": [
              {"name": "install", "command": "printf installed"},
              {"name": "lint", "command": "printf 'lint broke' >&2; exit 2", "timeoutMs": 5000},
              {"name": "build", "command": "true"}
            ]}"#,
        );
        let spec = spec_for(
            &dir,
            "requiredFiles: [README.md, LICENSE]\nsecretScan:\n  patterns:\n    - name: private-key-assignment\n      pattern: \"(PRIVATE_KEY|OPERATOR_KEY)\\\\s*=\\\\s*(0x)?[0-9a-fA-F]{32,}\"\n      allowIn: [docs/allowed.md]\n",
        );
        let cache = dir.join(".harness/runs/r/cache/install-fingerprint.txt");
        let result = run(&dir, &spec, Some(&cache)).await.expect("assert runs");
        assert!(!result.passed);
        assert_eq!(
            ids(&result.findings),
            vec![
                "required-file:LICENSE",
                "forbidden-file:.env",
                // The tolerance runs one way: a scalar where the spec expects a one-item
                // array passes; an array where the spec expects a scalar does not.
                "json:package.json:keywords",
                "json:package.json:missing.deep",
                "json-parse:broken.json",
                "static-required:AGENTS.md",
                "static-forbidden:src",
                "text:README.md:absent needle",
                "text-missing:MISSING.md",
                "secret-pattern:hello-word:README.md",
                "secret-file:.env",
                "secret-pattern:private-key-assignment:src/config.ts",
                "command:lint",
            ]
        );
        let by_id = |id: &str| result.findings.iter().find(|f| f.id == id).expect(id);
        assert_eq!(
            by_id("required-file:LICENSE").message,
            "Required file is missing: LICENSE"
        );
        assert_eq!(
            by_id("json:package.json:missing.deep").details.as_deref(),
            Some("Expected \"x\" but found undefined")
        );
        assert_eq!(
            by_id("json:package.json:keywords").details.as_deref(),
            Some("Expected \"one\" but found [\"one\"]")
        );
        assert_eq!(
            by_id("text:README.md:absent needle").message,
            "Expected README.md to contain \"absent needle\""
        );
        assert_eq!(
            by_id("command:lint").message,
            "Validation command failed: lint"
        );
        assert_eq!(by_id("command:lint").details.as_deref(), Some("lint broke"));
        assert_eq!(by_id("command:lint").category, Category::Commands);
        assert_eq!(result.command_results.len(), 3);
        assert_eq!(result.command_results[0].stdout, "installed");
        assert!(
            cache.exists(),
            "fingerprint cached after a successful install"
        );
        assert!(!is_ready_for_smoke(&result.findings));

        // Second run: install is skipped on an unchanged fingerprint.
        let again = run(&dir, &spec, Some(&cache)).await.expect("assert runs");
        assert_eq!(again.command_results[0].skipped, Some(true));
        assert_eq!(
            again.command_results[0].skip_reason.as_deref(),
            Some("fingerprint-unchanged")
        );
        assert_eq!(
            again.command_results[0].stdout,
            "skipped: dependency fingerprint unchanged"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn a_clean_workspace_passes_and_config_errors_are_loud() {
        let dir = workspace("clean");
        write(&dir, ".harness/validators/static.json", "{}");
        write(&dir, ".harness/validators/yarn.json", r#"{"commands": []}"#);
        let spec = spec_for(&dir, "");
        let result = run(&dir, &spec, None).await.expect("assert runs");
        assert!(result.passed);
        assert!(result.findings.is_empty());
        assert!(is_ready_for_smoke(&[Finding::new(
            "generator-exit:1",
            Category::Agent,
            "x"
        )]));

        write(&dir, ".harness/validators/static.json", "{nope");
        let error = run(&dir, &spec, None).await.expect_err("loud");
        assert!(matches!(error, Error::Parse { .. }), "{error}");
        write(&dir, ".harness/validators/static.json", "{}");
        let bad_pattern = spec_for(
            &dir,
            "secretScan:\n  patterns:\n    - name: broken\n      pattern: \"(\"\n",
        );
        let error = run(&dir, &bad_pattern, None).await.expect_err("loud");
        assert!(
            error
                .to_string()
                .starts_with("secret pattern \"broken\" does not compile"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
