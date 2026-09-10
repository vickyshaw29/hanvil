//! `hanvil doctor` end to end: the CI fixture recipe in a fresh git repository, and a recipe
//! that cannot load.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Copy `tests/harness/` into a fresh git repository with one commit, as the CI job does.
fn fixture_repo(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hanvil-doctor-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/harness");
    copy_dir(&source, &root);
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "doctor@hanvil"],
        vec!["config", "user.name", "doctor"],
        vec!["add", "-A"],
        vec!["commit", "-q", "--no-gpg-sign", "-m", "fixture"],
    ] {
        let status = Command::new("git")
            .args(&args)
            .current_dir(&root)
            .status()
            .expect("git runs");
        assert!(status.success(), "git {args:?} failed");
    }
    root
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("readdir") {
        let entry = entry.expect("entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy");
        }
    }
}

fn doctor(args: &[&str], cwd: &Path) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .arg("doctor")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("hanvil runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
    )
}

#[test]
fn the_fixture_recipe_is_ready_to_run_with_an_empty_environment() {
    let repo = fixture_repo("ready");
    let (ok, report) = doctor(&[], &repo);
    assert!(ok, "{report}");
    let expected = [
        "  ✔ node — v",
        "  ✔ git — on PATH",
        "  ✔ git repo — on main",
        "(schema v3)",
        "  ✔ agent (claude) — bash on PATH",
        "  ✔ package manager — npm on PATH",
        "  ✔ prd — present",
        "  ✔ validators.static — present",
        "  ✔ validators.commands — present",
        "  ✔ prompts — using bundled prompts",
        "  ✔ chain — in-process hanvil at http://localhost:7546, http://localhost:5551, localhost:50211",
        "\nReady to run.",
    ];
    for line in expected {
        assert!(report.contains(line), "missing {line:?} in:\n{report}");
    }
    assert!(!report.contains('✘'), "{report}");
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn recipe_only_reports_one_check_and_a_broken_recipe_fails() {
    let repo = fixture_repo("recipe");
    let (ok, report) = doctor(&[".harness/spec.yaml", "--recipe-only"], &repo);
    assert!(ok, "{report}");
    assert_eq!(report.lines().filter(|l| l.starts_with("  ✔")).count(), 1);
    assert!(report.ends_with("Ready to run.\n"), "{report}");

    std::fs::write(
        repo.join(".harness/spec.yaml"),
        "schemaVersion: 3\nname: broken\nlogging: x\n",
    )
    .expect("write");
    let (ok, report) = doctor(&[], &repo);
    assert!(!ok);
    assert!(report.contains("uses removed key(s): logging."), "{report}");
    assert!(report.contains("  ✘ recipe — "), "{report}");
    assert!(
        report.contains("Fix the recipe, or bootstrap one with `hanvil init`."),
        "{report}"
    );
    assert!(report.contains("  ✔ git repo — on main"), "{report}");
    assert!(
        report.ends_with("1 check(s) failed — `run` would not get past preflight.\n"),
        "{report}"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// The recipe names `bash` as its generator so the agent check does not depend on `claude`
/// being on the runner's PATH; the three failures are the ones the recipe was written to have.
#[test]
fn a_missing_prd_and_a_testnet_recipe_are_named() {
    let repo = fixture_repo("missing");
    std::fs::write(
        repo.join(".harness/spec.yaml"),
        "schemaVersion: 3\nname: t\nprd: nowhere.md\ngenerator:\n  provider: command\n  command: bash\nvalidator:\n  enabled: true\nchainValidation:\n  network: testnet\n  operator: {accountIdEnv: A, privateKeyEnv: B}\nbaseline:\n  commands:\n    - name: install\n      command: \"true\"\n",
    )
    .expect("write");
    let (ok, report) = doctor(&[], &repo);
    assert!(!ok);
    assert!(report.contains("  ✘ prd — missing: "), "{report}");
    assert!(
        report.contains("  ✘ eval — `validator.enabled` is set but `eval` is not"),
        "{report}"
    );
    assert!(
        report.contains("  ✘ chain — network: testnet is not supported by hanvil run"),
        "{report}"
    );
    assert!(report.contains("3 check(s) failed"), "{report}");
    let _ = std::fs::remove_dir_all(repo);
}

/// The browser probe needs `npx` and a Chromium or Chrome; without them the test says so.
fn browser_available() -> bool {
    let npx = Command::new("sh")
        .args(["-c", "command -v npx"])
        .output()
        .is_ok_and(|o| o.status.success());
    let home = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let chromium = home.join("Library/Caches/ms-playwright").exists()
        || home.join(".cache/ms-playwright").exists();
    let chrome = Command::new("sh")
        .args(["-c", "command -v google-chrome || command -v google-chrome-stable || test -d '/Applications/Google Chrome.app'"])
        .output()
        .is_ok_and(|o| o.status.success());
    npx && (chromium || chrome)
}

#[test]
fn a_recipe_with_a_smoke_gate_gets_its_browser_probed() {
    if !browser_available() {
        eprintln!("skipped: the browser probe needs npx and a Chromium or Chrome on this machine");
        return;
    }
    let root = std::env::temp_dir().join(format!("hanvil-doctor-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/harness-smoke"),
        &root,
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "doctor@hanvil"],
        vec!["config", "user.name", "doctor"],
        vec!["add", "-A"],
        vec!["commit", "-q", "--no-gpg-sign", "-m", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(&root)
                .status()
                .expect("git")
                .success()
        );
    }
    let (ok, report) = doctor(&[], &root);
    assert!(ok, "{report}");
    assert!(
        report.contains("  ✔ validators.playwright — present"),
        "{report}"
    );
    // EVALUATE is off in this recipe, so the SMOKE-only browser check runs and navigates.
    assert!(report.contains("  ✔ SMOKE browser — "), "{report}");
    assert!(report.ends_with("Ready to run.\n"), "{report}");
    let _ = std::fs::remove_dir_all(root);
}
