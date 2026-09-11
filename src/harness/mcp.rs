//! The browser behind SMOKE, EVALUATE and the doctor's probe: `@playwright/mcp`, spoken to over
//! stdio. `mcpBrowser.ts` and `validatorMcp.ts` of hedera-harness dev @ 587a2f3 chose the
//! browser and wrote the config the validator agent loads; here the harness is also a client
//! of the same server, so SMOKE needs no Playwright library and no Node script of its own.
//!
//! Pinned at 0.0.80 rather than `@latest`: a moving pin once demanded a Chromium nobody had
//! downloaded, and failed a run after the generator had been paid for.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader};

use crate::harness::command;

/// The server, pinned.
pub(crate) const PACKAGE: &str = "@playwright/mcp@0.0.80";
/// `mcpBrowser.ts:17`: marks a server entry as harness-written, so cleanup never strips a
/// user's own.
pub(crate) const MARKER: &str = "--headless";
/// MCP protocol revision the server speaks.
const PROTOCOL_VERSION: &str = "2024-11-05";
/// `mcpBrowser.ts` probe cap, and the cap on launching the browser before SMOKE's first route.
pub(crate) const LAUNCH_TIMEOUT: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = LAUNCH_TIMEOUT;

/// What can go wrong between the harness and the server.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `npx` could not be started.
    #[error("starting {PACKAGE} with npx: {0}")]
    Spawn(#[source] std::io::Error),
    /// The server closed its end.
    #[error("the Playwright MCP server closed the connection{0}")]
    Closed(String),
    /// A call did not answer in time.
    #[error("{method} did not answer within {timeout_s}s")]
    Timeout {
        /// The JSON-RPC method.
        method: String,
        /// The limit.
        timeout_s: u64,
    },
    /// The server answered with a JSON-RPC error.
    #[error("{method}: {message}")]
    Rpc {
        /// The method.
        method: String,
        /// `error.message`.
        message: String,
    },
    /// stdin could not be written.
    #[error("writing to the Playwright MCP server: {0}")]
    Write(#[source] std::io::Error),
    /// A config file could not be written.
    #[error("writing {path}: {source}")]
    Config {
        /// The file.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// How many of the server's last stderr lines an error carries. `npx` failing an install
/// prints hundreds; the cause is at the end of them.
const STDERR_TAIL_LINES: usize = 6;
/// How long to wait for a server that has closed its stdout to actually exit, so the drain task
/// has the last of its stderr before the error is built.
const STDERR_FLUSH: Duration = Duration::from_secs(2);

/// The message for a server that closed mid-call, carrying what it printed. Without this the
/// caller learns the server died and nothing about why, which is the one thing they need.
fn closed_message(during: &str, stderr: &str) -> String {
    let said: Vec<&str> = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .collect();
    if said.is_empty() {
        return during.to_string();
    }
    let tail = &said[said.len().saturating_sub(STDERR_TAIL_LINES)..];
    format!("{during}; it printed:\n{}", tail.join("\n"))
}

/// `mcpBrowser.ts:19-38`: which browser the server drives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrowserChoice {
    /// Playwright's own Chromium, already on disk.
    PlaywrightChromium(PathBuf),
    /// Whatever Chrome the machine has.
    SystemChrome,
}

impl BrowserChoice {
    /// `mcpBrowser.ts:38-50`.
    pub(crate) fn args(&self) -> Vec<String> {
        let mut args = vec!["-y".to_string(), PACKAGE.to_string(), MARKER.to_string()];
        match self {
            Self::PlaywrightChromium(path) => {
                args.push("--executable-path".to_string());
                args.push(path.to_string_lossy().into_owned());
            }
            Self::SystemChrome => {
                args.push("--browser".to_string());
                args.push("chrome".to_string());
            }
        }
        args
    }

    /// `mcpBrowser.ts:86,92`: one line for logs and doctor output.
    pub(crate) fn detail(&self) -> String {
        match self {
            Self::PlaywrightChromium(path) => format!(
                "Playwright Chromium (shared with the SMOKE gate) — {}",
                path.display()
            ),
            Self::SystemChrome => {
                "system Chrome — shared by SMOKE and EVALUATE because Playwright Chromium was unavailable"
                    .to_string()
            }
        }
    }

    /// `project-playwright` or `system-chrome`.
    pub(crate) fn source(&self) -> &'static str {
        match self {
            Self::PlaywrightChromium(_) => "project-playwright",
            Self::SystemChrome => "system-chrome",
        }
    }

    /// `preflight.ts:360-363`.
    pub(crate) fn repair(&self) -> &'static str {
        match self {
            Self::PlaywrightChromium(_) => "npx playwright install chromium",
            Self::SystemChrome => "Install Google Chrome, or run: npx playwright install chromium",
        }
    }
}

/// `mcpBrowser.ts:74-94`: Playwright's cached Chromium when it is on disk, else system Chrome.
/// Playwright is not a dependency here, so the cache is searched directly:
/// `$PLAYWRIGHT_BROWSERS_PATH`, else `~/Library/Caches/ms-playwright` or `~/.cache/ms-playwright`.
pub(crate) fn resolve_browser() -> BrowserChoice {
    playwright_chromium()
        .map(BrowserChoice::PlaywrightChromium)
        .unwrap_or(BrowserChoice::SystemChrome)
}

fn playwright_cache_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(explicit) = std::env::var_os("PLAYWRIGHT_BROWSERS_PATH") {
        dirs.push(PathBuf::from(explicit));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join("Library/Caches/ms-playwright"));
        dirs.push(home.join(".cache/ms-playwright"));
    }
    dirs
}

/// The newest `chromium-<revision>/…` executable: `Chromium.app/Contents/MacOS/Chromium` or
/// `Google Chrome for Testing.app/Contents/MacOS/…` on macOS, `chrome-linux*/chrome` on Linux.
fn playwright_chromium() -> Option<PathBuf> {
    let mut revisions: Vec<PathBuf> = playwright_cache_dirs()
        .into_iter()
        .filter_map(|cache| std::fs::read_dir(cache).ok())
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("chromium-")
                        && name["chromium-".len()..]
                            .bytes()
                            .all(|b| b.is_ascii_digit())
                })
                && path.join("INSTALLATION_COMPLETE").exists()
        })
        .collect();
    revisions.sort();
    revisions
        .into_iter()
        .rev()
        .find_map(|revision| find_executable(&revision, 0))
}

fn find_executable(directory: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 6 {
        return None;
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for entry in &entries {
        let name = entry.file_name()?.to_string_lossy().into_owned();
        let parent = directory
            .file_name()
            .map(|p| p.to_string_lossy().into_owned());
        let is_binary = entry.is_file()
            && (name == "chrome"
                || (parent.as_deref() == Some("MacOS")
                    && (name == "Chromium" || name.starts_with("Google Chrome"))));
        if is_binary {
            return Some(entry.clone());
        }
    }
    entries
        .iter()
        .filter(|entry| entry.is_dir())
        .find_map(|entry| find_executable(entry, depth + 1))
}

/// `validatorMcp.ts:23-44` and `mcpBrowser.ts:244-259`: the config the validator agent loads
/// with `--mcp-config`, and the output directory the server writes into.
pub(crate) fn write_config(run_directory: &Path, choice: &BrowserChoice) -> Result<PathBuf, Error> {
    let mcp_dir = run_directory.join("mcp");
    let output_dir = mcp_dir.join("output");
    std::fs::create_dir_all(&output_dir).map_err(|source| Error::Config {
        path: output_dir.clone(),
        source,
    })?;
    let mut args = choice.args();
    args.push("--output-dir".to_string());
    args.push(output_dir.to_string_lossy().into_owned());
    let config = json!({
        "mcpServers": {
            "playwright": { "command": "npx", "args": args },
        },
    });
    let path = mcp_dir.join("playwright.json");
    let rendered = serde_json::to_string_pretty(&config).unwrap_or_default();
    std::fs::write(&path, format!("{rendered}\n")).map_err(|source| Error::Config {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// `mcpBrowser.ts:306-338`: for a CLI that only reads a fixed workspace file (Cursor's
/// `.cursor/mcp.json`), merge the harness's server entry in for the duration of EVALUATE and
/// put the file back byte for byte afterwards — or remove it, when there was none.
pub(crate) struct WorkspaceFile {
    path: PathBuf,
    previous: Option<Vec<u8>>,
}

impl WorkspaceFile {
    /// Write the merged file.
    pub(crate) fn install(
        workspace: &Path,
        relative: &str,
        choice: &BrowserChoice,
        output_dir: &Path,
    ) -> Result<Self, Error> {
        let path = workspace.join(relative);
        let previous = std::fs::read(&path).ok();
        let mut servers = previous
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|value| value.get("mcpServers").cloned())
            .and_then(|servers| match servers {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        let mut args = choice.args();
        args.push("--output-dir".to_string());
        args.push(output_dir.to_string_lossy().into_owned());
        servers.insert(
            "playwright".to_string(),
            json!({ "command": "npx", "args": args }),
        );
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| Error::Config {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let rendered =
            serde_json::to_string_pretty(&json!({ "mcpServers": servers })).unwrap_or_default();
        std::fs::write(&path, format!("{rendered}\n")).map_err(|source| Error::Config {
            path: path.clone(),
            source,
        })?;
        Ok(Self { path, previous })
    }

    /// Restore what was there.
    pub(crate) fn restore(self) {
        match self.previous {
            Some(bytes) => {
                let _ = std::fs::write(&self.path, bytes);
            }
            None => {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// `runCleanup.ts:84-130`: strip a harness-written `playwright` entry from a workspace MCP
/// file; delete the file when that empties it. Never a user's own entry.
pub(crate) fn strip_harness_entry(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(mut parsed) = serde_json::from_str::<Value>(&raw) else {
        return false;
    };
    let Some(servers) = parsed.get_mut("mcpServers").and_then(Value::as_object_mut) else {
        return false;
    };
    let Some(entry) = servers.get("playwright") else {
        return false;
    };
    if !is_harness_mcp_server(entry) {
        return false;
    }
    servers.remove("playwright");
    if servers.is_empty() {
        let _ = std::fs::remove_file(path);
        return true;
    }
    let rendered = serde_json::to_string_pretty(&parsed).unwrap_or_default();
    std::fs::write(path, format!("{rendered}\n")).is_ok()
}

/// `mcpBrowser.ts:218-225`: an entry the harness wrote carries both the pin and the marker.
pub(crate) fn is_harness_mcp_server(entry: &Value) -> bool {
    let args: Vec<&str> = entry
        .get("args")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    entry.get("command").and_then(Value::as_str) == Some("npx")
        && args.contains(&PACKAGE)
        && args.contains(&MARKER)
}

/// What a tool call came back with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CallResult {
    /// Every `content[].text`, joined.
    pub(crate) text: String,
    /// `isError` on the result.
    pub(crate) is_error: bool,
}

/// A running server and the pipes to it.
pub(crate) struct Client {
    child: tokio::process::Child,
    pgid: Option<u32>,
    stdin: tokio::process::ChildStdin,
    lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
}

impl Client {
    /// Start `npx -y @playwright/mcp@… <args>` in `cwd`, isolated so its profile never clashes
    /// with another server's, and complete the MCP handshake.
    pub(crate) async fn spawn(
        choice: &BrowserChoice,
        output_dir: &Path,
        cwd: &Path,
    ) -> Result<Self, Error> {
        let mut args = choice.args();
        args.push("--isolated".to_string());
        args.push("--output-dir".to_string());
        args.push(output_dir.to_string_lossy().into_owned());
        let mut child = tokio::process::Command::new("npx")
            .args(&args)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(Error::Spawn)?;
        let pgid = child.id();
        command::register_group(pgid);
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Closed(" before the handshake".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Closed(" before the handshake".into()))?;
        let stderr_text = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            let sink = Arc::clone(&stderr_text);
            let capture = Arc::new(Mutex::new(command::BoundedOutput::with_limits(
                64 * 1024,
                64 * 1024,
            )));
            command::drain(pipe, capture, move |chunk| {
                if let Ok(mut text) = sink.lock() {
                    text.push_str(&String::from_utf8_lossy(chunk));
                }
            });
        }
        let mut client = Self {
            child,
            pgid,
            stdin,
            lines: BufReader::new(stdout).lines(),
            stderr: stderr_text,
            next_id: 0,
        };
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "hanvil", "version": env!("CARGO_PKG_VERSION") },
                }),
                PROBE_TIMEOUT,
            )
            .await?;
        client
            .notify("notifications/initialized", json!({}))
            .await?;
        Ok(client)
    }

    /// The error for a server that closed its end. It has already said why on stderr, so wait
    /// for it to exit — it has closed stdout, so this returns at once — and quote it.
    async fn closed(&mut self, during: &str) -> Error {
        let _ = tokio::time::timeout(STDERR_FLUSH, self.child.wait()).await;
        Error::Closed(closed_message(during, &self.stderr_text()))
    }

    /// Everything the server wrote to stderr so far.
    pub(crate) fn stderr_text(&self) -> String {
        self.stderr
            .lock()
            .map(|text| text.clone())
            .unwrap_or_default()
    }

    async fn write_line(&mut self, message: &Value) -> Result<(), Error> {
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(Error::Write)?;
        self.stdin.flush().await.map_err(Error::Write)
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), Error> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await
    }

    /// Send a request and wait for its response, skipping notifications and other traffic.
    async fn request(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, Error> {
        self.next_id += 1;
        let id = self.next_id;
        self.write_line(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let wait = async {
            loop {
                let next = self.lines.next_line().await;
                let line = match next {
                    Ok(Some(line)) => line,
                    Ok(None) => return Err(self.closed(&format!(" during {method}")).await),
                    Err(error) => {
                        return Err(self.closed(&format!(" during {method}: {error}")).await);
                    }
                };
                let Ok(message) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if message.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(Error::Rpc {
                        method: method.to_string(),
                        message: error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string(),
                    });
                }
                return Ok(message.get("result").cloned().unwrap_or(Value::Null));
            }
        };
        tokio::time::timeout(timeout, wait)
            .await
            .unwrap_or(Err(Error::Timeout {
                method: method.to_string(),
                timeout_s: timeout.as_secs(),
            }))
    }

    /// `tools/call`.
    pub(crate) async fn call(
        &mut self,
        tool: &str,
        arguments: Value,
        timeout: Duration,
    ) -> Result<CallResult, Error> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": tool, "arguments": arguments }),
                timeout,
            )
            .await?;
        let text = result
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(CallResult {
            text,
            is_error: result.get("isError") == Some(&Value::Bool(true)),
        })
    }

    /// Stop the server and its browser.
    pub(crate) async fn close(mut self) {
        drop(self.stdin);
        let _ = command::stop_group(&mut self.child, self.pgid).await;
        command::unregister_group(self.pgid);
    }
}

/// `mcpBrowser.ts:116-121`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Probe {
    /// The browser navigated.
    pub(crate) ok: bool,
    /// Which browser was tried.
    pub(crate) choice: BrowserChoice,
    /// The launch error as reported by the server, when it failed.
    pub(crate) error: Option<String>,
}

/// `mcpBrowser.ts:129-215`: start the server and actually navigate, because every cheaper check
/// has lied.
pub(crate) async fn probe(cwd: &Path) -> Probe {
    let choice = resolve_browser();
    let output_dir = std::env::temp_dir().join(format!("hanvil-mcp-probe-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&output_dir);
    let result = tokio::time::timeout(PROBE_TIMEOUT, async {
        let mut client = Client::spawn(&choice, &output_dir, cwd).await?;
        let navigated = client
            .call(
                "browser_navigate",
                json!({ "url": "about:blank" }),
                PROBE_TIMEOUT,
            )
            .await;
        let stderr = client.stderr_text();
        client.close().await;
        navigated.map(|call| (call, stderr))
    })
    .await;
    let _ = std::fs::remove_dir_all(&output_dir);
    let (ok, error) = match result {
        Ok(Ok((call, stderr))) => {
            let combined = format!("{}\n{stderr}", call.text);
            match verdict_from(&combined) {
                Some(true) if !call.is_error => (true, None),
                _ => (false, Some(launch_error(&combined))),
            }
        }
        Ok(Err(error)) => (false, Some(error.to_string())),
        Err(_) => (
            false,
            Some(format!(
                "the Playwright MCP browser did not answer within {}s",
                PROBE_TIMEOUT.as_secs()
            )),
        ),
    };
    Probe { ok, choice, error }
}

/// `mcpBrowser.ts:106-114`: true = navigated, false = launch failed, None = unknown.
fn verdict_from(output: &str) -> Option<bool> {
    let lower = output.to_lowercase();
    if [
        "is not installed",
        "executable doesn't exist",
        "expected executable at",
        "failed to launch",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        return Some(false);
    }
    if ["about:blank", "page url", "ran playwright code"]
        .iter()
        .any(|needle| lower.contains(needle))
    {
        return Some(true);
    }
    None
}

fn launch_error(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| {
            let lower = line.to_lowercase();
            lower.contains("not installed")
                || lower.contains("executable")
                || lower.contains("failed to launch")
                || lower.contains("error")
        })
        .unwrap_or("the Playwright MCP browser could not be launched");
    line.chars().take(400).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_choices_render_upstreams_argv_and_details() {
        let chromium = BrowserChoice::PlaywrightChromium(PathBuf::from("/b/chrome"));
        assert_eq!(
            chromium.args(),
            vec![
                "-y",
                PACKAGE,
                "--headless",
                "--executable-path",
                "/b/chrome"
            ]
        );
        assert_eq!(
            BrowserChoice::SystemChrome.args(),
            vec!["-y", PACKAGE, "--headless", "--browser", "chrome"]
        );
        assert!(
            chromium
                .detail()
                .starts_with("Playwright Chromium (shared with the SMOKE gate) — /b/chrome")
        );
        assert_eq!(BrowserChoice::SystemChrome.source(), "system-chrome");
        assert_eq!(chromium.repair(), "npx playwright install chromium");
    }

    #[test]
    fn the_validator_config_has_upstreams_shape_and_is_recognised() {
        let dir = std::env::temp_dir().join(format!("hanvil-mcp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = write_config(&dir, &BrowserChoice::SystemChrome).expect("config");
        assert_eq!(path, dir.join("mcp/playwright.json"));
        assert!(dir.join("mcp/output").is_dir());
        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let server = &parsed["mcpServers"]["playwright"];
        assert_eq!(server["command"], "npx");
        assert_eq!(server["args"][1], PACKAGE);
        assert_eq!(server["args"][2], "--headless");
        assert_eq!(server["args"][5], "--output-dir");
        assert!(is_harness_mcp_server(server));
        assert!(!is_harness_mcp_server(
            &json!({"command": "npx", "args": ["-y", "@playwright/mcp@latest"]})
        ));
        assert!(!is_harness_mcp_server(
            &json!({"command": "node", "args": [PACKAGE, MARKER]})
        ));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_server_that_died_is_quoted_not_just_reported_dead() {
        assert_eq!(
            closed_message(" during initialize", "   \n  "),
            " during initialize"
        );
        let npx = (1..=10)
            .map(|n| format!("npm error line {n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let message = closed_message(" during initialize", &npx);
        assert!(message.starts_with(" during initialize; it printed:\n"));
        assert!(message.contains("npm error line 5"), "{message}");
        assert!(message.ends_with("npm error line 10"), "{message}");
        assert!(!message.contains("npm error line 4"), "{message}");
    }

    #[test]
    fn verdicts_follow_upstreams_regexes() {
        assert_eq!(verdict_from("Navigated to about:blank"), Some(true));
        assert_eq!(
            verdict_from("Error: browserType.launch: Executable doesn't exist at /x"),
            Some(false)
        );
        assert_eq!(verdict_from("Chromium is not installed"), Some(false));
        assert_eq!(verdict_from("starting"), None);
        assert!(
            launch_error("hello\nError: Failed to launch the browser\n")
                .starts_with("Error: Failed to launch")
        );
    }

    #[test]
    fn the_resolver_finds_a_cached_chromium_when_one_is_installed() {
        // On a machine without the cache this is SystemChrome; on one with it, the path exists.
        match resolve_browser() {
            BrowserChoice::PlaywrightChromium(path) => {
                assert!(path.is_file(), "{}", path.display())
            }
            BrowserChoice::SystemChrome => {}
        }
    }
}
