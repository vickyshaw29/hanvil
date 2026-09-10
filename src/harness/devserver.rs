//! The app under test, running. `validation/devServer.ts` of hedera-harness dev @ 587a2f3: one
//! session per attempt, shared by SMOKE and EVALUATE, started here and nowhere else.
//!
//! One deviation, in the app's favour: upstream fails when the server prints no `Local:` line
//! within 30 s, which every Express or bare `http.createServer` app does. Here the recipe's
//! required `server.url` is polled instead when the line never comes, so a backend recipe works
//! and a Next.js recipe behaves as before.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::harness::command;

/// `devServer.ts:7`.
const URL_DETECT_TIMEOUT: Duration = Duration::from_secs(30);
/// `devServer.ts:87`.
const DEFAULT_READY_TIMEOUT: Duration = Duration::from_secs(120);

/// What stops a dev server from being usable.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The gate YAML could not be read or parsed.
    #[error("{path}: {message}")]
    Config {
        /// The gate config.
        path: PathBuf,
        /// What was wrong.
        message: String,
    },
    /// `devServer.ts:81`.
    #[error("Playwright config {0} requires server.command and server.url.")]
    MissingServer(PathBuf),
    /// The command could not be started.
    #[error("starting the dev server: {0}")]
    Spawn(#[source] std::io::Error),
    /// `devServer.ts:123`, when `server.url` did not answer either.
    #[error(
        "Dev server did not report a Local URL within {0}ms. Expected output like \"Local: http://localhost:3000\"."
    )]
    NoLocalUrl(u128),
    /// `devServer.ts:174`.
    #[error("Dev server exited before reporting a Local URL ({0}).")]
    ExitedEarly(String),
    /// `devServer.ts:202`.
    #[error("Dev server did not become ready at {url} within {timeout_ms}ms ({last_error}).")]
    NotReady {
        /// The URL polled.
        url: String,
        /// The limit.
        timeout_ms: u128,
        /// The last answer.
        last_error: String,
    },
}

/// `devServer.ts:9-13`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Config {
    /// Run through the shell in the workspace.
    pub(crate) command: String,
    /// Where the recipe says the app listens.
    pub(crate) configured_url: String,
    /// Readiness limit.
    pub(crate) timeout: Duration,
}

/// `devServer.ts:74-89`: `server.command`, `server.url`, `server.timeoutMs` from the gate YAML.
pub(crate) fn load_config(playwright_yaml: &Path) -> Result<Config, Error> {
    let raw = std::fs::read_to_string(playwright_yaml).map_err(|e| Error::Config {
        path: playwright_yaml.to_path_buf(),
        message: e.to_string(),
    })?;
    let parsed: Value = serde_yaml_ng::from_str(&raw).map_err(|e| Error::Config {
        path: playwright_yaml.to_path_buf(),
        message: e.to_string(),
    })?;
    let server = parsed.get("server");
    let field = |key: &str| {
        server
            .and_then(|s| s.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let (Some(command), Some(url)) = (field("command"), field("url")) else {
        return Err(Error::MissingServer(playwright_yaml.to_path_buf()));
    };
    Ok(Config {
        command,
        configured_url: url,
        timeout: server
            .and_then(|s| s.get("timeoutMs"))
            .and_then(Value::as_u64)
            .map_or(DEFAULT_READY_TIMEOUT, Duration::from_millis),
    })
}

/// `devServer.ts:22-28`: a live server, borrowed by the gates.
#[derive(Debug)]
pub(crate) struct Session {
    child: tokio::process::Child,
    pgid: Option<u32>,
    /// The URL the gates use — detected, or the configured one.
    pub(crate) url: String,
    /// `server.command`.
    pub(crate) command: String,
}

impl Session {
    /// `devServer.ts:63-66`: false once the child has exited.
    pub(crate) fn is_alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// `devServer.ts:205-230`: SIGTERM the group, SIGKILL after 5 s.
    pub(crate) async fn stop(mut self) {
        let _ = command::stop_group(&mut self.child, self.pgid).await;
        command::unregister_group(self.pgid);
    }
}

/// `devServer.ts:35-72`: spawn → detect the URL → wait until it answers; on failure the
/// server is stopped so it does not keep the port for the rest of the session.
pub(crate) async fn start(
    workspace: &Path,
    config: &Config,
    log_prefix: &str,
    env: &BTreeMap<String, String>,
) -> Result<Session, Error> {
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(&config.command)
        .current_dir(workspace)
        .envs(env)
        .env("FORCE_COLOR", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(Error::Spawn)?;
    let pgid = child.id();
    command::register_group(pgid);
    let detected: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    for (pipe, stream) in [
        (child.stdout.take().map(Pipe::Out), "stdout"),
        (child.stderr.take().map(Pipe::Err), "stderr"),
    ] {
        let Some(pipe) = pipe else {
            continue;
        };
        let prefix = if stream == "stderr" {
            format!("[hanvil:{log_prefix}:server:stderr]")
        } else {
            format!("[hanvil:{log_prefix}:server]")
        };
        let found = Arc::clone(&detected);
        let conflict_prefix = log_prefix.to_string();
        let observer = move |chunk: &[u8]| {
            let text = String::from_utf8_lossy(chunk);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let collapsed = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
                println!("{prefix} {}", truncate(&collapsed, 240));
            }
            if let Some(url) = extract_local_url(&text)
                && let Ok(mut slot) = found.lock()
                && slot.is_none()
            {
                *slot = Some(normalise_base_url(&url));
            }
            if text.to_lowercase().contains("port ") && text.to_lowercase().contains(" is in use") {
                println!(
                    "[hanvil] {conflict_prefix} detected a port conflict; health checks will follow the server's reported Local URL."
                );
            }
        };
        let capture = Arc::new(Mutex::new(command::BoundedOutput::with_limits(
            64 * 1024,
            64 * 1024,
        )));
        match pipe {
            Pipe::Out(out) => {
                command::drain(out, capture, observer);
            }
            Pipe::Err(err) => {
                command::drain(err, capture, observer);
            }
        }
    }

    let mut session = Session {
        child,
        pgid,
        url: config.configured_url.clone(),
        command: config.command.clone(),
    };

    // Detect the URL, or fall back to the configured one when it already answers.
    let detect_deadline = Instant::now() + URL_DETECT_TIMEOUT;
    let url = loop {
        if let Some(url) = detected.lock().ok().and_then(|slot| slot.clone()) {
            break url;
        }
        if let Ok(Some(status)) = session.child.try_wait() {
            let reason = command::signal_name(&status)
                .map(|signal| format!("signal {signal}"))
                .unwrap_or_else(|| {
                    format!(
                        "exit code {}",
                        status
                            .code()
                            .map_or_else(|| "null".to_string(), |c| c.to_string())
                    )
                });
            session.stop().await;
            return Err(Error::ExitedEarly(reason));
        }
        if Instant::now() >= detect_deadline {
            if http_get_status(&config.configured_url)
                .await
                .is_ok_and(ready)
            {
                println!(
                    "[hanvil] Dev server printed no Local URL in {}s; {} answers, using it.",
                    URL_DETECT_TIMEOUT.as_secs(),
                    config.configured_url
                );
                break config.configured_url.clone();
            }
            session.stop().await;
            return Err(Error::NoLocalUrl(URL_DETECT_TIMEOUT.as_millis()));
        }
        // The configured URL may answer long before the Local line is printed, or instead of it.
        if http_get_status(&config.configured_url)
            .await
            .is_ok_and(ready)
        {
            break config.configured_url.clone();
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    if let Err(error) = wait_for_server(&url, config.timeout).await {
        session.stop().await;
        return Err(error);
    }
    if url != config.configured_url {
        println!(
            "[hanvil] Dev server using detected URL {url} (config specified {})",
            config.configured_url
        );
    }
    session.url = url;
    Ok(session)
}

enum Pipe {
    Out(tokio::process::ChildStdout),
    Err(tokio::process::ChildStderr),
}

fn ready(status: u16) -> bool {
    (200..400).contains(&status)
}

/// `devServer.ts:184-203`: poll once a second until 2xx/3xx.
async fn wait_for_server(url: &str, timeout: Duration) -> Result<(), Error> {
    let deadline = Instant::now() + timeout;
    let mut last_error = "server not ready".to_string();
    while Instant::now() < deadline {
        match http_get_status(url).await {
            Ok(status) if ready(status) => return Ok(()),
            Ok(status) => last_error = format!("HTTP {status}"),
            Err(error) => last_error = error,
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    Err(Error::NotReady {
        url: url.to_string(),
        timeout_ms: timeout.as_millis(),
        last_error,
    })
}

/// `devServer.ts:6`: `Local:\s*(https?://[^\s-]+)`, case-insensitive.
pub(crate) fn extract_local_url(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let at = lower.find("local:")?;
    let rest = text[at + "local:".len()..].trim_start();
    let lower_rest = rest.to_lowercase();
    if !lower_rest.starts_with("http://") && !lower_rest.starts_with("https://") {
        return None;
    }
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '-')
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// `devServer.ts:244-250`: scheme, host and port only, no trailing slash.
pub(crate) fn normalise_base_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").unwrap_or(("http", url));
    let host_port = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(rest)
        .trim_end_matches('/');
    format!("{scheme}://{host_port}")
}

/// A single `GET` over a raw socket, answering the HTTP status. No HTTP client crate: the node
/// makes no outbound calls and this one is to a process the harness started.
pub(crate) async fn http_get_status(url: &str) -> Result<u16, String> {
    let (host, port, path) = split_url(url).ok_or_else(|| format!("not a URL: {url}"))?;
    let mut stream = tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| "connect timed out".to_string())?
    .map_err(|e| e.to_string())?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: hanvil\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 512];
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buffer))
        .await
        .map_err(|_| "read timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let head = String::from_utf8_lossy(&buffer[..read]);
    head.lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .ok_or_else(|| "no HTTP status line".to_string())
}

fn split_url(url: &str) -> Option<(String, u16, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host.to_string(), port.parse().ok()?),
        None => (
            authority.to_string(),
            if scheme == "https" { 443 } else { 80 },
        ),
    };
    Some((host, port, path.to_string()))
}

fn truncate(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        format!("{}...", value.chars().take(max).collect::<String>())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_urls_are_extracted_and_normalised_like_upstream() {
        assert_eq!(
            extract_local_url("  ▲ Next.js 15\n  - Local:        http://localhost:3000\n")
                .as_deref(),
            Some("http://localhost:3000")
        );
        assert_eq!(
            extract_local_url("local: https://127.0.0.1:8080/app?x=1 - ready").as_deref(),
            Some("https://127.0.0.1:8080/app?x=1")
        );
        assert_eq!(extract_local_url("Local: not-a-url"), None);
        assert_eq!(extract_local_url("listening on 3000"), None);
        assert_eq!(
            normalise_base_url("http://localhost:3000/app?x=1#y"),
            "http://localhost:3000"
        );
        assert_eq!(
            normalise_base_url("http://localhost:3000/"),
            "http://localhost:3000"
        );
        assert_eq!(
            split_url("http://127.0.0.1:4242/health"),
            Some(("127.0.0.1".into(), 4242, "/health".into()))
        );
        assert_eq!(
            split_url("https://example.test"),
            Some(("example.test".into(), 443, "/".into()))
        );
    }

    #[test]
    fn the_gate_config_needs_a_command_and_a_url() {
        let dir = std::env::temp_dir().join(format!("hanvil-devserver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("playwright-smoke.yaml");
        std::fs::write(&path, "server:\n  command: yarn dev\n  url: http://localhost:3000\n  timeoutMs: 5000\nroutes: []\n").expect("write");
        let config = load_config(&path).expect("config");
        assert_eq!(config.command, "yarn dev");
        assert_eq!(config.configured_url, "http://localhost:3000");
        assert_eq!(config.timeout, Duration::from_millis(5000));
        std::fs::write(&path, "server:\n  command: yarn dev\n").expect("write");
        assert_eq!(
            load_config(&path).expect_err("missing url").to_string(),
            format!(
                "Playwright config {} requires server.command and server.url.",
                path.display()
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("addr")
            .port()
    }

    #[tokio::test]
    async fn a_server_that_prints_its_local_url_is_detected_and_stopped() {
        let port = free_port();
        let config = Config {
            command: format!(
                "python3 -c 'import http.server,socketserver,sys\nprint(\"  Local: http://127.0.0.1:{port}/\", flush=True)\nsocketserver.TCPServer.allow_reuse_address=True\nsocketserver.TCPServer((\"127.0.0.1\",{port}),http.server.SimpleHTTPRequestHandler).serve_forever()'"
            ),
            configured_url: "http://127.0.0.1:1".to_string(),
            timeout: Duration::from_secs(20),
        };
        let mut session = start(&std::env::temp_dir(), &config, "test", &BTreeMap::new())
            .await
            .expect("session");
        assert_eq!(session.url, format!("http://127.0.0.1:{port}"));
        assert!(session.is_alive());
        assert!(ready(http_get_status(&session.url).await.expect("status")));
        session.stop().await;
        assert!(
            http_get_status(&format!("http://127.0.0.1:{port}"))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn a_server_that_prints_nothing_is_accepted_when_the_configured_url_answers() {
        let port = free_port();
        let config = Config {
            command: format!(
                "python3 -c 'import http.server,socketserver\nsocketserver.TCPServer.allow_reuse_address=True\nsocketserver.TCPServer((\"127.0.0.1\",{port}),http.server.SimpleHTTPRequestHandler).serve_forever()' 2>/dev/null"
            ),
            configured_url: format!("http://127.0.0.1:{port}"),
            timeout: Duration::from_secs(20),
        };
        let session = start(&std::env::temp_dir(), &config, "test", &BTreeMap::new())
            .await
            .expect("session");
        assert_eq!(session.url, format!("http://127.0.0.1:{port}"));
        session.stop().await;
    }

    #[tokio::test]
    async fn a_server_that_exits_is_reported_with_its_exit_code() {
        let config = Config {
            command: "exit 7".to_string(),
            configured_url: "http://127.0.0.1:1".to_string(),
            timeout: Duration::from_secs(5),
        };
        let error = start(&std::env::temp_dir(), &config, "test", &BTreeMap::new())
            .await
            .expect_err("exits");
        assert_eq!(
            error.to_string(),
            "Dev server exited before reporting a Local URL (exit code 7)."
        );
    }
}
