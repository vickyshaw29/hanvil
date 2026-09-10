//! `hanvil run` end to end with the CI fixture and a fake agent: the node boots in-process, the
//! signer is provisioned and swept, the chain is snapshotted per attempt, and every artifact the
//! TypeScript harness writes is written.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

/// Copy `tests/harness/` into a fresh git repository with one commit, as the CI job does.
fn fixture_repo(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hanvil-run-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/harness"),
        &root,
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "run@hanvil"],
        vec!["config", "user.name", "run"],
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

fn git_stdout(args: &[&str], cwd: &Path) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git runs");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// `hanvil run` on random ports with an empty environment; returns exit success and stdout.
fn run(args: &[&str], cwd: &Path) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        // --no-skills: the tests must not clone hedera-skills from the network.
        .args([
            "run",
            "--no-skills",
            "--port",
            "0",
            "--mirror-port",
            "0",
            "--grpc-port",
            "0",
        ])
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("hanvil runs");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn jsonl_events(repo: &Path) -> Vec<Value> {
    std::fs::read_to_string(repo.join(".harness/runs/harness.log.jsonl"))
        .expect("jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).expect("json line"))
        .collect()
}

fn event<'a>(events: &'a [Value], kind: &str) -> &'a Value {
    events
        .iter()
        .find(|e| e["type"] == kind)
        .unwrap_or_else(|| panic!("no {kind} event in {events:?}"))
}

fn run_directory(repo: &Path) -> PathBuf {
    let runs = repo.join(".harness/runs");
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&runs)
        .expect("runs")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs.pop().expect("a run directory")
}

#[test]
fn the_fixture_passes_in_one_attempt_with_the_signer_swept_and_the_chain_dumped() {
    let repo = fixture_repo("pass");
    let (ok, stdout, stderr) = run(&["--max-attempts", "2"], &repo);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");

    for line in [
        "[hanvil] Stage 1/5 GENERATE — attempt 1 [opus]",
        "[hanvil] Chain snapshot taken — attempt 1 — 0x0",
        "[hanvil] Stage 2/5 ASSERT",
        "[hanvil] Stage 3/5 CHAIN",
        "[hanvil] Attempt 1 PASSED — deterministic gates passed",
        "[hanvil] Workspace committed — harness: run attempt 1 passed @ ",
        "[hanvil] Chain signer swept — 0.0.",
        "Run PASSED",
        "findings=0 open",
        "workingTree=clean (consumer-relevant)",
        "The harness did not push, open a PR, merge, delete a branch, or switch branches.",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    assert!(stderr.is_empty(), "{stderr}");

    let branch = git_stdout(&["branch", "--show-current"], &repo);
    assert!(
        branch.starts_with("harness/run-chain-on-hanvil-"),
        "{branch}"
    );
    assert_eq!(
        git_stdout(&["log", "-1", "--format=%s"], &repo),
        "harness: run attempt 1 passed"
    );
    let committed = git_stdout(&["show", "--stat", "--format=", "HEAD"], &repo);
    assert!(committed.contains("generated.txt"), "{committed}");
    assert!(!committed.contains("chain-signer"), "{committed}");
    // The copy carries no .gitignore, so run artifacts are untracked; nothing else may be.
    let leftover: Vec<String> = git_stdout(&["status", "--porcelain"], &repo)
        .lines()
        .filter(|line| !line.ends_with(".harness/runs/"))
        .map(str::to_string)
        .collect();
    assert!(leftover.is_empty(), "{leftover:?}");

    let events = jsonl_events(&repo);
    let kinds: Vec<&str> = events.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "run_started",
            "chain_signer_provisioned",
            "context_vendored",
            "generator_started",
            "chain_snapshot_taken",
            "generator_finished",
            "chain_assertions_finished",
            "validation_finished",
            "chain_state_written",
            "workspace_git_committed",
            "run_finished",
            "chain_signer_swept",
        ]
    );
    let provisioned = event(&events, "chain_signer_provisioned");
    assert_eq!(provisioned["network"], "local");
    assert_eq!(provisioned["reused"], false);
    // The README quotes these; they must come out of the log, not be claimed.
    for kind in [
        "chain_signer_provisioned",
        "chain_snapshot_taken",
        "chain_state_written",
    ] {
        let micros = &event(&events, kind)["durationMicros"];
        assert!(
            micros.is_u64(),
            "{kind} carries no durationMicros: {micros}"
        );
    }
    let account = provisioned["accountId"].as_str().expect("account id");
    assert!(account.starts_with("0.0.10"), "{account}");
    assert_eq!(event(&events, "chain_signer_swept")["success"], true);
    assert_eq!(event(&events, "chain_signer_swept")["accountId"], account);
    assert_eq!(event(&events, "generator_finished")["exitCode"], 0);
    assert_eq!(event(&events, "validation_finished")["passed"], true);
    assert_eq!(event(&events, "workspace_git_committed")["committed"], true);
    assert_eq!(event(&events, "run_finished")["passed"], true);

    let run_dir = run_directory(&repo);
    for relative in [
        "layout.json",
        "session.json",
        "status.json",
        "prompts/generator-attempt-1.txt",
        "logs/generator-attempt-1.log",
        "logs/generator-attempt-1.activity.log",
        "logs/validation-attempt-1.json",
        "logs/chain-state-attempt-1.json",
        "reports/report.json",
    ] {
        assert!(
            run_dir.join(relative).exists(),
            "missing {relative} in {}",
            run_dir.display()
        );
    }
    assert!(
        !run_dir.join("chain-signer.json").exists(),
        "the signer file is removed by the sweep"
    );
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("reports/report.json")).expect("report"),
    )
    .expect("json");
    assert_eq!(report["passed"], true);
    assert_eq!(report["attempts"], 1);
    assert_eq!(report["maxAttempts"], 2);
    assert_eq!(report["validation"]["findings"], serde_json::json!([]));
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("session.json")).expect("session"),
    )
    .expect("json");
    assert_eq!(session["gateStatus"], "passed");
    assert_eq!(session["lastAttempt"], 1);
    assert_eq!(session["branch"], Value::String(branch.clone()));
    // macOS: the workspace comes back as /private/var/…, temp_dir() says /var/….
    let recorded = PathBuf::from(session["chainStatePath"].as_str().expect("chainStatePath"));
    assert_eq!(
        recorded.canonicalize().expect("recorded dump exists"),
        run_dir
            .join("logs/chain-state-attempt-1.json")
            .canonicalize()
            .expect("dump exists")
    );
    let prompt =
        std::fs::read_to_string(run_dir.join("prompts/generator-attempt-1.txt")).expect("prompt");
    assert!(prompt.contains("## Local Hedera network"), "{prompt}");
    assert!(
        prompt.contains(&format!(
            "A funded ECDSA account is provisioned for this run: {account}"
        )),
        "{prompt}"
    );

    // The dump boots a node in exactly the state the attempt left: the swept signer is gone
    // but the record of it is there.
    let dump = run_dir.join("logs/chain-state-attempt-1.json");
    let state = std::fs::read_to_string(&dump).expect("dump");
    assert!(
        state.contains("hanvil run signer"),
        "the signer's create record is in the dump"
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_dirty_tree_and_conflicting_flags_are_refused_with_upstreams_words() {
    let repo = fixture_repo("refuse");
    let (ok, _stdout, stderr) = run(&["--new", "--continue", "harness/run-x-ab"], &repo);
    assert!(!ok);
    assert!(
        stderr.contains("cannot be used with")
            || stderr.contains("Cannot pass both --new and --continue."),
        "{stderr}"
    );

    std::fs::write(repo.join("scratch.txt"), "dirty").expect("write");
    let (ok, _stdout, stderr) = run(&[], &repo);
    assert!(!ok);
    assert!(
        stderr.contains("Harness run requires a completely clean working tree (no auto-stash).\nCommit or discard local changes, then re-run.\n?? scratch.txt"),
        "{stderr}"
    );
    assert_eq!(
        git_stdout(&["branch", "--show-current"], &repo),
        "main",
        "no branch was created"
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_failed_attempt_reverts_the_chain_under_the_repair() {
    let repo = fixture_repo("revert");
    let (ok, stdout, stderr) = run(&[".harness/spec-repair.yaml", "--max-attempts", "3"], &repo);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");

    for line in [
        "[hanvil] Stage 3/5 CHAIN — skipped — deterministic gates are not clean",
        "[hanvil] Attempt 1 FAILED — 1 open, 1 new",
        "[hanvil] Chain reverted — attempt 2 starts on the state attempt 1 started on",
        "[hanvil] Stage 1/5 GENERATE — repair, attempt 2 [opus, escalated — last attempt fixed nothing]",
        "[hanvil] Chain assertions — 1 of 1 passed",
        "[hanvil] Attempt 2 PASSED — deterministic gates passed",
        "Run PASSED",
        "attempts=2/3",
        "findings=0 open, 1 fixed",
        "Closed by the last attempt (1):",
        "- required-file:generated.txt",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }

    let events = jsonl_events(&repo);
    let reverted = event(&events, "chain_snapshot_reverted");
    assert_eq!(reverted["attempt"], 1);
    assert_eq!(reverted["snapshotId"], "0x0");
    assert_eq!(reverted["success"], true);
    assert!(reverted["durationMicros"].is_u64(), "{reverted}");
    let taken: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "chain_snapshot_taken")
        .collect();
    assert_eq!(taken.len(), 2);
    assert_eq!(
        taken[1]["snapshotId"], "0x1",
        "ids are never reused after a revert"
    );
    let signer = event(&events, "chain_signer_provisioned")["accountId"]
        .as_str()
        .expect("account id")
        .trim_start_matches("0.0.")
        .to_string();

    let run_dir = run_directory(&repo);
    let dump = |attempt: u32| -> Value {
        serde_json::from_str(
            &std::fs::read_to_string(
                run_dir.join(format!("logs/chain-state-attempt-{attempt}.json")),
            )
            .expect("dump"),
        )
        .expect("json")
    };
    // Attempt 1 drained the signer; attempt 2 ran on the reverted chain with it funded again.
    assert_eq!(dump(1)["accounts"][&signer]["balance"], 0);
    assert_eq!(dump(2)["accounts"][&signer]["balance"], 1_000_000_000);

    let repair = std::fs::read_to_string(run_dir.join("prompts/repair-attempt-2.txt"))
        .expect("repair prompt");
    assert!(
        repair.contains("The local chain was reset to the state before your previous attempt"),
        "{repair}"
    );
    assert!(repair.contains("Repair scope: **broad**"), "{repair}");
    assert!(
        repair.contains("- [files] Required file is missing: generated.txt"),
        "{repair}"
    );
    assert!(
        !repair.contains("[chain]"),
        "CHAIN did not run on attempt 1, so no chain finding: {repair}"
    );
    assert_eq!(
        git_stdout(&["log", "--format=%s", "-2"], &repo),
        "harness: run attempt 2 passed\nharness: run attempt 1 failed"
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn a_chain_assertion_the_app_cannot_meet_fails_the_run_with_a_runtime_repair() {
    let repo = fixture_repo("assert");
    let (ok, stdout, stderr) = run(&[".harness/spec-assert.yaml", "--max-attempts", "2"], &repo);
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        "[hanvil] Chain assertions — 0 of 1 passed",
        "[hanvil] Stage 4/5 SMOKE — skipped — chain assertions failed",
        "[hanvil] Attempt 1 FAILED — 1 open",
        "[hanvil] Attempt 2 FAILED — 1 open",
        "Run FAILED",
        "attempts=2/2",
        "findings=1 open",
        "Open findings:",
        "- [chain] chain:0:account: Chain assertion 0 (account) failed: account 0.0.",
        "holds 10 ℏ, below the required 100 ℏ",
        "  hanvil run .harness/spec-assert.yaml --new",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    let run_dir = run_directory(&repo);
    let repair = std::fs::read_to_string(run_dir.join("prompts/repair-attempt-2.txt"))
        .expect("repair prompt");
    assert!(repair.contains("Repair scope: **runtime**"), "{repair}");
    assert!(
        repair.contains("- [chain] Chain assertion 0 (account) failed"),
        "{repair}"
    );
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("reports/report.json")).expect("report"),
    )
    .expect("json");
    assert_eq!(report["passed"], false);
    assert_eq!(
        report["openFindingIds"],
        serde_json::json!(["chain:0:account"])
    );
    let session: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("session.json")).expect("session"),
    )
    .expect("json");
    assert_eq!(session["gateStatus"], "failed");
    let _ = std::fs::remove_dir_all(repo);
}

/// The SMOKE gate needs `npx` and a browser; without them the test says so instead of passing.
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
fn the_smoke_gate_walks_the_routes_and_names_the_forbidden_text() {
    if !browser_available() {
        eprintln!("skipped: the SMOKE gate needs npx and a Chromium or Chrome on this machine");
        return;
    }
    let repo = std::env::temp_dir().join(format!("hanvil-run-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/harness-smoke"),
        &repo,
    );
    for args in [
        vec!["init", "-q", "-b", "main"],
        vec!["config", "user.email", "run@hanvil"],
        vec!["config", "user.name", "run"],
        vec!["add", "-A"],
        vec!["commit", "-q", "--no-gpg-sign", "-m", "fixture"],
    ] {
        assert!(
            Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status()
                .expect("git")
                .success()
        );
    }
    let (ok, stdout, stderr) = run(&["--max-attempts", "1"], &repo);
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        "[hanvil] Stage 4/5 SMOKE — booting dev server",
        "[hanvil:runtime:server] Local: http://127.0.0.1:47391",
        "[hanvil] Attempt 1 FAILED — 3 open",
        "- [playwright] playwright:route:broken:forbidden:application-error: Playwright gate route /broken contains forbidden text: \"Application error\"",
        "- [playwright] playwright:route:missing:status: Playwright gate route /nope returned HTTP 404",
        // Chrome logs the 404 document as a console error, as Playwright's listener reports it.
        "- [playwright] playwright:route:missing:console: Playwright gate route /nope logged browser console errors",
    ] {
        assert!(
            stdout.contains(line),
            "missing {line:?} in:\n{stdout}\n{stderr}"
        );
    }
    let run_dir = run_directory(&repo);
    let gate: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("logs/playwright-gate-attempt-1.json"))
            .expect("gate json"),
    )
    .expect("json");
    assert_eq!(gate["passed"], false);
    assert_eq!(gate["serverUrl"], "http://127.0.0.1:47391");
    assert_eq!(gate["serverCommand"], "node server.js");
    let routes = gate["routes"].as_array().expect("routes");
    assert_eq!(routes.len(), 3);
    assert_eq!(routes[0]["name"], "home");
    assert_eq!(routes[0]["statusCode"], 200);
    assert_eq!(routes[0]["rendered"], true);
    assert_eq!(routes[0]["forbiddenTextFound"], serde_json::json!([]));
    assert_eq!(
        routes[1]["forbiddenTextFound"],
        serde_json::json!(["Application error"])
    );
    assert_eq!(routes[2]["statusCode"], 404);
    // The dev server was stopped with the attempt.
    assert!(
        std::net::TcpStream::connect("127.0.0.1:47391").is_err(),
        "the dev server is still listening"
    );
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn validate_runs_assert_alone_and_reports_like_upstream() {
    let repo = fixture_repo("validate");
    let output = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .args(["validate"])
        .current_dir(&repo)
        .output()
        .expect("hanvil runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{stdout}");
    assert_eq!(
        stdout.trim(),
        "Validation finished\npassed=false\nfindings=1\n- Required file is missing: generated.txt"
    );
    assert_eq!(
        git_stdout(&["branch", "--show-current"], &repo),
        "main",
        "validate makes no branch"
    );
    assert!(
        !repo.join(".harness/runs").exists(),
        "validate writes no run"
    );

    std::fs::write(repo.join("generated.txt"), "ok\n").expect("write");
    let output = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .args(["validate", ".harness/spec.yaml"])
        .current_dir(&repo)
        .output()
        .expect("hanvil runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "Validation finished\npassed=true\nfindings=0"
    );
    let _ = std::fs::remove_dir_all(repo);
}
