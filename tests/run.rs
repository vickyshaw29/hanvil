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

/// D13: Ctrl-C during GENERATE stops the agent's process group, marks the run interrupted,
/// exits 130, and leaves neither the signer's key file nor the runtime directories behind.
#[test]
fn ctrl_c_stops_the_agent_and_cleans_the_workspace() {
    let repo = fixture_repo("ctrlc");
    // Outside the workspace: an untracked file inside it would make `run` refuse the tree.
    let stdout_path =
        std::env::temp_dir().join(format!("hanvil-run-ctrlc-{}.stdout", std::process::id()));
    let stdout_file = std::fs::File::create(&stdout_path).expect("stdout file");
    let mut child = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .args([
            "run",
            ".harness/spec-slow.yaml",
            "--no-skills",
            "--port",
            "0",
            "--mirror-port",
            "0",
            "--grpc-port",
            "0",
        ])
        .current_dir(&repo)
        .stdout(stdout_file)
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("hanvil starts");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let stdout = std::fs::read_to_string(&stdout_path).unwrap_or_default();
        if stdout.contains("[hanvil] Stage 1/5 GENERATE") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "GENERATE never started:\n{stdout}"
        );
        assert!(
            child.try_wait().expect("wait").is_none(),
            "hanvil exited before GENERATE:\n{stdout}"
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let killed = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill runs");
    assert!(killed.success());
    let status = child.wait().expect("hanvil exits");
    assert_eq!(status.code(), Some(130), "{status:?}");

    let stdout = std::fs::read_to_string(&stdout_path).expect("stdout");
    for line in [
        "[hanvil] interrupted — stopping the agent, the dev server and the browser",
        "[hanvil] stopped 1 process group(s)",
        "[hanvil] Run runtime cleaned — ",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    let run_dir = run_directory(&repo);
    let status_json: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("status.json")).expect("status.json"),
    )
    .expect("json");
    assert_eq!(status_json["phase"], "interrupted");
    assert!(
        !run_dir.join("chain-signer.json").exists(),
        "the signer's key file outlived the run"
    );
    assert!(
        !repo.join(".harness/runtime").exists(),
        "runtime dir left behind"
    );
    // The fake agent was `bash -c "sleep 5959"` in its own process group.
    std::thread::sleep(std::time::Duration::from_secs(1));
    let survivors = Command::new("pgrep")
        .args(["-f", "sleep 5959"])
        .output()
        .expect("pgrep runs");
    assert!(
        String::from_utf8_lossy(&survivors.stdout).trim().is_empty(),
        "the agent survived Ctrl-C: {}",
        String::from_utf8_lossy(&survivors.stdout)
    );
    let _ = std::fs::remove_file(stdout_path);
    let _ = std::fs::remove_dir_all(repo);
}

#[test]
fn continue_reloads_the_chain_the_failed_cycle_left() {
    let repo = fixture_repo("continue");
    let (ok, stdout, stderr) = run(&[".harness/spec-repair.yaml", "--max-attempts", "1"], &repo);
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(stdout.contains("Run FAILED"), "{stdout}");
    let branch = git_stdout(&["rev-parse", "--abbrev-ref", "HEAD"], &repo);
    assert!(
        branch.starts_with("harness/run-repair-on-hanvil-"),
        "{branch}"
    );
    let first_cycle = jsonl_events(&repo);
    let first_signer = event(&first_cycle, "chain_signer_provisioned")["accountId"]
        .as_str()
        .expect("account id")
        .to_string();
    let dump = event(&first_cycle, "chain_state_written")["path"]
        .as_str()
        .expect("dump path")
        .to_string();

    let (ok, stdout, stderr) = run(
        &[
            ".harness/spec-repair.yaml",
            "--continue",
            &branch,
            "--max-attempts",
            "2",
        ],
        &repo,
    );
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        &format!("[hanvil] Chain reloaded — {dump}"),
        "[hanvil] Run continued",
        "[hanvil] Chain assertions — 1 of 1 passed",
        "Run PASSED",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }

    // The reloaded chain still holds the first cycle's signer, which the fake agent drained, so
    // the second cycle's signer takes the next id: the chain came from the dump, not genesis.
    let events = jsonl_events(&repo);
    let provisioned: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "chain_signer_provisioned")
        .collect();
    assert_eq!(provisioned.len(), 2);
    assert_eq!(provisioned[0]["accountId"], first_signer.as_str());
    assert_ne!(provisioned[1]["accountId"], first_signer.as_str());
    let second_dump = events
        .iter()
        .rfind(|e| e["type"] == "chain_state_written")
        .expect("second dump")["path"]
        .as_str()
        .expect("path")
        .to_string();
    let chain: Value =
        serde_json::from_str(&std::fs::read_to_string(&second_dump).expect("dump")).expect("json");
    // `accounts` is keyed by entity number, `"1032"` for `0.0.1032`.
    let number = first_signer.rsplit('.').next().expect("entity number");
    assert!(
        chain["accounts"][number].is_object(),
        "first signer {first_signer} not in the second cycle's dump: {:?}",
        chain["accounts"]
            .as_object()
            .map(|a| a.keys().collect::<Vec<_>>())
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

/// The ledger is the half of the chain a mirror node does not have. An app sends three
/// transfers, the node refuses two of them for a value that is not a whole tinybar, and the
/// finding names that rather than only the count that came up short.
#[test]
fn a_refused_transaction_reaches_the_finding_the_prompt_and_the_artifact() {
    let repo = fixture_repo("ledger");
    let (ok, stdout, stderr) = run(&[".harness/spec-ledger.yaml", "--max-attempts", "1"], &repo);
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        "[hanvil] Chain ledger — attempt 1 — 3 transaction(s), 2 rejected before consensus",
        "1  ETHEREUMTRANSACTION  0.0.1002  SUCCESS",
        "REJECTED Invalid params: 1 weibar is not a multiple of 10^10 (1…",
        "[hanvil] Chain assertions — 0 of 1 passed",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }

    let run_dir = run_directory(&repo);
    let validation: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("logs/validation-attempt-1.json"))
            .expect("validation"),
    )
    .expect("json");
    let details = validation["findings"][0]["details"]
        .as_str()
        .expect("the finding carries the cause");
    assert!(
        details.starts_with("2 ETHEREUMTRANSACTION submission(s) were refused before consensus"),
        "{details}"
    );
    assert!(
        details.ends_with("— no record exists for them on any Hedera network"),
        "{details}"
    );

    // The artifact keeps every row, unclipped, with the fee a refusal did not cost.
    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("logs/chain-ledger-attempt-1.json")).expect("ledger"),
    )
    .expect("json");
    let entries = ledger["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0]["outcome"], "success");
    assert!(entries[0]["feeTinybar"].as_u64().expect("fee") > 0);
    assert_eq!(entries[2]["outcome"], "rejected");
    assert_eq!(entries[2]["feeTinybar"], 0);
    assert_eq!(
        entries[2]["result"],
        "Invalid params: 1 weibar is not a multiple of 10^10 (1 tinybar); the relay rejects such values"
    );

    // The chain the attempt left says the same thing from the other side: one transaction to
    // read back, and two refusals that only the node kept.
    let state: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("logs/chain-state-attempt-1.json")).expect("state"),
    )
    .expect("json");
    assert_eq!(
        state["txs"].as_object().expect("txs").len(),
        1,
        "a refused transaction is not a transaction"
    );
    assert_eq!(state["rejections"].as_array().expect("rejections").len(), 2);
    let _ = std::fs::remove_dir_all(repo);
}

/// The gate a mirror-node harness cannot offer: the run fails on the refusals alone, with no
/// assertion about what the app was supposed to achieve.
#[test]
fn a_refusal_fails_the_run_on_its_own() {
    let repo = fixture_repo("rejections");
    let (ok, stdout, stderr) = run(
        &[".harness/spec-rejections.yaml", "--max-attempts", "1"],
        &repo,
    );
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("[hanvil] Chain assertions — 0 of 1 passed"),
        "{stdout}"
    );
    let run_dir = run_directory(&repo);
    let validation: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("logs/validation-attempt-1.json"))
            .expect("validation"),
    )
    .expect("json");
    assert_eq!(validation["findings"][0]["id"], "chain:0:rejections");

    // The four counts CI can assert on, without parsing the rows.
    let report: Value = serde_json::from_str(
        &std::fs::read_to_string(run_dir.join("reports/report.json")).expect("report"),
    )
    .expect("json");
    assert_eq!(
        report["chainLedger"],
        serde_json::json!({
            "transactions": 3,
            "succeeded": 1,
            "failed": 0,
            "rejected": 2,
        })
    );
    let message = validation["findings"][0]["message"]
        .as_str()
        .expect("message");
    assert!(
        message.contains(
            "the node refused 2 ETHEREUMTRANSACTION submission(s), more than the 0 allowed"
        ),
        "{message}"
    );
    assert!(
        message.ends_with("a refused transaction leaves no record on any Hedera network"),
        "{message}"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// A repair that fixes the symptom and leaves the cause sends the same refused transaction
/// again. Nothing else in the run would say so.
#[test]
fn a_repair_that_changed_nothing_is_told_the_refusal_repeated() {
    let repo = fixture_repo("repeated");
    let (ok, stdout, stderr) = run(
        &[".harness/spec-rejections.yaml", "--max-attempts", "3"],
        &repo,
    );
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains(
            "[hanvil] Chain refusals repeated — 2× ETHEREUMTRANSACTION — Invalid params: 1 weibar"
        ),
        "{stdout}"
    );

    let run_dir = run_directory(&repo);
    // Attempt 2's repair had no earlier attempt to compare against; attempt 3's does.
    let second =
        std::fs::read_to_string(run_dir.join("prompts/repair-attempt-2.txt")).expect("repair 2");
    assert!(!second.contains("refused the same way"), "{second}");
    let third =
        std::fs::read_to_string(run_dir.join("prompts/repair-attempt-3.txt")).expect("repair 3");
    assert!(
        third.contains(
            "The attempt before this one was refused the same way (2× ETHEREUMTRANSACTION"
        ),
        "{third}"
    );
    assert!(
        third.contains("Change how the transaction is built, not what the page shows."),
        "{third}"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// A deploy command that fails returns before the assertions. The ledger is printed anyway,
/// because a refusal is usually why the command failed.
#[test]
fn a_failed_deploy_command_still_shows_what_the_chain_did() {
    let repo = fixture_repo("deploy-fails");
    let (ok, stdout, stderr) = run(
        &[".harness/spec-deploy-fails.yaml", "--max-attempts", "1"],
        &repo,
    );
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        "[hanvil] Chain ledger — attempt 1 — 3 transaction(s), 2 rejected before consensus",
        "[hanvil] Stage 4/5 SMOKE — skipped — chain deploy failed",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    assert!(
        run_directory(&repo)
            .join("logs/chain-ledger-attempt-1.json")
            .exists(),
        "the artifact is written on the deploy-failure path too"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// A one-week deadline, swept in one run. The contract reverts with "Deadline: too early"
/// before `openUntil`, so a pass is proof the chain clock moved and not that the assertion was
/// weak. On testnet this recipe would take a week.
#[test]
fn a_phase_moves_the_clock_a_week_before_its_commands_run() {
    let repo = fixture_repo("phases");
    let (ok, stdout, stderr) = run(&[".harness/spec-phases.yaml", "--max-attempts", "1"], &repo);
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    for line in [
        "[hanvil] Chain assertions — 1 of 1 passed",
        "[hanvil] Chain phase — after-a-week",
        "[hanvil] Chain time advanced — 604800 s",
        "[hanvil] Chain deploy — sweep — bash .harness/sweep-deadline.sh",
        "[hanvil] Chain assertions — 2 of 2 passed — phase after-a-week",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    // The gap between the deploy and the sweep is the week, visible in the ledger itself. The
    // tenth of a second after it is however long the sweep took, so it is not asserted.
    assert!(
        stdout.contains("  +604800."),
        "the ledger shows the week: {stdout}"
    );

    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(run_directory(&repo).join("logs/chain-ledger-attempt-1.json"))
            .expect("ledger"),
    )
    .expect("json");
    let entries = ledger["entries"].as_array().expect("entries");
    assert_eq!(entries.len(), 2);
    // The week the phase moved, plus however long the sweep itself took on this machine.
    let gap = entries[1]["atMillis"].as_u64().expect("offset");
    assert!(
        (604_800_000..608_400_000).contains(&gap),
        "a week between the deploy and the sweep, got {gap} ms"
    );
    let _ = std::fs::remove_dir_all(repo);
}

/// The ledger is rebuilt before every phase's assertions, not once after the last one.
///
/// The flat block asserts nothing has been refused; the phase then causes two refusals and
/// tolerates them. Built once at the end, the flat assertion would see the phase's refusals and
/// the run would fail. It passes, and the final ledger carries both refusals, so the two
/// assertion sets were evaluated against different ledgers.
#[test]
fn a_phase_s_refusals_are_not_visible_to_the_assertions_that_ran_before_it() {
    let repo = fixture_repo("phase-isolation");
    let (ok, stdout, stderr) = run(
        &[".harness/spec-phase-isolation.yaml", "--max-attempts", "1"],
        &repo,
    );
    assert!(ok, "stdout:\n{stdout}\nstderr:\n{stderr}");

    // Two ledgers, printed one per assertion set. Built once after the last phase, these two
    // lines would be identical and the flat assertion would have failed.
    assert!(
        stdout.contains("[hanvil] Chain ledger — attempt 1 — the attempt sent no transactions"),
        "the flat block's ledger is empty:\n{stdout}"
    );
    assert!(
        stdout.contains(
            "[hanvil] Chain ledger — attempt 1 — 3 transaction(s), 2 rejected before consensus"
        ),
        "the phase's ledger carries its own refusals:\n{stdout}"
    );

    let flat = stdout
        .find("[hanvil] Chain assertions — 1 of 1 passed\n")
        .expect("the flat block passed");
    let phase = stdout
        .find("[hanvil] Chain phase — refusals")
        .expect("the phase ran");
    assert!(flat < phase, "the flat block is evaluated first:\n{stdout}");
    assert!(
        stdout.contains("[hanvil] Chain assertions — 1 of 1 passed — phase refusals"),
        "{stdout}"
    );

    // The refusals the flat assertion did not see are on the chain by the end.
    let ledger: Value = serde_json::from_str(
        &std::fs::read_to_string(run_directory(&repo).join("logs/chain-ledger-attempt-1.json"))
            .expect("ledger"),
    )
    .expect("json");
    let rejected = ledger["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .filter(|e| e["outcome"] == "rejected")
        .count();
    assert_eq!(rejected, 2, "the phase's refusals are in the final ledger");
    let _ = std::fs::remove_dir_all(repo);
}

/// Without the phase's advance the same sweep reverts, which is what makes the test above proof
/// rather than assertion.
#[test]
fn the_same_sweep_reverts_when_the_clock_does_not_move() {
    let repo = fixture_repo("phases-no-advance");
    let spec = repo.join(".harness/spec-phases.yaml");
    let relaxed = std::fs::read_to_string(&spec)
        .expect("spec")
        .replace("advanceTimeSeconds: 604800", "advanceTimeSeconds: 0");
    std::fs::write(&spec, relaxed).expect("write");
    // The fixture is committed by `fixture_repo`; `hanvil run` refuses a dirty tree.
    git_stdout(&["add", "-A"], &repo);
    git_stdout(
        &["commit", "-q", "--no-gpg-sign", "-m", "relax the phase"],
        &repo,
    );

    let (ok, stdout, stderr) = run(&[".harness/spec-phases.yaml", "--max-attempts", "1"], &repo);
    assert!(!ok, "stdout:\n{stdout}\nstderr:\n{stderr}");
    assert!(
        stdout.contains("reverted: Deadline: too early"),
        "the ledger names the revert: {stdout}"
    );
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
        // The browser is launched before the first route, as upstream's launchSharedBrowser
        // does, so a cold Chromium start is never charged to a route's timeout.
        "[hanvil] SMOKE browser ready — ",
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
    assert!(gate["browserLaunchMs"].is_u64(), "{gate}");
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

/// `hanvil validate` boots the chain and provisions a signer for the dev server, as `run` does;
/// an app that refuses to start without `HARNESS_SIGNER_*` and `HANVIL_*` therefore starts.
#[test]
fn validate_gives_the_app_the_chain_and_the_signer() {
    if !browser_available() {
        eprintln!("skipped: the SMOKE gate needs npx and a Chromium or Chrome on this machine");
        return;
    }
    let repo =
        std::env::temp_dir().join(format!("hanvil-run-validate-chain-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&repo);
    copy_dir(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/harness-smoke"),
        &repo,
    );
    let spec_path = repo.join(".harness/spec.yaml");
    let mut spec = std::fs::read_to_string(&spec_path).expect("spec");
    spec.push_str("chainValidation:\n  enabled: true\n  network: local\n");
    std::fs::write(&spec_path, spec).expect("write spec");
    std::fs::write(
        repo.join(".harness/validators/playwright-smoke.yaml"),
        "server:\n  command: node server-env.js\n  url: http://127.0.0.1:47392\n  timeoutMs: 30000\ndefaults:\n  timeoutMs: 20000\n  hydrationTimeoutMs: 15000\nroutes:\n  - name: home\n    path: /\n",
    )
    .expect("write gate");
    std::fs::write(
        repo.join("server-env.js"),
        "for (const name of [\"HANVIL_RPC_URL\", \"HANVIL_MIRROR_URL\", \"HARNESS_SIGNER_ACCOUNT_ID\", \"HARNESS_SIGNER_PRIVATE_KEY\"]) {\n  if (!process.env[name]) { console.error(`${name} is not set`); process.exit(1); }\n}\nprocess.env.PORT = \"47392\";\nrequire(\"./server.js\");\n",
    )
    .expect("write server");
    std::fs::write(repo.join("generated.txt"), "ok\n").expect("write");

    let output = Command::new(env!("CARGO_BIN_EXE_hanvil"))
        .args([
            "validate",
            "--port",
            "0",
            "--mirror-port",
            "0",
            "--grpc-port",
            "0",
        ])
        .current_dir(&repo)
        .output()
        .expect("hanvil runs");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stdout:\n{stdout}\nstderr:\n{stderr}"
    );
    for line in [
        "[hanvil] Chain signer provisioned — 0.0.1032",
        "[hanvil] SMOKE browser ready — ",
        "[hanvil] Chain signer swept — 0.0.1032",
        "Validation finished\npassed=true\nfindings=0\nplaywrightGate=true routes=1",
    ] {
        assert!(stdout.contains(line), "missing {line:?} in:\n{stdout}");
    }
    assert!(
        !repo.join(".harness/runs").exists(),
        "validate writes no run"
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
