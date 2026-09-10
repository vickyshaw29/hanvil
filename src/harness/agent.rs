//! Spawning the coding agent and watching it work. `providers/commandAgentProvider.ts`,
//! `agentStreamLogger.ts` and `modelSelection.ts` of hedera-harness dev @ 587a2f3.
//!
//! The prompt travels in argv; stdin is closed. Both pipes are drained from the first byte, the
//! idle timer restarts on every chunk of either, and a timeout stops the agent's whole process
//! group. The raw log keeps the stream verbatim with the prompt redacted; the activity log keeps
//! one line per notable event, and understands both Claude's and Cursor's `stream-json`.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::harness::artifacts::now_iso8601;
use crate::harness::command::{self, BoundedOutput};
use crate::harness::env;
use crate::harness::spec::{AgentPreset, CommandConfig};

const PROMPT_PLACEHOLDER: &str = "{prompt}";
const WORKSPACE_PLACEHOLDER: &str = "{workspace}";
/// `commandAgentProvider.ts:9`.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30 * 60);
/// `commandAgentProvider.ts:75-76`. The verdict arrives last, so the tail is the part that must
/// survive a chatty run.
const STDOUT_HEAD: usize = 256 * 1024;
const STDOUT_TAIL: usize = 3 * 1024 * 1024;
const STDERR_HEAD: usize = 128 * 1024;
const STDERR_TAIL: usize = 512 * 1024;

/// What can go wrong before the agent has a chance to.
#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    /// `commandAgentProvider.ts:31`.
    #[error("Command agent provider requires a non-empty command.")]
    EmptyCommand,
    /// `commandAgentProvider.ts:42`.
    #[error("Agent run requires a workspace path.")]
    EmptyWorkspace,
    /// `commandAgentProvider.ts:46`.
    #[error("Agent run requires a non-empty prompt.")]
    EmptyPrompt,
    /// The binary could not be started.
    #[error("spawning agent {command}: {source}")]
    Spawn {
        /// The binary.
        command: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// A log file could not be written.
    #[error("writing {path}: {source}")]
    Log {
        /// The log.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
}

/// `agentStreamLogger.ts:3-8`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Progress {
    /// The last summarised event.
    pub(crate) last_activity: String,
    /// Tool calls seen starting.
    pub(crate) tool_calls_started: u64,
    /// Tool calls seen finishing.
    pub(crate) tool_calls_completed: u64,
    /// The agent's session id, once it said one.
    pub(crate) session_id: Option<String>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            last_activity: "waiting for agent output".to_string(),
            tool_calls_started: 0,
            tool_calls_completed: 0,
            session_id: None,
        }
    }
}

/// Called after every activity line; the attempt loop uses it to keep `status.json` current.
pub(crate) type ProgressSink = Arc<dyn Fn(&Progress) + Send + Sync>;

/// `types.ts` `AgentRunInput`.
pub(crate) struct RunInput<'a> {
    /// Delivered in argv.
    pub(crate) prompt: &'a str,
    /// The agent's working directory.
    pub(crate) workspace: &'a Path,
    /// Raw stream log, overwritten.
    pub(crate) log_path: Option<&'a Path>,
    /// Activity log, overwritten.
    pub(crate) activity_log_path: Option<&'a Path>,
    /// Wall-clock override.
    pub(crate) timeout: Option<Duration>,
    /// Called after every activity line.
    pub(crate) on_progress: Option<ProgressSink>,
}

/// `types.ts` `AgentRunResult`.
#[derive(Debug, Clone)]
pub(crate) struct RunResult {
    /// `None` when killed by a signal.
    pub(crate) exit_code: Option<i32>,
    /// Bounded capture.
    pub(crate) stdout: String,
    /// Bounded capture, plus the idle notice when the harness stopped the agent for silence.
    pub(crate) stderr: String,
    /// Wall time.
    pub(crate) duration_ms: u64,
    /// The binary.
    pub(crate) command: String,
    /// Argv as run, prompt included.
    pub(crate) args: Vec<String>,
    /// The harness stopped it, for either reason.
    pub(crate) timed_out: bool,
    /// Signal name when killed.
    pub(crate) signal: Option<String>,
}

/// `commandAgentProvider.ts:26-181`.
pub(crate) struct Provider {
    config: CommandConfig,
    preset: AgentPreset,
    idle_timeout: Duration,
}

impl Provider {
    /// Wrap a resolved `generator:`/`validator:` config. The idle timeout comes from the
    /// environment and the preset.
    pub(crate) fn new(config: CommandConfig, preset: AgentPreset) -> Result<Self, Error> {
        if config.command.trim().is_empty() {
            return Err(Error::EmptyCommand);
        }
        Ok(Self {
            config,
            preset,
            idle_timeout: env::agent_idle_timeout(preset),
        })
    }

    /// Override the idle timeout.
    pub(crate) fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout;
        self
    }

    /// Add environment on top of the config's own; the config's entries win on a clash.
    pub(crate) fn with_env(mut self, extra: std::collections::BTreeMap<String, String>) -> Self {
        let mut merged = extra;
        if let Some(own) = self.config.env.take() {
            merged.extend(own);
        }
        self.config.env = Some(merged);
        self
    }

    /// Run the agent once and wait for it.
    pub(crate) async fn run(&self, input: RunInput<'_>) -> Result<RunResult, Error> {
        if input.workspace.as_os_str().is_empty() {
            return Err(Error::EmptyWorkspace);
        }
        if input.prompt.trim().is_empty() {
            return Err(Error::EmptyPrompt);
        }
        let started = Instant::now();
        let args = build_args(
            self.config.args.as_deref().unwrap_or_default(),
            input.prompt,
            input.workspace,
        );
        let timeout = input
            .timeout
            .or(self.config.timeout_ms.map(Duration::from_millis))
            .unwrap_or(DEFAULT_TIMEOUT);
        let idle_timeout = self.idle_timeout;

        let raw_log = match input.log_path {
            Some(path) => Some(RawLog::create(
                path,
                &self.config.command,
                &redact_prompt_args(&args, input.prompt),
                timeout,
                idle_timeout,
            )?),
            None => None,
        };
        let raw_log = Arc::new(Mutex::new(raw_log));
        let activity = match input.activity_log_path {
            Some(path) => Some(StreamLogger::create(path, input.on_progress.clone())?),
            None => None,
        };
        let activity = Arc::new(Mutex::new(activity));

        let mut command = tokio::process::Command::new(&self.config.command);
        command
            .args(&args)
            .current_dir(input.workspace)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true);
        if let Some(extra) = &self.config.env {
            command.envs(extra);
        }
        // A `claude -p` started from inside a Claude Code session sees that session's variables
        // and treats itself as nested. The agent is a fresh session; upstream passes the whole
        // environment through because it never runs from inside one.
        if self.preset == AgentPreset::Claude {
            for (key, _) in std::env::vars_os() {
                let key_text = key.to_string_lossy();
                if key_text == "CLAUDECODE" || key_text.starts_with("CLAUDE_CODE_") {
                    command.env_remove(&key);
                }
            }
        }
        let mut child = command.spawn().map_err(|source| Error::Spawn {
            command: self.config.command.clone(),
            source,
        })?;
        let pgid = child.id();

        let last_activity = Arc::new(Mutex::new(Instant::now()));
        let stdout = Arc::new(Mutex::new(BoundedOutput::with_limits(
            STDOUT_HEAD,
            STDOUT_TAIL,
        )));
        let stderr = Arc::new(Mutex::new(BoundedOutput::with_limits(
            STDERR_HEAD,
            STDERR_TAIL,
        )));
        let stdout_reader = child.stdout.take().map(|pipe| {
            let touch = Arc::clone(&last_activity);
            let raw_log = Arc::clone(&raw_log);
            let activity = Arc::clone(&activity);
            command::drain(pipe, Arc::clone(&stdout), move |chunk| {
                touch_now(&touch);
                with_lock(&raw_log, |log| {
                    if let Some(log) = log {
                        log.append(chunk);
                    }
                });
                with_lock(&activity, |logger| {
                    if let Some(logger) = logger {
                        logger.process_chunk(&String::from_utf8_lossy(chunk));
                    }
                });
            })
        });
        let stderr_reader = child.stderr.take().map(|pipe| {
            let touch = Arc::clone(&last_activity);
            let raw_log = Arc::clone(&raw_log);
            command::drain(pipe, Arc::clone(&stderr), move |chunk| {
                touch_now(&touch);
                let text = String::from_utf8_lossy(chunk);
                with_lock(&raw_log, |log| {
                    if let Some(log) = log {
                        log.append(format!("\n## stderr\n{text}").as_bytes());
                    }
                });
                println!("[hanvil:agent:stderr] {}", truncate_collapsed(&text, 300));
            })
        });

        let wall_deadline = tokio::time::Instant::now() + timeout;
        let mut timed_out = false;
        let mut idle_timed_out = false;
        let status = loop {
            let idle_deadline =
                tokio::time::Instant::from_std(idle_at(&last_activity) + idle_timeout);
            tokio::select! {
                status = child.wait() => break status,
                () = tokio::time::sleep_until(wall_deadline) => {
                    timed_out = true;
                    self.settle(&mut child, pgid, "wall-clock", timeout, started, &raw_log, &activity).await;
                    break child.wait().await;
                }
                () = tokio::time::sleep_until(idle_deadline) => {
                    if idle_at(&last_activity) + idle_timeout > Instant::now() {
                        continue; // output arrived while we slept
                    }
                    timed_out = true;
                    idle_timed_out = true;
                    self.settle(&mut child, pgid, "idle", idle_timeout, started, &raw_log, &activity).await;
                    break child.wait().await;
                }
            }
        };
        let status = status.map_err(|source| Error::Spawn {
            command: self.config.command.clone(),
            source,
        })?;
        if let Some(reader) = stdout_reader {
            let _ = reader.await;
        }
        if let Some(reader) = stderr_reader {
            let _ = reader.await;
        }

        let mut stderr_text = command::lock_render(&stderr);
        if idle_timed_out {
            stderr_text.push_str(&format!(
                "\n[hanvil] Agent produced no output for {}ms; treating as failure.\n",
                idle_timeout.as_millis()
            ));
        }
        let result = RunResult {
            exit_code: status.code(),
            stdout: command::lock_render(&stdout),
            stderr: stderr_text,
            duration_ms: started.elapsed().as_millis() as u64,
            command: self.config.command.clone(),
            args,
            timed_out,
            signal: command::signal_name(&status),
        };
        let progress = with_lock(&activity, |logger| {
            logger.as_ref().map(StreamLogger::progress)
        });
        with_lock(&raw_log, |log| {
            if let Some(log) = log {
                log.finish(&result, progress.as_ref());
            }
        });
        Ok(result)
    }

    /// `commandAgentProvider.ts:92-114`.
    #[allow(clippy::too_many_arguments)]
    async fn settle(
        &self,
        child: &mut tokio::process::Child,
        pgid: Option<u32>,
        reason: &str,
        limit: Duration,
        started: Instant,
        raw_log: &Mutex<Option<RawLog>>,
        activity: &Mutex<Option<StreamLogger>>,
    ) {
        let prefix = if reason == "idle" { "idle-" } else { "" };
        println!(
            "[hanvil] Agent {prefix}timeout after {}s — stopping agent",
            limit.as_secs_f64().round() as u64
        );
        with_lock(raw_log, |log| {
            if let Some(log) = log {
                log.append(
                    format!(
                        "\n## harness\nagent {prefix}timed out after {}ms\n",
                        limit.as_millis()
                    )
                    .as_bytes(),
                );
            }
        });
        let synthetic = serde_json::json!({
            "type": "result",
            "subtype": if reason == "idle" { "idle_timeout" } else { "timeout" },
            "is_error": true,
            "duration_ms": started.elapsed().as_millis() as u64,
        });
        with_lock(activity, |logger| {
            if let Some(logger) = logger {
                logger.process_chunk(&format!("{synthetic}\n"));
            }
        });
        command::signal_group(child, pgid, "TERM").await;
        if tokio::time::timeout(command::KILL_GRACE, child.wait())
            .await
            .is_err()
        {
            command::signal_group(child, pgid, "KILL").await;
        }
    }
}

fn touch_now(last: &Mutex<Instant>) {
    if let Ok(mut guard) = last.lock() {
        *guard = Instant::now();
    }
}

fn idle_at(last: &Mutex<Instant>) -> Instant {
    last.lock()
        .map(|guard| *guard)
        .unwrap_or_else(|_| Instant::now())
}

fn with_lock<T, R>(mutex: &Mutex<T>, f: impl FnOnce(&mut T) -> R) -> R {
    match mutex.lock() {
        Ok(mut guard) => f(&mut guard),
        Err(poisoned) => f(&mut poisoned.into_inner()),
    }
}

/// `commandAgentProvider.ts:253-264`. Replace the placeholders in every argument; when none
/// carries the prompt, append it as the last argument (that is how Cursor receives it).
pub(crate) fn build_args(config_args: &[String], prompt: &str, workspace: &Path) -> Vec<String> {
    let workspace = workspace.to_string_lossy();
    let mut replaced: Vec<String> = config_args
        .iter()
        .map(|arg| {
            arg.replace(WORKSPACE_PLACEHOLDER, &workspace)
                .replace(PROMPT_PLACEHOLDER, prompt)
        })
        .collect();
    if !config_args
        .iter()
        .any(|arg| arg.contains(PROMPT_PLACEHOLDER))
    {
        replaced.push(prompt.to_string());
    }
    replaced
}

/// `commandAgentProvider.ts:191-193`. Prompts can carry the signer's private key, so any
/// argument containing the prompt is replaced wherever it sits.
pub(crate) fn redact_prompt_args(args: &[String], prompt: &str) -> Vec<String> {
    args.iter()
        .map(|arg| {
            if arg.contains(prompt) {
                "<prompt redacted>".to_string()
            } else {
                arg.clone()
            }
        })
        .collect()
}

/// `commandAgentProvider.ts:266-270`.
fn truncate_collapsed(value: &str, max: usize) -> String {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, max)
}

/// `agentStreamLogger.ts:147-151`: trim, then cut with `...`.
fn truncate(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.chars().count() <= max {
        trimmed.to_string()
    } else {
        format!("{}...", trimmed.chars().take(max).collect::<String>())
    }
}

/// `commandAgentProvider.ts:195-251`: the raw stream log.
struct RawLog {
    path: PathBuf,
    file: std::fs::File,
}

impl RawLog {
    fn create(
        path: &Path,
        command: &str,
        redacted_args: &[String],
        timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<Self, Error> {
        let header = format!(
            "# agent raw stream log\ncommand={command}\nargs={}\ntimeoutMs={}\nidleTimeoutMs={}\n\n## stdout\n\n",
            serde_json::to_string(redacted_args).unwrap_or_default(),
            timeout.as_millis(),
            idle_timeout.as_millis()
        );
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|source| Error::Log {
                path: path.to_path_buf(),
                source,
            })?;
        file.write_all(header.as_bytes())
            .map_err(|source| Error::Log {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
        })
    }

    fn append(&mut self, chunk: &[u8]) {
        if self.file.write_all(chunk).is_err() {
            eprintln!("[hanvil] could not append to {}", self.path.display());
        }
    }

    fn finish(&mut self, result: &RunResult, progress: Option<&Progress>) {
        let mut lines = vec![
            String::new(),
            "## harness".to_string(),
            format!(
                "exitCode={}",
                result
                    .exit_code
                    .map_or_else(|| "null".to_string(), |c| c.to_string())
            ),
            format!("timedOut={}", result.timed_out),
            format!("durationMs={}", result.duration_ms),
            format!("signal={}", result.signal.as_deref().unwrap_or("null")),
            format!("stdoutBytes={}", result.stdout.len()),
            format!("stderrBytes={}", result.stderr.len()),
        ];
        if let Some(progress) = progress {
            lines.push(format!("lastActivity={}", progress.last_activity));
            lines.push(format!("toolCallsStarted={}", progress.tool_calls_started));
            lines.push(format!(
                "toolCallsCompleted={}",
                progress.tool_calls_completed
            ));
            if let Some(session) = &progress.session_id {
                lines.push(format!("sessionId={session}"));
            }
        }
        lines.push(String::new());
        self.append(lines.join("\n").as_bytes());
    }
}

/// `agentStreamLogger.ts:10-74`: one human-readable line per notable event.
pub(crate) struct StreamLogger {
    path: PathBuf,
    file: std::fs::File,
    line_buffer: String,
    progress: Progress,
    /// Claude reports a tool's result by id only; remember the name from the call.
    tool_names: HashMap<String, String>,
    on_progress: Option<ProgressSink>,
}

impl StreamLogger {
    fn create(path: &Path, on_progress: Option<ProgressSink>) -> Result<Self, Error> {
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .map_err(|source| Error::Log {
                path: path.to_path_buf(),
                source,
            })?;
        file.write_all(b"# agent activity log\n# one human-readable line per notable event\n")
            .map_err(|source| Error::Log {
                path: path.to_path_buf(),
                source,
            })?;
        Ok(Self {
            path: path.to_path_buf(),
            file,
            line_buffer: String::new(),
            progress: Progress::default(),
            tool_names: HashMap::new(),
            on_progress,
        })
    }

    fn progress(&self) -> Progress {
        self.progress.clone()
    }

    /// Feed raw stream text; complete lines are summarised, the rest waits for more.
    pub(crate) fn process_chunk(&mut self, chunk: &str) {
        self.line_buffer.push_str(chunk);
        while let Some(newline) = self.line_buffer.find('\n') {
            let line = self.line_buffer[..newline].trim().to_string();
            self.line_buffer.drain(..=newline);
            if !line.is_empty() {
                self.process_line(&line);
            }
        }
    }

    fn process_line(&mut self, line: &str) {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            return;
        };
        let summaries = summarize_stream_event(&event, &mut self.tool_names);
        if summaries.is_empty() {
            return;
        }
        if let Some(session) = event.get("session_id").and_then(Value::as_str) {
            self.progress.session_id = Some(session.to_string());
        }
        for (summary, counts) in summaries {
            match counts {
                ToolCount::Started => self.progress.tool_calls_started += 1,
                ToolCount::Completed => self.progress.tool_calls_completed += 1,
                ToolCount::None => {}
            }
            self.progress.last_activity = summary.clone();
            let stamped = format!("{} {summary}\n", now_iso8601());
            if self.file.write_all(stamped.as_bytes()).is_err() {
                eprintln!("[hanvil] could not append to {}", self.path.display());
            }
            println!("[hanvil:agent] {summary}");
            if let Some(callback) = &self.on_progress {
                callback(&self.progress);
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolCount {
    None,
    Started,
    Completed,
}

/// `agentStreamLogger.ts:76-141` for Cursor's events, plus Claude's `assistant`/`user` message
/// shapes, which upstream's summariser does not know: under the default preset it only ever
/// printed the session start and the final result.
fn summarize_stream_event(
    event: &Value,
    tool_names: &mut HashMap<String, String>,
) -> Vec<(String, ToolCount)> {
    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
    let subtype = event.get("subtype").and_then(Value::as_str);
    match event_type {
        "system" if subtype == Some("init") => {
            let model = event
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or("unknown-model");
            vec![(format!("SESSION started model={model}"), ToolCount::None)]
        }
        "tool_call" => vec![summarize_cursor_tool_call(event, subtype)],
        "assistant" => summarize_claude_tool_uses(event, tool_names),
        "user" => summarize_claude_tool_results(event, tool_names),
        "result" => {
            let subtype = subtype.unwrap_or("unknown");
            let is_error = event.get("is_error") == Some(&Value::Bool(true));
            let duration = event
                .get("duration_ms")
                .and_then(Value::as_u64)
                .filter(|ms| *ms > 0);
            let mut line = format!("RESULT {subtype}");
            if is_error {
                line.push_str(" error");
            }
            if let Some(ms) = duration {
                line.push_str(&format!(" durationMs={ms}"));
            }
            vec![(line, ToolCount::None)]
        }
        "thinking" if subtype == Some("completed") => {
            vec![("THINKING completed".to_string(), ToolCount::None)]
        }
        _ => Vec::new(),
    }
}

/// `agentStreamLogger.ts:84-127`.
fn summarize_cursor_tool_call(event: &Value, subtype: Option<&str>) -> (String, ToolCount) {
    let (phase, count) = match subtype {
        Some("started") => ("START", ToolCount::Started),
        Some("completed") => ("DONE", ToolCount::Completed),
        _ => ("CALL", ToolCount::None),
    };
    let Some(tool_call) = event.get("tool_call").and_then(Value::as_object) else {
        return (format!("TOOL {phase}"), count);
    };
    let (tool_name, payload) = tool_call
        .iter()
        .find(|(key, _)| key.ends_with("ToolCall"))
        .map_or(("tool", None), |(key, value)| (key.as_str(), Some(value)));
    let empty = serde_json::Map::new();
    let args = payload
        .and_then(|p| p.get("args"))
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let string_arg = |key: &str| args.get(key).and_then(Value::as_str);
    let line = match tool_name {
        "editToolCall" if string_arg("path").is_some() => {
            format!("TOOL {phase} edit {}", string_arg("path").unwrap_or(""))
        }
        "shellToolCall" | "runTerminalCommandToolCall" => {
            let command = string_arg("command")
                .map(str::to_string)
                .unwrap_or_else(|| Value::Object(args.clone()).to_string());
            format!("TOOL {phase} shell {}", truncate(&command, 160))
        }
        "readToolCall" if string_arg("path").is_some() => {
            format!("TOOL {phase} read {}", string_arg("path").unwrap_or(""))
        }
        "grepToolCall" if string_arg("pattern").is_some() => format!(
            "TOOL {phase} grep {}",
            truncate(string_arg("pattern").unwrap_or(""), 80)
        ),
        "globToolCall" if string_arg("globPattern").is_some() => {
            format!(
                "TOOL {phase} glob {}",
                string_arg("globPattern").unwrap_or("")
            )
        }
        "deleteToolCall" if string_arg("path").is_some() => {
            format!("TOOL {phase} delete {}", string_arg("path").unwrap_or(""))
        }
        other => format!(
            "TOOL {phase} {} {}",
            other.trim_end_matches("ToolCall"),
            truncate(&Value::Object(args.clone()).to_string(), 120)
        ),
    };
    (line, count)
}

/// Claude: `{"type":"assistant","message":{"content":[{"type":"tool_use","id","name","input"}]}}`.
fn summarize_claude_tool_uses(
    event: &Value,
    tool_names: &mut HashMap<String, String>,
) -> Vec<(String, ToolCount)> {
    let Some(content) = event.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
        .map(|block| {
            let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
            if let Some(id) = block.get("id").and_then(Value::as_str) {
                tool_names.insert(id.to_string(), name.to_string());
            }
            let empty = serde_json::Map::new();
            let input = block
                .get("input")
                .and_then(Value::as_object)
                .unwrap_or(&empty);
            let string_arg = |key: &str| input.get(key).and_then(Value::as_str);
            let line = match name {
                "Edit" | "Write" | "MultiEdit" | "NotebookEdit"
                    if string_arg("file_path").is_some() =>
                {
                    format!("TOOL START edit {}", string_arg("file_path").unwrap_or(""))
                }
                "Bash" if string_arg("command").is_some() => format!(
                    "TOOL START shell {}",
                    truncate(string_arg("command").unwrap_or(""), 160)
                ),
                "Read" if string_arg("file_path").is_some() => {
                    format!("TOOL START read {}", string_arg("file_path").unwrap_or(""))
                }
                "Grep" if string_arg("pattern").is_some() => format!(
                    "TOOL START grep {}",
                    truncate(string_arg("pattern").unwrap_or(""), 80)
                ),
                "Glob" if string_arg("pattern").is_some() => {
                    format!("TOOL START glob {}", string_arg("pattern").unwrap_or(""))
                }
                other => format!(
                    "TOOL START {} {}",
                    other.trim_start_matches("mcp__playwright__"),
                    truncate(&Value::Object(input.clone()).to_string(), 120)
                ),
            };
            (line, ToolCount::Started)
        })
        .collect()
}

/// Claude: `{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id"}]}}`.
fn summarize_claude_tool_results(
    event: &Value,
    tool_names: &mut HashMap<String, String>,
) -> Vec<(String, ToolCount)> {
    let Some(content) = event.pointer("/message/content").and_then(Value::as_array) else {
        return Vec::new();
    };
    content
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
        .map(|block| {
            let name = block
                .get("tool_use_id")
                .and_then(Value::as_str)
                .and_then(|id| tool_names.remove(id))
                .unwrap_or_else(|| "tool".to_string());
            let errored = block.get("is_error") == Some(&Value::Bool(true));
            let line = if errored {
                format!("TOOL DONE {name} error")
            } else {
                format!("TOOL DONE {name}")
            };
            (line, ToolCount::Completed)
        })
        .collect()
}

/// `modelSelection.ts:5-8`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ModelChoice {
    /// The model to pass.
    pub(crate) model: String,
    /// `first-attempt`, `repair` or `escalated`.
    pub(crate) reason: &'static str,
}

/// `modelSelection.ts:14-37`. First attempt uses the strong model; repairs use the cheaper one
/// unless the previous repair fixed nothing — then escalate rather than cheap-repeat a failure.
pub(crate) fn select_model(
    preset: AgentPreset,
    first_attempt_of_cycle: bool,
    previous_fixed_count: u64,
    has_repaired: bool,
) -> ModelChoice {
    let strong = env::model().unwrap_or_else(|| preset.default_model().to_string());
    let cheap = env::repair_model().unwrap_or_else(|| preset.repair_model().to_string());
    if first_attempt_of_cycle || env::no_model_switch() {
        return ModelChoice {
            model: strong,
            reason: "first-attempt",
        };
    }
    if has_repaired && previous_fixed_count == 0 {
        return ModelChoice {
            model: strong,
            reason: "escalated",
        };
    }
    ModelChoice {
        model: cheap,
        reason: "repair",
    }
}

/// `modelSelection.ts:40-59`. Set the value after `model_flag` when the flag is already in
/// the argv; a `generator:` block that omits it is left alone.
pub(crate) fn with_model(config: &CommandConfig, model_flag: &str, model: &str) -> CommandConfig {
    let mut args = config.args.clone().unwrap_or_default();
    let Some(index) = args.iter().position(|arg| arg == model_flag) else {
        return config.clone();
    };
    if index == args.len() - 1 {
        args.push(model.to_string());
    } else {
        args[index + 1] = model.to_string();
    }
    CommandConfig {
        args: Some(args),
        ..config.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_string()).collect()
    }

    #[test]
    fn placeholders_are_substituted_or_the_prompt_is_appended() {
        let ws = Path::new("/w");
        assert_eq!(
            build_args(
                &strings(&["-p", "{prompt}", "--cwd", "{workspace}"]),
                "hi",
                ws
            ),
            strings(&["-p", "hi", "--cwd", "/w"])
        );
        assert_eq!(
            build_args(&strings(&["-p", "--workspace", "{workspace}"]), "hi", ws),
            strings(&["-p", "--workspace", "/w", "hi"])
        );
    }

    #[test]
    fn the_prompt_is_redacted_wherever_it_sits() {
        let args = strings(&["-p", "secret key 0xabc", "--flag"]);
        assert_eq!(
            redact_prompt_args(&args, "secret key 0xabc"),
            strings(&["-p", "<prompt redacted>", "--flag"])
        );
    }

    #[test]
    fn claude_and_cursor_events_summarise_the_same_way() {
        let mut names = HashMap::new();
        let cases: Vec<(&str, Vec<(&str, ToolCount)>)> = vec![
            (
                r#"{"type":"system","subtype":"init","model":"opus","session_id":"s1"}"#,
                vec![("SESSION started model=opus", ToolCount::None)],
            ),
            (
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"yarn build"}},{"type":"tool_use","id":"t2","name":"Edit","input":{"file_path":"src/a.ts"}}]}}"#,
                vec![
                    ("TOOL START shell yarn build", ToolCount::Started),
                    ("TOOL START edit src/a.ts", ToolCount::Started),
                ],
            ),
            (
                r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"},{"type":"tool_result","tool_use_id":"t2","is_error":true}]}}"#,
                vec![
                    ("TOOL DONE Bash", ToolCount::Completed),
                    ("TOOL DONE Edit error", ToolCount::Completed),
                ],
            ),
            (
                r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t3","name":"mcp__playwright__browser_navigate","input":{"url":"http://x"}}]}}"#,
                vec![(
                    "TOOL START browser_navigate {\"url\":\"http://x\"}",
                    ToolCount::Started,
                )],
            ),
            (
                r#"{"type":"tool_call","subtype":"started","tool_call":{"shellToolCall":{"args":{"command":"ls"}}}}"#,
                vec![("TOOL START shell ls", ToolCount::Started)],
            ),
            (
                r#"{"type":"tool_call","subtype":"completed","tool_call":{"editToolCall":{"args":{"path":"a.ts"}}}}"#,
                vec![("TOOL DONE edit a.ts", ToolCount::Completed)],
            ),
            (
                r#"{"type":"tool_call","subtype":"started","tool_call":{"fooToolCall":{"args":{"k":1}}}}"#,
                vec![("TOOL START foo {\"k\":1}", ToolCount::Started)],
            ),
            (
                r#"{"type":"result","subtype":"success","duration_ms":1200}"#,
                vec![("RESULT success durationMs=1200", ToolCount::None)],
            ),
            (
                r#"{"type":"result","subtype":"idle_timeout","is_error":true}"#,
                vec![("RESULT idle_timeout error", ToolCount::None)],
            ),
            (
                r#"{"type":"thinking","subtype":"completed"}"#,
                vec![("THINKING completed", ToolCount::None)],
            ),
            (r#"{"type":"stream_event"}"#, vec![]),
        ];
        for (line, expected) in cases {
            let event: Value = serde_json::from_str(line).expect("json");
            let got: Vec<(String, ToolCount)> = summarize_stream_event(&event, &mut names);
            let expected: Vec<(String, ToolCount)> = expected
                .into_iter()
                .map(|(s, c)| (s.to_string(), c))
                .collect();
            assert_eq!(got, expected, "for {line}");
        }
    }

    #[test]
    fn model_selection_and_flag_rewrite_follow_upstream() {
        let preset = AgentPreset::Claude;
        assert_eq!(select_model(preset, true, 0, false).reason, "first-attempt");
        assert_eq!(select_model(preset, false, 2, true).reason, "repair");
        assert_eq!(select_model(preset, false, 0, true).reason, "escalated");
        assert_eq!(select_model(preset, false, 0, false).reason, "repair");
        let config = CommandConfig {
            command: "claude".into(),
            args: Some(strings(&["-p", "{prompt}", "--model", "opus"])),
            env: None,
            timeout_ms: None,
        };
        assert_eq!(
            with_model(&config, "--model", "sonnet").args,
            Some(strings(&["-p", "{prompt}", "--model", "sonnet"]))
        );
        let trailing = CommandConfig {
            args: Some(strings(&["--model"])),
            ..config.clone()
        };
        assert_eq!(
            with_model(&trailing, "--model", "sonnet").args,
            Some(strings(&["--model", "sonnet"]))
        );
        let without = CommandConfig {
            args: Some(strings(&["-p"])),
            ..config
        };
        assert_eq!(
            with_model(&without, "--model", "x").args,
            Some(strings(&["-p"]))
        );
    }

    fn provider(script: &str) -> Provider {
        Provider::new(
            CommandConfig {
                command: "sh".into(),
                args: Some(strings(&["-c", script])),
                env: None,
                timeout_ms: None,
            },
            AgentPreset::Claude,
        )
        .expect("provider")
    }

    #[tokio::test]
    async fn a_fake_agent_is_run_logged_and_summarised() {
        let dir = std::env::temp_dir().join(format!("hanvil-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let log = dir.join("raw.log");
        let activity = dir.join("activity.log");
        // The prompt is appended as $0; the script echoes a Claude-shaped stream.
        let script = r#"printf '%s\n' '{"type":"system","subtype":"init","model":"m","session_id":"abc"}' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"1","name":"Bash","input":{"command":"true"}}]}}' '{"type":"result","subtype":"success","result":"done"}'; echo "warn $0" >&2"#;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let result = provider(script)
            .run(RunInput {
                prompt: "the prompt",
                workspace: &dir,
                log_path: Some(&log),
                activity_log_path: Some(&activity),
                timeout: None,
                on_progress: Some(Arc::new(move |p: &Progress| {
                    if let Ok(mut seen) = sink.lock() {
                        seen.push(p.last_activity.clone());
                    }
                })),
            })
            .await
            .expect("runs");
        assert_eq!(result.exit_code, Some(0));
        assert!(!result.timed_out);
        assert!(result.stdout.contains("\"subtype\":\"success\""));
        assert!(result.stderr.contains("warn the prompt"));
        assert_eq!(result.args.last().map(String::as_str), Some("the prompt"));

        let raw = std::fs::read_to_string(&log).expect("raw log");
        assert!(raw.starts_with("# agent raw stream log\ncommand=sh\nargs=[\"-c\",\""));
        // The script is logged verbatim; only the argument carrying the prompt is redacted.
        assert!(raw.contains("args=[\"-c\",\"printf"), "{raw}");
        assert!(raw.contains("\",\"<prompt redacted>\"]\n"), "{raw}");
        assert!(!raw.contains("args=[\"-c\",\"printf") || !raw.contains("\"the prompt\"]"));
        assert!(raw.contains("## stderr\nwarn the prompt"));
        assert!(raw.contains("\n## harness\nexitCode=0\ntimedOut=false\n"));
        assert!(raw.contains("toolCallsStarted=1\ntoolCallsCompleted=0\nsessionId=abc\n"));

        let lines = std::fs::read_to_string(&activity).expect("activity");
        assert!(lines.contains(" SESSION started model=m\n"));
        assert!(lines.contains(" TOOL START shell true\n"));
        assert!(lines.contains(" RESULT success\n"));
        let seen = seen.lock().expect("seen").clone();
        assert_eq!(
            seen,
            [
                "SESSION started model=m",
                "TOOL START shell true",
                "RESULT success"
            ]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn an_agent_that_goes_silent_is_stopped_and_marked() {
        let started = Instant::now();
        let result = provider("echo started; sleep 20")
            .with_idle_timeout(Duration::from_millis(300))
            .run(RunInput {
                prompt: "p",
                workspace: &std::env::temp_dir(),
                log_path: None,
                activity_log_path: None,
                timeout: None,
                on_progress: None,
            })
            .await
            .expect("runs");
        assert!(result.timed_out);
        assert!(
            result
                .stderr
                .contains("Agent produced no output for 300ms; treating as failure.")
        );
        assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_wall_clock_timeout_stops_the_agent() {
        let result = provider("sleep 20")
            .run(RunInput {
                prompt: "p",
                workspace: &std::env::temp_dir(),
                log_path: None,
                activity_log_path: None,
                timeout: Some(Duration::from_millis(200)),
                on_progress: None,
            })
            .await
            .expect("runs");
        assert!(result.timed_out);
        assert!(!result.stderr.contains("produced no output"));
        assert_eq!(result.signal.as_deref(), Some("SIGTERM"));
    }
}
