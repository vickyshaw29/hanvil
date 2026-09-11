//! The node half never opens an outbound socket (`.claude/CLAUDE.md` §3.9).
//!
//! `hanvil run` may, and only on `chainValidation.network: testnet`, where the harness is a
//! client of somebody else's network. Everything that serves the local chain — `state/`, `evm/`,
//! `rpc/`, `mirror/`, `hapi/` — must stay reachable-from-nothing, because "the node makes no
//! outbound calls" is a README claim and the reason a judge can trust what it measures.

use std::path::Path;

/// Crates that can open a socket to somewhere else. Extend when a dependency is added, not when
/// a test fails.
const OUTBOUND_CRATES: [&str; 6] = [
    "reqwest",
    "ureq",
    "hyper_util",
    "hyper_tls",
    "isahc",
    "curl",
];

/// Modules that serve the local chain and must never reach out.
const NODE_MODULES: [&str; 5] = ["state", "evm", "rpc", "mirror", "hapi"];

fn rust_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn the_node_half_imports_no_http_client() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offences = Vec::new();
    for module in NODE_MODULES {
        let dir = src.join(module);
        let mut files = Vec::new();
        if dir.is_dir() {
            rust_files(&dir, &mut files);
        }
        for file in files {
            let body = std::fs::read_to_string(&file).expect("readable");
            for (number, line) in body.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                if !code.contains("use ") && !code.contains("::") {
                    continue;
                }
                for krate in OUTBOUND_CRATES {
                    if code.contains(&format!("{krate}::")) {
                        offences.push(format!(
                            "{}:{}: {krate} — the node half must not reach out",
                            file.display(),
                            number + 1
                        ));
                    }
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "\u{a7}3.9: the node never opens an outbound socket\n{}",
        offences.join("\n")
    );
}

/// And `TcpStream::connect` is the same hole one layer down: the listeners bind, they do not dial.
#[test]
fn the_node_half_dials_nothing_directly() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offences = Vec::new();
    for module in NODE_MODULES {
        let dir = src.join(module);
        let mut files = Vec::new();
        if dir.is_dir() {
            rust_files(&dir, &mut files);
        }
        for file in files {
            let body = std::fs::read_to_string(&file).expect("readable");
            for (number, line) in body.lines().enumerate() {
                let code = line.split("//").next().unwrap_or(line);
                if code.contains("TcpStream::connect") || code.contains("TcpSocket::connect") {
                    offences.push(format!("{}:{}", file.display(), number + 1));
                }
            }
        }
    }
    assert!(
        offences.is_empty(),
        "\u{a7}3.9: the node binds, it does not dial\n{}",
        offences.join("\n")
    );
}
