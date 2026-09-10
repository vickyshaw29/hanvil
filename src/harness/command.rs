//! Subprocesses the harness runs and reads. `command.ts` of hedera-harness dev @ 587a2f3.
//!
//! Every child leads its own process group so a timeout can tear down grandchildren — a
//! validator runs through `sh`, and signalling only `sh` would leave yarn or next running.
//! Signals reach the group through `pkill -g`; `libc::kill(-pgid)` would need `unsafe`.

use std::collections::{BTreeMap, VecDeque};
use std::ffi::OsStr;
use std::path::Path;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;

/// `command.ts:18`.
pub(crate) const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// `command.ts:20`. Between SIGTERM and SIGKILL when a child overruns.
pub(crate) const KILL_GRACE: Duration = Duration::from_secs(5);
/// `command.ts:22-23`. Retained bytes per stream: head keeps the start, tail keeps what failed.
const CAPTURE_HEAD_BYTES: usize = 256 * 1024;
const CAPTURE_TAIL_BYTES: usize = 768 * 1024;

/// `command.ts:33-82`. Bounded capture of a child stream: the head keeps the startup context and
/// the tail keeps whatever failed (or, for agents, the final verdict) while the middle is
/// dropped, so a long run cannot exhaust memory.
#[derive(Debug)]
pub(crate) struct BoundedOutput {
    head: Vec<u8>,
    head_limit: usize,
    tail: VecDeque<Vec<u8>>,
    tail_bytes: usize,
    tail_limit: usize,
    dropped: usize,
}

impl BoundedOutput {
    /// The command-sized capture.
    pub(crate) fn new() -> Self {
        Self::with_limits(CAPTURE_HEAD_BYTES, CAPTURE_TAIL_BYTES)
    }

    /// A capture with explicit head and tail budgets.
    pub(crate) fn with_limits(head_limit: usize, tail_limit: usize) -> Self {
        Self {
            head: Vec::new(),
            head_limit,
            tail: VecDeque::new(),
            tail_bytes: 0,
            tail_limit,
            dropped: 0,
        }
    }

    /// Append a chunk, dropping whole earlier tail chunks once the tail is over budget.
    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let mut rest = chunk;
        if self.head.len() < self.head_limit {
            let room = self.head_limit - self.head.len();
            if rest.len() <= room {
                self.head.extend_from_slice(rest);
                return;
            }
            self.head.extend_from_slice(&rest[..room]);
            rest = &rest[room..];
        }
        self.tail.push_back(rest.to_vec());
        self.tail_bytes += rest.len();
        while self.tail.len() > 1 && self.tail_bytes > self.tail_limit {
            if let Some(dropped) = self.tail.pop_front() {
                self.tail_bytes -= dropped.len();
                self.dropped += dropped.len();
            }
        }
    }

    /// Bytes that were dropped from the middle.
    pub(crate) fn truncated_bytes(&self) -> usize {
        self.dropped
    }

    /// Everything kept, with a marker where the middle went.
    pub(crate) fn render(&self) -> String {
        let head = String::from_utf8_lossy(&self.head);
        let tail: Vec<u8> = self.tail.iter().flatten().copied().collect();
        let tail = String::from_utf8_lossy(&tail);
        if self.dropped == 0 {
            format!("{head}{tail}")
        } else {
            format!(
                "{head}\n...[hanvil] omitted {} bytes of output...\n{tail}",
                self.dropped
            )
        }
    }
}

/// `command.ts:4-16`.
pub(crate) struct Execute<'a> {
    /// Binary, or a shell line when `shell` is set.
    pub(crate) command: &'a str,
    /// Arguments.
    pub(crate) args: &'a [String],
    /// Working directory.
    pub(crate) cwd: &'a Path,
    /// Added on top of the harness's own environment.
    pub(crate) env: &'a BTreeMap<String, String>,
    /// Wall-clock limit; `DEFAULT_TIMEOUT` when absent.
    pub(crate) timeout: Option<Duration>,
    /// Run through `sh -c` with the arguments joined, as Node's `shell: true` does.
    pub(crate) shell: bool,
    /// Tee the child's output to the terminal while still capturing it.
    pub(crate) stream_output: bool,
}

/// `types.ts` `CommandExecutionResult`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Execution {
    /// As requested.
    pub(crate) command: String,
    /// As requested.
    pub(crate) args: Vec<String>,
    /// `None` when killed by a signal.
    pub(crate) exit_code: Option<i32>,
    /// Bounded capture.
    pub(crate) stdout: String,
    /// Bounded capture.
    pub(crate) stderr: String,
    /// Wall time.
    pub(crate) duration_ms: u64,
    /// The harness stopped it.
    pub(crate) timed_out: bool,
    /// Signal name when killed, e.g. `SIGTERM`.
    pub(crate) signal: Option<String>,
}

impl Execution {
    /// `command.ts:199-205`.
    pub(crate) fn describe_failure(&self) -> String {
        let rendered = std::iter::once(self.command.as_str())
            .chain(self.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let reason = if self.timed_out {
            "timed out".to_string()
        } else {
            format!(
                "exited with code {}",
                self.exit_code
                    .map_or_else(|| "null".to_string(), |c| c.to_string())
            )
        };
        let stderr = self.stderr.trim();
        if stderr.is_empty() {
            format!("Command \"{rendered}\" {reason}.")
        } else {
            format!("Command \"{rendered}\" {reason}: {stderr}")
        }
    }
}

/// `command.ts:110-187`. Spawn, capture both pipes, stop the whole group on timeout.
pub(crate) async fn execute(options: Execute<'_>) -> std::io::Result<Execution> {
    let started = Instant::now();
    let timeout = options.timeout.unwrap_or(DEFAULT_TIMEOUT);
    let mut command = if options.shell {
        let line = std::iter::once(options.command)
            .chain(options.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let mut sh = tokio::process::Command::new("sh");
        sh.arg("-c").arg(line);
        sh
    } else {
        let mut direct = tokio::process::Command::new(options.command);
        direct.args(options.args);
        direct
    };
    command
        .current_dir(options.cwd)
        .envs(options.env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let pgid = child.id();

    let stdout = Arc::new(Mutex::new(BoundedOutput::new()));
    let stderr = Arc::new(Mutex::new(BoundedOutput::new()));
    let readers = (
        child.stdout.take().map(|pipe| {
            drain(pipe, Arc::clone(&stdout), move |chunk| {
                if options.stream_output {
                    print!("{}", String::from_utf8_lossy(chunk));
                }
            })
        }),
        child.stderr.take().map(|pipe| {
            drain(pipe, Arc::clone(&stderr), move |chunk| {
                if options.stream_output {
                    eprint!("{}", String::from_utf8_lossy(chunk));
                }
            })
        }),
    );

    let mut timed_out = false;
    let status = tokio::select! {
        status = child.wait() => status?,
        () = tokio::time::sleep(timeout) => {
            timed_out = true;
            stop_group(&mut child, pgid).await?
        }
    };
    if let Some(reader) = readers.0 {
        let _ = reader.await;
    }
    if let Some(reader) = readers.1 {
        let _ = reader.await;
    }

    Ok(Execution {
        command: options.command.to_string(),
        args: options.args.to_vec(),
        exit_code: status.code(),
        stdout: lock_render(&stdout),
        stderr: lock_render(&stderr),
        duration_ms: started.elapsed().as_millis() as u64,
        timed_out,
        signal: signal_name(&status),
    })
}

/// Read a pipe to EOF into a capture, handing each chunk to `observe` as it arrives. Both
/// pipes of a child are drained concurrently from the first byte; a full pipe blocks the child.
pub(crate) fn drain<R>(
    mut pipe: R,
    capture: Arc<Mutex<BoundedOutput>>,
    mut observe: impl FnMut(&[u8]) + Send + 'static,
) -> tokio::task::JoinHandle<()>
where
    R: AsyncReadExt + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = vec![0u8; 8 * 1024];
        loop {
            match pipe.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buffer[..n];
                    if let Ok(mut capture) = capture.lock() {
                        capture.push(chunk);
                    }
                    observe(chunk);
                }
            }
        }
    })
}

/// Render a capture, tolerating a poisoned lock from a panicked reader.
pub(crate) fn lock_render(capture: &Mutex<BoundedOutput>) -> String {
    match capture.lock() {
        Ok(guard) => guard.render(),
        Err(poisoned) => poisoned.into_inner().render(),
    }
}

/// `command.ts:91-108` `killProcessTree`, twice: SIGTERM, then SIGKILL after `KILL_GRACE` for a
/// child that ignores the first. Returns the exit status.
pub(crate) async fn stop_group(
    child: &mut tokio::process::Child,
    pgid: Option<u32>,
) -> std::io::Result<std::process::ExitStatus> {
    signal_group(child, pgid, "TERM").await;
    match tokio::time::timeout(KILL_GRACE, child.wait()).await {
        Ok(status) => status,
        Err(_) => {
            signal_group(child, pgid, "KILL").await;
            child.wait().await
        }
    }
}

/// `pkill -<signal> -g <pgid>`, falling back to the direct child when the group is unknown.
pub(crate) async fn signal_group(
    child: &mut tokio::process::Child,
    pgid: Option<u32>,
    signal: &str,
) {
    if let Some(pgid) = pgid {
        let _ = tokio::process::Command::new("pkill")
            .arg(format!("-{signal}"))
            .arg("-g")
            .arg(pgid.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    if signal == "KILL" {
        let _ = child.start_kill();
    }
}

/// `SIGTERM`, `SIGKILL`, … for a status ended by a signal.
pub(crate) fn signal_name(status: &std::process::ExitStatus) -> Option<String> {
    use std::os::unix::process::ExitStatusExt as _;
    status.signal().map(|number| match number {
        1 => "SIGHUP".to_string(),
        2 => "SIGINT".to_string(),
        9 => "SIGKILL".to_string(),
        15 => "SIGTERM".to_string(),
        other => format!("SIG{other}"),
    })
}

/// What a finished command left behind, for the callers that only want a yes or a line.
#[derive(Debug, Clone)]
pub(crate) struct Captured {
    /// Exit status was zero.
    pub(crate) ok: bool,
    /// Everything written to stdout, lossily decoded.
    pub(crate) stdout: String,
}

/// Run to completion and capture stdout. Stderr is discarded; the callers here only want to
/// know whether a tool answers.
pub(crate) async fn capture(
    program: impl AsRef<OsStr>,
    args: &[&str],
    cwd: &Path,
) -> std::io::Result<Captured> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await?;
    Ok(Captured {
        ok: output.status.success(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
    })
}

/// `harnessGit.ts` `commandExists`: `sh -c 'command -v <name>'`.
pub(crate) async fn exists(command: &str, cwd: &Path) -> bool {
    let quoted = shell_quote(command);
    capture("sh", &["-c", &format!("command -v {quoted}")], cwd)
        .await
        .is_ok_and(|captured| captured.ok)
}

/// Single-quote for `sh`, the way `harnessGit.ts` `shellQuote` does.
pub(crate) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_survives_an_embedded_quote() {
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("plain"), "'plain'");
    }

    #[test]
    fn bounded_output_keeps_the_head_and_the_tail() {
        let mut output = BoundedOutput::with_limits(4, 6);
        output.push(b"abcdef"); // head takes abcd, tail ef
        output.push(b"ghij"); // tail efghij (6)
        output.push(b"klm"); // 9 > 6: drop "ef", still 7 > 6: drop "ghij"; one chunk always stays
        assert_eq!(output.truncated_bytes(), 6);
        assert_eq!(
            output.render(),
            "abcd\n...[hanvil] omitted 6 bytes of output...\nklm"
        );
        let mut small = BoundedOutput::new();
        small.push(b"hello");
        assert_eq!(small.render(), "hello");
        assert_eq!(small.truncated_bytes(), 0);
    }

    #[test]
    fn a_failed_command_is_described_like_upstream() {
        let mut execution = Execution {
            command: "yarn".into(),
            args: vec!["lint".into()],
            exit_code: Some(2),
            stdout: String::new(),
            stderr: " boom \n".into(),
            duration_ms: 1,
            timed_out: false,
            signal: None,
        };
        assert_eq!(
            execution.describe_failure(),
            "Command \"yarn lint\" exited with code 2: boom"
        );
        execution.stderr.clear();
        execution.timed_out = true;
        assert_eq!(
            execution.describe_failure(),
            "Command \"yarn lint\" timed out."
        );
    }

    #[tokio::test]
    async fn exists_answers_for_sh_and_not_for_nonsense() {
        let cwd = std::env::temp_dir();
        assert!(exists("sh", &cwd).await);
        assert!(!exists("hanvil-no-such-binary-7f3a", &cwd).await);
    }

    #[tokio::test]
    async fn execute_captures_both_pipes_through_the_shell() {
        let env = BTreeMap::from([("HANVIL_T".to_string(), "v".to_string())]);
        let execution = execute(Execute {
            command: "printf out; printf err >&2; printf $HANVIL_T; exit 3",
            args: &[],
            cwd: &std::env::temp_dir(),
            env: &env,
            timeout: None,
            shell: true,
            stream_output: false,
        })
        .await
        .expect("spawns");
        assert_eq!(execution.exit_code, Some(3));
        assert_eq!(execution.stdout, "outv");
        assert_eq!(execution.stderr, "err");
        assert!(!execution.timed_out);
        assert_eq!(execution.signal, None);
    }

    #[tokio::test]
    async fn execute_stops_the_whole_group_on_timeout() {
        let started = Instant::now();
        let execution = execute(Execute {
            command: "sh -c 'sleep 30' & sleep 30",
            args: &[],
            cwd: &std::env::temp_dir(),
            env: &BTreeMap::new(),
            timeout: Some(Duration::from_millis(200)),
            shell: true,
            stream_output: false,
        })
        .await
        .expect("spawns");
        assert!(execution.timed_out);
        assert_eq!(execution.signal.as_deref(), Some("SIGTERM"));
        // The grandchild held the pipes; the group kill closes them well inside the grace.
        assert!(started.elapsed() < KILL_GRACE + Duration::from_secs(2));
    }
}
