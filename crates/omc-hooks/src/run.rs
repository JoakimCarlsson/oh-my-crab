//! Running one hook command: a shell command line that reads a JSON payload on standard input and answers on standard output and with its exit code.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

/// The most of each output stream of a hook that is kept; the rest is read and dropped.
const KEPT: usize = 256 * 1024;

/// How long the output of a hook that ended is still read, in case something it started holds the streams open.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// How a hook command ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// It exited with `code`, having written `stdout` and `stderr`.
    Exited {
        /// The exit code; `-1` when a signal ended it.
        code: i32,
        /// What it wrote to standard output.
        stdout: String,
        /// What it wrote to standard error.
        stderr: String,
    },
    /// It ran longer than it may and was killed, with everything it started.
    TimedOut(Duration),
    /// It could not be started, for this reason.
    Unstarted(String),
}

impl Outcome {
    /// Why the hook gave no answer, for the reader: it timed out or could not be started; nothing when it exited.
    pub fn silence(&self) -> Option<String> {
        match self {
            Self::Exited { .. } => None,
            Self::TimedOut(limit) => Some(format!(
                "it ran longer than its {}s timeout and was stopped",
                limit.as_secs_f64()
            )),
            Self::Unstarted(reason) => Some(format!("it could not be started: {reason}")),
        }
    }
}

/// Runs `command` in a shell in `folder`, with `env` added to the environment, writing `input` to its standard input, and waits at most `limit` for it to end.
///
/// The command runs in a process group of its own, which is killed as a whole when it runs too long or when the returned future is dropped, so a hook never outlives its turn.
pub async fn run(
    command: &str,
    input: &[u8],
    folder: &Path,
    env: &[(String, String)],
    limit: Duration,
) -> Outcome {
    let mut shell = shell(command);
    shell
        .current_dir(folder)
        .envs(env.iter().map(|(key, value)| (key, value)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = match shell.spawn() {
        Ok(child) => child,
        Err(error) => return Outcome::Unstarted(error.to_string()),
    };
    let mut guard = GroupGuard(child.id());
    let stdin = child.stdin.take();
    let input = input.to_vec();
    let feeding = tokio::spawn(async move {
        if let Some(mut stdin) = stdin {
            let _ = stdin.write_all(&input).await;
            let _ = stdin.shutdown().await;
        }
    });
    let readers = (
        tokio::spawn(read_kept(child.stdout.take())),
        tokio::spawn(read_kept(child.stderr.take())),
    );
    let waited = tokio::time::timeout(limit, child.wait()).await;
    feeding.abort();
    let status = match waited {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            guard.kill();
            return Outcome::Unstarted(error.to_string());
        }
        Err(_) => {
            guard.kill();
            let _ = child.wait().await;
            readers.0.abort();
            readers.1.abort();
            return Outcome::TimedOut(limit);
        }
    };
    guard.disarm();
    let stdout = drained(readers.0).await;
    let stderr = drained(readers.1).await;
    Outcome::Exited {
        code: status.code().unwrap_or(-1),
        stdout,
        stderr,
    }
}

/// What the reader `reading` collected, waiting at most [`DRAIN_GRACE`] for it; empty when it did not finish in time.
async fn drained(reading: tokio::task::JoinHandle<String>) -> String {
    match tokio::time::timeout(DRAIN_GRACE, reading).await {
        Ok(Ok(text)) => text,
        _ => String::new(),
    }
}

/// Reads `source` to its end, keeping the first [`KEPT`] bytes as text.
async fn read_kept<R: AsyncRead + Unpin>(source: Option<R>) -> String {
    let Some(mut source) = source else {
        return String::new();
    };
    let mut kept = Vec::new();
    let mut chunk = [0u8; 8192];
    while let Ok(read) = source.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let room = KEPT.saturating_sub(kept.len());
        kept.extend_from_slice(&chunk[..read.min(room)]);
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// Kills the process group of a hook when dropped while still armed.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    /// Kills the group now, and stops the guard from doing so again.
    fn kill(&mut self) {
        if let Some(pid) = self.0.take() {
            kill_group(pid);
        }
    }

    /// Stops the guard from killing anything, once the hook has ended.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    /// Kills the group when the guard is still armed, as when the turn running the hook is stopped.
    fn drop(&mut self) {
        self.kill();
    }
}

/// The command that runs `command` with `bash -c`, in a process group of its own so the whole group can be killed.
#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("bash");
    shell.arg("-c").arg(command).process_group(0);
    shell
}

/// The command that runs `command` with `cmd /C`.
#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

/// Sends SIGKILL to the process group led by `pid`.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let Ok(group) = i32::try_from(pid) else {
        return;
    };
    unsafe {
        libc::kill(-group, libc::SIGKILL);
    }
}

/// Kills the process `pid` and every process it started, with `taskkill`.
#[cfg(windows)]
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();
}

#[cfg(all(test, unix))]
/// Tests for running hook commands.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// A hook reads its input, runs in the folder given with the environment given, and its output and exit code come back.
    #[tokio::test]
    async fn runs_with_input_folder_and_environment() {
        let folder = scratch("run-plain");
        let outcome = run(
            "read line; echo \"$line in $(basename \"$PWD\") as $WHO\"; echo warn >&2; exit 3",
            b"hello\n",
            &folder,
            &[("WHO".to_owned(), "crab".to_owned())],
            Duration::from_secs(10),
        )
        .await;
        assert_eq!(
            outcome,
            Outcome::Exited {
                code: 3,
                stdout: "hello in run-plain as crab\n".to_owned(),
                stderr: "warn\n".to_owned(),
            }
        );
    }

    /// A hook that runs too long is stopped, with what it started, in about its timeout, even when a child holds its output open.
    #[tokio::test]
    async fn a_slow_hook_is_stopped() {
        let folder = scratch("run-slow");
        let started = std::time::Instant::now();
        let outcome = run(
            "sleep 30 & sleep 30",
            b"",
            &folder,
            &[],
            Duration::from_millis(300),
        )
        .await;
        assert_eq!(outcome, Outcome::TimedOut(Duration::from_millis(300)));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(outcome.silence().unwrap().contains("timeout"));
    }

    /// A hook that never reads a large input still ends normally.
    #[tokio::test]
    async fn an_unread_input_does_not_block() {
        let folder = scratch("run-unread");
        let outcome = run(
            "exit 0",
            &vec![b'x'; 1 << 20],
            &folder,
            &[],
            Duration::from_secs(10),
        )
        .await;
        assert!(matches!(outcome, Outcome::Exited { code: 0, .. }));
    }

    /// A hook in a folder that does not exist cannot start.
    #[tokio::test]
    async fn a_missing_folder_is_reported() {
        let outcome = run(
            "true",
            b"",
            Path::new("/nonexistent/folder/for/hooks"),
            &[],
            Duration::from_secs(10),
        )
        .await;
        assert!(matches!(outcome, Outcome::Unstarted(_)));
    }
}
