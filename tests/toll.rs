//! `hanvil toll`: the rail is handed the chain and three accounts, and dies with the node.
//!
//! The rail itself is TypeScript and needs `yarn install`, so these tests substitute a fake one
//! through `--command`. What is under test is hanvil's half: the refusal when the directory is
//! not a rail, the environment the child is given, and that the child is reaped.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// A directory with a `package.json`, so the rail check passes, and nothing else.
fn rail_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("hanvil-toll-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create the rail directory");
    std::fs::write(root.join("package.json"), "{}\n").expect("write package.json");
    root
}

/// A port nothing is listening on. The listener is dropped before it is handed over.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral port");
    listener.local_addr().expect("local addr").port()
}

fn get_status(port: u16) -> Option<u16> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("read timeout");
    write!(
        stream,
        "GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    )
    .ok()?;
    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
}

fn wait_until<F: FnMut() -> bool>(limit: Duration, mut done: F) -> bool {
    let deadline = Instant::now() + limit;
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    done()
}

fn toll(dir: &Path, extra: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_hanvil"));
    command
        .arg("toll")
        .arg(dir)
        .args(["--port", "0", "--mirror-port", "0", "--grpc-port", "0"])
        .args(extra);
    command
}

#[test]
fn a_directory_without_a_package_json_is_refused() {
    let root = std::env::temp_dir().join(format!("hanvil-toll-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create the directory");

    let output = toll(&root, &[])
        .output()
        .expect("hanvil toll runs to completion");

    assert!(!output.status.success(), "a bare directory is not a rail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("does not look like a toll rail") && stderr.contains("package.json"),
        "the refusal says what was missing: {stderr}"
    );
    assert!(
        stderr.contains("examples/toll"),
        "and what the default is: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_rail_is_handed_the_chain_and_three_accounts() {
    let root = rail_dir("env");
    let port = free_port();
    // A fake rail: record the environment, then answer 200 on / so readiness is reached.
    let command = format!(
        "env > env.txt && exec python3 -m http.server {port} --bind 127.0.0.1 >/dev/null 2>&1"
    );

    let mut child: Child = toll(
        &root,
        &[
            "--command",
            &command,
            "--service-port",
            &port.to_string(),
            "--price",
            "250000",
        ],
    )
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .expect("hanvil toll starts");

    let env_file = root.join("env.txt");
    assert!(
        wait_until(Duration::from_secs(60), || {
            get_status(port) == Some(200) && env_file.is_file()
        }),
        "the fake rail answers on {port} and recorded its environment"
    );

    let recorded = std::fs::read_to_string(&env_file).expect("read env.txt");
    for expected in [
        // The first three predefined ECDSA accounts, in id order, in their three roles.
        "HEDERA_ACCOUNT_ID=0.0.1002",
        "PAY_TO=0.0.1003",
        "FACILITATOR_ACCOUNT_ID=0.0.1004",
        // The chain this process owns, not testnet.
        "HEDERA_NETWORK=local",
        "TOLL_PRICE_TINYBARS=250000",
        &format!("PORT={port}"),
    ] {
        assert!(
            recorded.contains(expected),
            "the rail was given {expected}; it got:\n{recorded}"
        );
    }
    for prefix in [
        "HANVIL_RPC_URL=http://127.0.0.1:",
        "HANVIL_GRPC_URL=127.0.0.1:",
    ] {
        assert!(
            recorded.contains(prefix),
            "the rail was told where the chain is ({prefix}); it got:\n{recorded}"
        );
    }
    // The keys are the payer's and the facilitator's, and they are not the same account's.
    let payer_key = key_for(&recorded, "HEDERA_PRIVATE_KEY=");
    let facilitator_key = key_for(&recorded, "FACILITATOR_PRIVATE_KEY=");
    assert!(
        payer_key.starts_with("0x") && payer_key.len() == 66,
        "payer key: {payer_key}"
    );
    assert_ne!(
        payer_key, facilitator_key,
        "the facilitator signs with its own key, not the payer's"
    );

    // SIGINT, which is what ctrl-c sends. The rail runs in a process group of its own, so a
    // terminal's ctrl-c never reaches it and hanvil has to stop it — that is what is asserted.
    interrupt(&child);
    let status = child.wait().expect("reap hanvil toll");
    assert_eq!(
        status.code(),
        Some(130),
        "an interrupted `hanvil toll` exits 130, as `hanvil run` does"
    );

    assert!(
        wait_until(Duration::from_secs(30), || get_status(port).is_none()),
        "the rail is stopped with the node, leaving {port} free"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// SIGINT to the child. `Child::kill` is SIGKILL, which no handler can clean up after.
fn interrupt(child: &Child) {
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("send SIGINT");
    assert!(status.success(), "SIGINT was delivered");
}

fn key_for(recorded: &str, name: &str) -> String {
    recorded
        .lines()
        .find_map(|line| line.strip_prefix(name))
        .unwrap_or_default()
        .to_string()
}
