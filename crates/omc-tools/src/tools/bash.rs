//! The `bash` tool: run a shell command with a timeout.

use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Timeout in seconds when the caller gives none.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Largest timeout in seconds a caller may ask for.
const MAX_TIMEOUT_SECS: u64 = 600;

/// Most bytes kept from each of stdout and stderr.
const MAX_STREAM_BYTES: usize = 30_000;

/// How long to wait for output pipes to close after the shell has exited.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// Runs shell commands in the working directory.
pub struct Bash {
    /// The directory commands run in.
    sandbox: Sandbox,
}

impl Bash {
    /// Creates the tool running commands in the root of `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of a `bash` call.
#[derive(Deserialize)]
struct Args {
    /// The command line to run with `bash -c`.
    command: String,
    /// Seconds before the command is killed.
    timeout: Option<u64>,
}

/// The head of one output stream plus the number of bytes it produced.
#[derive(Default)]
struct Capture {
    /// The first [`MAX_STREAM_BYTES`] bytes.
    data: Vec<u8>,
    /// Total bytes the stream produced.
    total: usize,
}

#[async_trait]
impl Tool for Bash {
    /// Returns `bash`.
    fn name(&self) -> &'static str {
        "bash"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Run a shell command with bash in the working directory. Returns the exit code, stdout \
         and stderr. The command is killed after the timeout, and long output is truncated. \
         The user is asked to approve each command."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command line to run." },
                "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in seconds. Defaults to 120, at most 600." }
            },
            "required": ["command"]
        })
    }

    /// Commands can do anything, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the command line.
    fn describe(&self, args: &Value) -> String {
        args["command"].as_str().unwrap_or("?").to_string()
    }

    /// Runs the command, killing its whole process group on timeout.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let seconds = args
            .timeout
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);
        run(self.sandbox.root(), &args.command, seconds).await
    }
}

/// Runs `command` in `dir`, waiting at most `seconds`.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the shell cannot start and
/// [`ToolError::Failed`], carrying the partial output, on timeout.
async fn run(dir: &std::path::Path, command: &str, seconds: u64) -> Result<ToolOutput, ToolError> {
    let mut child = Command::new("bash")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .process_group(0)
        .spawn()?;
    let pid = child.id();
    let stdout = Arc::new(Mutex::new(Capture::default()));
    let stderr = Arc::new(Mutex::new(Capture::default()));
    let readers = [
        tokio::spawn(capture(child.stdout.take(), stdout.clone())),
        tokio::spawn(capture(child.stderr.take(), stderr.clone())),
    ];
    let waited = tokio::time::timeout(Duration::from_secs(seconds), child.wait()).await;
    let status = match waited {
        Ok(status) => Some(status?),
        Err(_) => {
            if let Some(pid) = pid {
                kill_group(pid);
            }
            let _ = child.wait().await;
            None
        }
    };
    for reader in readers {
        let _ = tokio::time::timeout(DRAIN_GRACE, reader).await;
    }
    let report = format_output(status, &stdout, &stderr);
    match status {
        Some(_) => Ok(ToolOutput::new(report)),
        None => Err(ToolError::Failed(format!(
            "command timed out after {seconds}s and was killed\n{report}"
        ))),
    }
}

/// Reads `source` to the end, keeping the head of it in `into`.
async fn capture<R: AsyncRead + Unpin>(source: Option<R>, into: Arc<Mutex<Capture>>) {
    let Some(mut source) = source else { return };
    let mut chunk = [0u8; 8192];
    while let Ok(read) = source.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        let mut capture = into.lock().unwrap();
        let room = MAX_STREAM_BYTES.saturating_sub(capture.data.len());
        capture.data.extend_from_slice(&chunk[..read.min(room)]);
        capture.total += read;
    }
}

/// Sends SIGKILL to the process group led by `pid`.
fn kill_group(pid: u32) {
    let Ok(pgid) = i32::try_from(pid) else { return };
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Formats the exit status and the captured streams for the model.
fn format_output(
    status: Option<ExitStatus>,
    stdout: &Mutex<Capture>,
    stderr: &Mutex<Capture>,
) -> String {
    let mut text = match status {
        Some(status) => match status.code() {
            Some(code) => format!("exit code: {code}"),
            None => "terminated by a signal".to_string(),
        },
        None => "no exit code".to_string(),
    };
    for (label, capture) in [("stdout", stdout), ("stderr", stderr)] {
        let capture = capture.lock().unwrap();
        if capture.total == 0 {
            continue;
        }
        text.push_str(&format!(
            "\n--- {label} ---\n{}",
            String::from_utf8_lossy(&capture.data).trim_end()
        ));
        if capture.total > capture.data.len() {
            text.push_str(&format!(
                "\n[{label} truncated: showing {} of {} bytes]",
                capture.data.len(),
                capture.total
            ));
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::testutil::scratch;

    /// Builds a `bash` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Bash, std::path::PathBuf) {
        let dir = scratch(name);
        (Bash::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Stdout, stderr and the exit code are reported, in the working directory.
    #[tokio::test]
    async fn captures_output_and_exit_code() {
        let (tool, dir) = tool("bash-output");
        let out = tool
            .call(json!({ "command": "pwd; echo oops >&2; exit 3" }))
            .await
            .unwrap();
        assert!(out.text.starts_with("exit code: 3"));
        assert!(
            out.text
                .contains(&format!("--- stdout ---\n{}", dir.display()))
        );
        assert!(out.text.contains("--- stderr ---\noops"));
    }

    /// A command past its timeout is killed, including background children.
    #[tokio::test]
    async fn kills_the_process_group_on_timeout() {
        let (tool, _dir) = tool("bash-timeout");
        let started = Instant::now();
        let result = tool
            .call(json!({ "command": "echo started; sleep 30 & sleep 30", "timeout": 1 }))
            .await;
        assert!(started.elapsed() < Duration::from_secs(10));
        let Err(ToolError::Failed(message)) = result else {
            panic!("expected a timeout error");
        };
        assert!(message.contains("timed out after 1s"));
        assert!(message.contains("started"));
    }

    /// Output beyond the cap is cut and says how much was kept.
    #[tokio::test]
    async fn truncates_long_output() {
        let (tool, _dir) = tool("bash-truncate");
        let out = tool
            .call(json!({ "command": "head -c 100000 /dev/zero | tr '\\0' x" }))
            .await
            .unwrap();
        assert!(
            out.text
                .contains("stdout truncated: showing 30000 of 100000 bytes")
        );
        assert!(out.text.len() < 31_000);
    }

    /// A command that leaves a background process holding the pipes still returns.
    #[tokio::test]
    async fn returns_when_a_background_child_keeps_the_pipes_open() {
        let (tool, _dir) = tool("bash-background");
        let started = Instant::now();
        let out = tool
            .call(json!({ "command": "sleep 5 & echo done" }))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(out.text.contains("done"));
    }
}
