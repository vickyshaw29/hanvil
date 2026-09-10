//! Subprocesses the harness runs and reads. `command.ts` of hedera-harness; the streaming and
//! timeout half arrives with the attempt loop.

use std::ffi::OsStr;
use std::path::Path;

/// What a finished command left behind.
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
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
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

    #[tokio::test]
    async fn exists_answers_for_sh_and_not_for_nonsense() {
        let cwd = std::env::temp_dir();
        assert!(exists("sh", &cwd).await);
        assert!(!exists("hanvil-no-such-binary-7f3a", &cwd).await);
    }
}
