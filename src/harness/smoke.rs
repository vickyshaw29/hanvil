//! SMOKE: the app boots and its routes render. `validation/playwrightGate.ts` of hedera-harness
//! dev @ 587a2f3, driven through `@playwright/mcp` instead of the Playwright library. Per route:
//! navigate, wait for meaningful body text, read the HTTP status, collect console errors, look
//! for forbidden text. Finding ids and messages are upstream's.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::harness::devserver;
use crate::harness::evaluate::balanced_json_objects;
use crate::harness::findings::{Category, Finding, PlaywrightGateResult, RouteResult};
use crate::harness::mcp::{self, BrowserChoice, Client};
use crate::harness::session::log_phase;

/// `playwrightGate.ts:32-34`.
const DEFAULT_ROUTE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_HYDRATION_TIMEOUT: Duration = Duration::from_secs(60);
const DEFAULT_MIN_BODY_TEXT_LENGTH: usize = 20;
const HYDRATION_POLL: Duration = Duration::from_millis(250);

/// What stops the gate from being configured.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// The YAML could not be read or parsed.
    #[error("{path}: {message}")]
    Config {
        /// The gate config.
        path: PathBuf,
        /// What was wrong.
        message: String,
    },
    /// `playwrightGate.ts:195`.
    #[error("Playwright gate config {0} requires server.command and server.url.")]
    MissingServer(PathBuf),
    /// `playwrightGate.ts:199`.
    #[error("Playwright gate config {0} requires at least one route.")]
    NoRoutes(PathBuf),
}

/// `playwrightGate.ts:23-26`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Route {
    /// Appears in finding ids.
    pub(crate) name: String,
    /// Joined onto the dev server URL.
    pub(crate) path: String,
}

/// `playwrightGate.ts:8-30`, defaults applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GateConfig {
    /// How to start and reach the app.
    pub(crate) server: devserver::Config,
    /// At least one.
    pub(crate) routes: Vec<Route>,
    /// Navigation limit per route.
    pub(crate) route_timeout: Duration,
    /// How long to poll for hydrated body text.
    pub(crate) hydration_timeout: Duration,
    /// Trimmed `body.innerText` length that counts as rendered.
    pub(crate) min_body_text_length: usize,
    /// Console errors fail the route.
    pub(crate) fail_on_console_error: bool,
    /// `forbidden.visibleText`.
    pub(crate) forbidden_text: Vec<String>,
}

/// `playwrightGate.ts:190-203` plus `devServer.ts:74-89`, one read.
pub(crate) fn load_gate_config(path: &Path) -> Result<GateConfig, Error> {
    let raw = std::fs::read_to_string(path).map_err(|e| Error::Config {
        path: path.to_path_buf(),
        message: e.to_string(),
    })?;
    let parsed: Value = serde_yaml_ng::from_str(&raw).map_err(|e| Error::Config {
        path: path.to_path_buf(),
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
        return Err(Error::MissingServer(path.to_path_buf()));
    };
    let routes: Vec<Route> = parsed
        .get("routes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|route| {
            Some(Route {
                name: route.get("name")?.as_str()?.to_string(),
                path: route.get("path")?.as_str()?.to_string(),
            })
        })
        .collect();
    if routes.is_empty() {
        return Err(Error::NoRoutes(path.to_path_buf()));
    }
    let defaults = parsed.get("defaults");
    let millis = |key: &str, fallback: Duration| {
        defaults
            .and_then(|d| d.get(key))
            .and_then(Value::as_u64)
            .map_or(fallback, Duration::from_millis)
    };
    Ok(GateConfig {
        server: devserver::Config {
            command,
            configured_url: url,
            timeout: server
                .and_then(|s| s.get("timeoutMs"))
                .and_then(Value::as_u64)
                .map_or(Duration::from_secs(120), Duration::from_millis),
        },
        routes,
        route_timeout: millis("timeoutMs", DEFAULT_ROUTE_TIMEOUT),
        hydration_timeout: millis("hydrationTimeoutMs", DEFAULT_HYDRATION_TIMEOUT),
        min_body_text_length: defaults
            .and_then(|d| d.get("minBodyTextLength"))
            .and_then(Value::as_u64)
            .map_or(DEFAULT_MIN_BODY_TEXT_LENGTH, |n| n as usize),
        fail_on_console_error: defaults
            .and_then(|d| d.get("failOnConsoleError"))
            .and_then(Value::as_bool)
            .unwrap_or(true),
        forbidden_text: parsed
            .get("forbidden")
            .and_then(|f| f.get("visibleText"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    })
}

fn finding(route: &Route, kind: &str, message: String) -> Finding {
    Finding::new(
        format!("playwright:route:{}:{kind}", route.name),
        Category::Playwright,
        message,
    )
}

/// `playwrightGate.ts:42-188`: walk the routes against a running dev server. The browser is
/// the MCP server's; one session, one page, reused across routes as upstream does.
pub(crate) async fn run_gate(
    workspace: &Path,
    config_path: &Path,
    gate: &GateConfig,
    dev_server: &mut devserver::Session,
    output_dir: &Path,
    choice: &BrowserChoice,
) -> (PlaywrightGateResult, Vec<Finding>) {
    let started = Instant::now();
    let server_url = dev_server.url.clone();
    let mut routes = Vec::new();
    let mut findings = Vec::new();
    let mut browser_launch_ms = None;
    let gate_failed = |details: String| {
        Finding::new(
            "playwright:gate",
            Category::Playwright,
            "Playwright gate failed before route checks completed",
        )
        .with_details(details)
    };

    let client = Client::spawn(choice, output_dir, workspace).await;
    match client {
        Err(error) => findings.push(gate_failed(error.to_string())),
        Ok(mut client) => {
            // `launchSharedBrowser` (`playwrightGate.ts:80`) runs before the first route. The
            // MCP server launches its browser on the first navigation, so navigate once here:
            // a cold Chromium start on a loaded CI runner is not a route's timeout to pay.
            let launch = Instant::now();
            let warmed = client
                .call(
                    "browser_navigate",
                    json!({ "url": "about:blank" }),
                    mcp::LAUNCH_TIMEOUT,
                )
                .await;
            match warmed {
                Ok(call) if !call.is_error => {
                    let elapsed = launch.elapsed().as_millis() as u64;
                    browser_launch_ms = Some(elapsed);
                    log_phase("SMOKE browser ready", Some(&format!("{elapsed} ms")));
                    for route in &gate.routes {
                        let (result, route_findings) =
                            check_route(&mut client, gate, route, &server_url, dev_server).await;
                        routes.push(result);
                        findings.extend(route_findings);
                    }
                }
                Ok(call) => findings.push(gate_failed(call.text)),
                Err(error) => findings.push(gate_failed(error.to_string())),
            }
            client.close().await;
        }
    }

    let result = PlaywrightGateResult {
        passed: findings.is_empty(),
        config_path: config_path.display().to_string(),
        server_url,
        server_command: dev_server.command.clone(),
        routes,
        duration_ms: started.elapsed().as_millis() as u64,
        browser_launch_ms,
    };
    (result, findings)
}

/// `playwrightGate.ts:65-161`.
async fn check_route(
    client: &mut Client,
    gate: &GateConfig,
    route: &Route,
    server_url: &str,
    dev_server: &mut devserver::Session,
) -> (RouteResult, Vec<Finding>) {
    let started = Instant::now();
    let url = join_url(server_url, &route.path);
    let mut findings = Vec::new();
    let mut rendered = false;
    let mut status_code = None;
    let mut last_body_text = String::new();

    let navigated = client
        .call(
            "browser_navigate",
            json!({ "url": url }),
            gate.route_timeout,
        )
        .await;
    match navigated {
        Err(error) => findings.push(
            finding(
                route,
                "navigation",
                format!(
                    "Playwright gate failed to load route {} ({})",
                    route.path, route.name
                ),
            )
            .with_details(error.to_string()),
        ),
        Ok(call) if call.is_error => findings.push(
            finding(
                route,
                "navigation",
                format!(
                    "Playwright gate failed to load route {} ({})",
                    route.path, route.name
                ),
            )
            .with_details(call.text),
        ),
        Ok(_) => {
            status_code = read_status(client).await;
            match wait_for_body_text(client, gate, dev_server).await {
                Ok((hydrated, text)) => {
                    rendered = hydrated;
                    last_body_text = text;
                }
                Err(message) => findings.push(
                    finding(
                        route,
                        "navigation",
                        format!(
                            "Playwright gate failed to load route {} ({})",
                            route.path, route.name
                        ),
                    )
                    .with_details(message),
                ),
            }
        }
    }

    let forbidden_found = forbidden_text_found(client, &gate.forbidden_text).await;
    if let Some(status) = status_code
        && status >= 400
    {
        findings.push(finding(
            route,
            "status",
            format!(
                "Playwright gate route {} returned HTTP {status}",
                route.path
            ),
        ));
    }
    if !rendered {
        findings.push(
            finding(
                route,
                "render",
                format!(
                    "Playwright gate route {} did not render meaningful page content within {}ms",
                    route.path,
                    gate.hydration_timeout.as_millis()
                ),
            )
            .with_details(format!(
                "Last body text length={} (need >= {}): {}",
                last_body_text.len(),
                gate.min_body_text_length,
                {
                    let shown = truncate_collapsed(&last_body_text, 200);
                    if shown.is_empty() {
                        "(empty)".to_string()
                    } else {
                        shown
                    }
                }
            )),
        );
    }
    let console_errors = console_errors(client).await;
    if gate.fail_on_console_error && !console_errors.is_empty() {
        findings.push(
            finding(
                route,
                "console",
                format!(
                    "Playwright gate route {} logged browser console errors",
                    route.path
                ),
            )
            .with_details(truncate_list(&console_errors)),
        );
    }
    for text in &forbidden_found {
        findings.push(finding(
            route,
            &format!("forbidden:{}", slugify(text)),
            format!(
                "Playwright gate route {} contains forbidden text: \"{text}\"",
                route.path
            ),
        ));
    }

    (
        RouteResult {
            name: route.name.clone(),
            path: route.path.clone(),
            status_code,
            rendered,
            console_errors,
            forbidden_text_found: forbidden_found,
            duration_ms: started.elapsed().as_millis() as u64,
        },
        findings,
    )
}

/// Run a function in the page and return the object it produced. Every function here returns
/// an object carrying `hanvil: 1`, so it is picked out of the server's markdown regardless of
/// what else the response contains (the echoed code has braces of its own).
async fn evaluate(client: &mut Client, function: &str, timeout: Duration) -> Option<Value> {
    let call = client
        .call("browser_evaluate", json!({ "function": function }), timeout)
        .await
        .ok()?;
    if call.is_error {
        return None;
    }
    balanced_json_objects(&call.text)
        .into_iter()
        .filter_map(|candidate| serde_json::from_str::<Value>(&candidate).ok())
        .find(|value| value.get("hanvil").is_some())
}

/// `response.status()` for the document: `PerformanceNavigationTiming.responseStatus`.
async fn read_status(client: &mut Client) -> Option<u16> {
    let value = evaluate(
        client,
        "() => { const e = performance.getEntriesByType('navigation')[0]; const s = e && typeof e.responseStatus === 'number' && e.responseStatus > 0 ? e.responseStatus : null; return { hanvil: 1, status: s }; }",
        Duration::from_secs(10),
    )
    .await?;
    value
        .get("status")
        .and_then(Value::as_u64)
        .map(|status| status as u16)
}

/// `playwrightGate.ts:226-264`: poll `body.innerText` until it is long enough, or the deadline;
/// a dead dev server ends the wait at once.
async fn wait_for_body_text(
    client: &mut Client,
    gate: &GateConfig,
    dev_server: &mut devserver::Session,
) -> Result<(bool, String), String> {
    let deadline = Instant::now() + gate.hydration_timeout;
    let mut body_text = String::new();
    loop {
        if !dev_server.is_alive() {
            return Err(format!(
                "Dev server exited while waiting for page hydration (last body text length={}).",
                body_text.len()
            ));
        }
        if let Some(value) = evaluate(
            client,
            "() => { const b = document.body; const t = b ? b.innerText.trim() : ''; return { hanvil: 1, length: t.length, text: t.slice(0, 200) }; }",
            Duration::from_secs(5),
        )
        .await
        {
            let length = value.get("length").and_then(Value::as_u64).unwrap_or(0) as usize;
            body_text = value
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            if length >= gate.min_body_text_length {
                return Ok((true, body_text));
            }
        }
        if Instant::now() >= deadline {
            return Ok((false, body_text));
        }
        tokio::time::sleep(HYDRATION_POLL).await;
    }
}

/// `page.getByText(text, { exact: false })`: a case-insensitive substring of the visible text.
async fn forbidden_text_found(client: &mut Client, forbidden: &[String]) -> Vec<String> {
    if forbidden.is_empty() {
        return Vec::new();
    }
    let needles = serde_json::to_string(forbidden).unwrap_or_else(|_| "[]".to_string());
    let function = format!(
        "() => {{ const t = (document.body ? document.body.innerText : '').toLowerCase(); return {{ hanvil: 1, found: {needles}.filter(x => t.includes(String(x).toLowerCase())) }}; }}"
    );
    evaluate(client, &function, Duration::from_secs(10))
        .await
        .and_then(|value| value.get("found").cloned())
        .and_then(|found| serde_json::from_value::<Vec<String>>(found).ok())
        .unwrap_or_default()
}

/// `browser_console_messages` at level `error`: a `Total messages: N (Errors: E, …)` header, a
/// blank line, then one message per line.
async fn console_errors(client: &mut Client) -> Vec<String> {
    let Ok(call) = client
        .call(
            "browser_console_messages",
            json!({ "level": "error" }),
            Duration::from_secs(10),
        )
        .await
    else {
        return Vec::new();
    };
    parse_console_errors(&call.text)
}

fn parse_console_errors(text: &str) -> Vec<String> {
    let mut lines = text.lines();
    let Some(header) = lines.find(|line| line.starts_with("Total messages:")) else {
        return Vec::new();
    };
    let errors: usize = header
        .split("Errors: ")
        .nth(1)
        .and_then(|rest| rest.split([',', ')']).next())
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
    if errors == 0 {
        return Vec::new();
    }
    let mut messages: Vec<String> = lines
        .skip_while(|line| !line.trim().is_empty())
        .filter(|line| !line.trim().is_empty() && !line.starts_with("###"))
        .map(|line| line.trim().to_string())
        .collect();
    messages.truncate(errors.max(1));
    if messages.is_empty() {
        messages.push(format!("{errors} console error(s)"));
    }
    messages
}

/// `playwrightGate.ts:205-209`.
pub(crate) fn join_url(base: &str, route_path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        route_path.trim_start_matches('/')
    )
}

/// `playwrightGate.ts:276-285`: first five, each cut at 200, `…and N more`.
fn truncate_list(values: &[String]) -> String {
    let mut joined = values
        .iter()
        .take(5)
        .map(|value| truncate_collapsed(value, 200))
        .collect::<Vec<_>>()
        .join("\n");
    if values.len() > 5 {
        joined.push_str(&format!("\n...and {} more", values.len() - 5));
    } else {
        joined.truncate(800);
    }
    joined
}

fn truncate_collapsed(value: &str, max: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max {
        collapsed
    } else {
        format!("{}...", collapsed.chars().take(max).collect::<String>())
    }
}

/// `playwrightGate.ts:287-293`.
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
    let slug: String = slug.chars().take(48).collect();
    if slug.is_empty() {
        "text".to_string()
    } else {
        slug
    }
}

/// The browser choice and output directory the gate and the validator share.
pub(crate) fn output_dir(run_directory: &Path) -> PathBuf {
    run_directory.join("mcp").join("output")
}

/// Re-exported so the attempt loop resolves once and passes the same choice to both stages.
pub(crate) fn resolve_browser() -> BrowserChoice {
    mcp::resolve_browser()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_gate_config_applies_upstreams_defaults() {
        let dir = std::env::temp_dir().join(format!("hanvil-smoke-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("smoke.yaml");
        std::fs::write(
            &path,
            "server:\n  command: yarn dev\n  url: http://localhost:3000\nroutes:\n  - name: home\n    path: /\n  - name: about\n    path: /about\nforbidden:\n  visibleText: [\"Application error\"]\n",
        )
        .expect("write");
        let gate = load_gate_config(&path).expect("config");
        assert_eq!(gate.routes.len(), 2);
        assert_eq!(gate.route_timeout, Duration::from_secs(30));
        assert_eq!(gate.hydration_timeout, Duration::from_secs(60));
        assert_eq!(gate.min_body_text_length, 20);
        assert!(gate.fail_on_console_error);
        assert_eq!(gate.forbidden_text, vec!["Application error"]);
        assert_eq!(gate.server.timeout, Duration::from_secs(120));

        std::fs::write(
            &path,
            "server:\n  command: x\n  url: http://l\nroutes: []\n",
        )
        .expect("write");
        assert_eq!(
            load_gate_config(&path).expect_err("no routes").to_string(),
            format!(
                "Playwright gate config {} requires at least one route.",
                path.display()
            )
        );
        std::fs::write(&path, "routes:\n  - {name: a, path: /}\n").expect("write");
        assert_eq!(
            load_gate_config(&path).expect_err("no server").to_string(),
            format!(
                "Playwright gate config {} requires server.command and server.url.",
                path.display()
            )
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn helpers_follow_upstream() {
        assert_eq!(
            join_url("http://localhost:3000", "/about"),
            "http://localhost:3000/about"
        );
        assert_eq!(
            join_url("http://localhost:3000/", "about"),
            "http://localhost:3000/about"
        );
        assert_eq!(slugify("Application error!"), "application-error");
        assert_eq!(slugify("***"), "text");
        let many: Vec<String> = (1..=7).map(|i| format!("error {i}")).collect();
        assert_eq!(
            truncate_list(&many),
            "error 1\nerror 2\nerror 3\nerror 4\nerror 5\n...and 2 more"
        );
        assert_eq!(
            parse_console_errors(
                "Total messages: 3 (Errors: 2, Warnings: 1)\nReturning 2 messages for level \"error\"\n\n[ERROR] boom @ http://x\n[ERROR] again\n"
            ),
            vec!["[ERROR] boom @ http://x", "[ERROR] again"]
        );
        assert!(parse_console_errors("Total messages: 0 (Errors: 0, Warnings: 0)\n").is_empty());
        assert!(parse_console_errors("No console messages").is_empty());
    }
}
