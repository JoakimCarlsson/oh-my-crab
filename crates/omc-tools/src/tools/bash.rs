//! The `bash` tool: run a shell command with a timeout.

use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::capture::Capture;
use crate::tool::parse_args;
use crate::{Jobs, Sandbox, Tool, ToolError, ToolOutput};

/// Timeout in seconds when the caller gives none.
const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Largest timeout in seconds a caller may ask for.
const MAX_TIMEOUT_SECS: u64 = 600;

/// How long to wait for output pipes to close after the shell has exited.
const DRAIN_GRACE: Duration = Duration::from_secs(1);

/// What the model is told about the tool.
const DESCRIPTION: &str = "Run a shell command in the working directory, with bash (cmd on Windows). Returns the exit code, stdout \
     and stderr. The command is killed after the timeout. Long output shows its start and end only, \
     and the result names a file holding the whole of it, to read in parts with `read` instead of running the command again. \
     The user is asked to approve each command.";

/// What the model is told about the `background` flag, when the tool has it.
const BACKGROUND_NOTE: &str = " Pass `background: true` to start a long-running command (a dev server, a long build) and get a handle \
     back at once; it has no timeout unless you give one, and `wait` returns its exit code and output, long output cut the same way, when it ends.";

/// Runs shell commands in the working directory.
pub struct Bash {
    /// The directory commands run in.
    sandbox: Sandbox,
    /// Where background commands run, when the tool may start them.
    jobs: Option<Arc<Jobs>>,
    /// What the model is told it does.
    description: String,
}

impl Bash {
    /// Creates the tool running commands in the root of `sandbox`, in the foreground only.
    pub fn new(sandbox: Sandbox) -> Self {
        Self {
            sandbox,
            jobs: None,
            description: DESCRIPTION.to_owned(),
        }
    }

    /// Creates the tool running commands in the root of `sandbox`, which can also start them as background runs in `jobs`.
    pub fn with_jobs(sandbox: Sandbox, jobs: Arc<Jobs>) -> Self {
        Self {
            sandbox,
            jobs: Some(jobs),
            description: format!("{DESCRIPTION}{BACKGROUND_NOTE}"),
        }
    }
}

/// Arguments of a `bash` call.
#[derive(Deserialize)]
struct Args {
    /// The command line to run with `bash -c`.
    command: String,
    /// Seconds before the command is killed.
    timeout: Option<u64>,
    /// Whether to start the command in the background and return a handle.
    #[serde(default)]
    background: bool,
}

#[async_trait]
impl Tool for Bash {
    /// Returns `bash`.
    fn name(&self) -> &'static str {
        "bash"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &str {
        &self.description
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "The command line to run." },
                "timeout": { "type": "integer", "minimum": 1, "description": "Timeout in seconds. Defaults to 120, at most 600; a background command has none unless given." }
            },
            "required": ["command"]
        });
        if self.jobs.is_some() {
            schema["properties"]["background"] = json!({
                "type": "boolean",
                "description": "Start the command in the background and return a handle at once; `wait` returns its result."
            });
        }
        schema
    }

    /// Commands can do anything, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the command line.
    fn describe(&self, args: &Value) -> String {
        args["command"].as_str().unwrap_or("?").to_string()
    }

    /// A copy that runs commands in the foreground only, when this one can start background runs.
    fn foreground(&self) -> Option<Arc<dyn Tool>> {
        self.jobs
            .as_ref()
            .map(|_| Arc::new(Bash::new(self.sandbox.clone())) as Arc<dyn Tool>)
    }

    /// Runs the call without an id, which only loses the link from a background run to the call that started it.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Runs the command, killing its whole process group on timeout, or starts it in the background under the call `id`.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if !args.background {
            let seconds = args
                .timeout
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS);
            return run(
                &self.sandbox,
                &args.command,
                Some(Duration::from_secs(seconds)),
            )
            .await;
        }
        let jobs = self.jobs.as_ref().ok_or_else(|| {
            ToolError::Failed("this agent cannot start background runs".to_owned())
        })?;
        let timeout = args
            .timeout
            .map(|seconds| Duration::from_secs(seconds.clamp(1, MAX_TIMEOUT_SECS)));
        let sandbox = self.sandbox.clone();
        let command = args.command;
        let label: String = command
            .lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect();
        let work = {
            let command = command.clone();
            async move {
                run(&sandbox, &command, timeout)
                    .await
                    .map(|output| output.text)
                    .map_err(|error| error.to_string())
            }
        };
        let handle = jobs.spawn(label, Some(id.to_owned()).filter(|id| !id.is_empty()), work);
        Ok(ToolOutput::new(format!(
            "Started in the background as {handle}. Call `wait` to get its result when it ends."
        )))
    }
}

/// Kills the process group of a command when dropped, so that a command whose run is cancelled does not outlive it.
struct GroupGuard(Option<u32>);

impl GroupGuard {
    /// Stops the guard from killing anything, once the command has been waited for.
    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for GroupGuard {
    /// Kills the group when the guard is still armed.
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            kill_group(pid);
        }
    }
}

/// Runs `command` in the root of `sandbox`, waiting at most `limit` when there is one.
///
/// An output stream too long to show is saved whole in the sandbox's spill folder, and only its start and end are reported, with the file's path. The command is killed, with its process group, when the returned future is dropped.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the shell cannot start and
/// [`ToolError::Failed`], carrying the partial output, on timeout.
async fn run(
    sandbox: &Sandbox,
    command: &str,
    limit: Option<Duration>,
) -> Result<ToolOutput, ToolError> {
    let mut child = shell(command)
        .current_dir(sandbox.root())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let pid = child.id();
    let mut guard = GroupGuard(pid);
    let fresh = |label| Arc::new(Mutex::new(Capture::new(label, sandbox.spill().clone())));
    let stdout = fresh("stdout");
    let stderr = fresh("stderr");
    let readers = [
        tokio::spawn(capture(child.stdout.take(), stdout.clone())),
        tokio::spawn(capture(child.stderr.take(), stderr.clone())),
    ];
    let waited = match limit {
        Some(limit) => tokio::time::timeout(limit, child.wait()).await.ok(),
        None => Some(child.wait().await),
    };
    let status = match waited {
        Some(status) => Some(status?),
        None => {
            if let Some(pid) = pid {
                kill_group(pid);
            }
            let _ = child.wait().await;
            None
        }
    };
    guard.disarm();
    for reader in readers {
        let _ = tokio::time::timeout(DRAIN_GRACE, reader).await;
    }
    let report = format_output(status, &stdout, &stderr);
    match status {
        Some(_) => Ok(ToolOutput::new(report)),
        None => Err(ToolError::Failed(format!(
            "command timed out after {}s and was killed\n{report}",
            limit.unwrap_or_default().as_secs()
        ))),
    }
}

/// Reads `source` to the end into `into`.
async fn capture<R: AsyncRead + Unpin>(source: Option<R>, into: Arc<Mutex<Capture>>) {
    let Some(mut source) = source else { return };
    let mut chunk = [0u8; 8192];
    while let Ok(read) = source.read(&mut chunk).await {
        if read == 0 {
            break;
        }
        into.lock().unwrap().push(&chunk[..read]);
    }
}

/// Builds the command that runs `command` in a shell: `bash -c` in a process group of its own, so the whole group can be killed.
#[cfg(unix)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("bash");
    shell.arg("-c").arg(command).process_group(0);
    shell
}

/// Builds the command that runs `command` in a shell: `cmd /C`.
#[cfg(windows)]
fn shell(command: &str) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

/// Sends SIGKILL to the process group led by `pid`.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let Ok(pgid) = i32::try_from(pid) else { return };
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Kills the process `pid` and every process it started, with `taskkill`.
#[cfg(windows)]
fn kill_group(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .status();
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
    for capture in [stdout, stderr] {
        if let Some(stream) = capture.lock().unwrap().render() {
            text.push('\n');
            text.push_str(&stream);
        }
    }
    text
}

#[cfg(all(test, unix))]
mod tests {
    use std::time::Instant;

    use super::*;
    use crate::testutil::scratch;

    /// A sandbox rooted at the scratch directory `name`, saving long output in a scratch directory of its own outside the root.
    fn sandbox(name: &str) -> (Sandbox, std::path::PathBuf) {
        let dir = scratch(name);
        let spill = crate::Spill::new(scratch(&format!("{name}-output")));
        (Sandbox::new(&dir).unwrap().with_spill(spill), dir)
    }

    /// Builds a `bash` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Bash, std::path::PathBuf) {
        let (sandbox, dir) = sandbox(name);
        (Bash::new(sandbox), dir)
    }

    /// The path of the file the result `text` says holds the whole output.
    fn saved_path(text: &str) -> String {
        let (_, rest) = text.split_once("the whole output is in ").unwrap();
        rest.split_once(';').unwrap().0.to_owned()
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

    /// Output beyond the cap keeps its start and end, says how long it was, and is saved whole where `read` can open it.
    #[tokio::test]
    async fn truncates_long_output_and_saves_it_whole() {
        let (sandbox, _dir) = sandbox("bash-truncate");
        let tool = Bash::new(sandbox.clone());
        let out = tool
            .call(json!({ "command": "seq 1 50000; echo failed >&2" }))
            .await
            .unwrap();
        assert!(out.text.contains("--- stdout ---\n1\n2\n3\n"));
        assert!(
            out.text
                .contains("49999\n50000\n[stdout truncated: 288894 bytes in all")
        );
        assert!(out.text.contains("--- stderr ---\nfailed"));
        assert!(out.text.len() < 31_000);
        let path = saved_path(&out.text);
        let whole = std::fs::read_to_string(&path).unwrap();
        assert_eq!(whole.lines().count(), 50_000);
        let read = crate::Read::new(sandbox)
            .call(json!({ "path": format!("{path}:-2") }))
            .await
            .unwrap();
        assert!(read.text.contains("50000"));
    }

    /// A background command returns a handle at once, `wait` reports its exit code and output, and a failing command is reported as failed.
    #[tokio::test]
    async fn background_commands_report_through_wait() {
        let dir = scratch("bash-bg");
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Bash::with_jobs(Sandbox::new(&dir).unwrap(), jobs.clone());
        let started = Instant::now();
        let out = tool
            .call(json!({ "command": "sleep 1; echo hello; exit 2", "background": true }))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(500));
        assert!(out.text.contains("bg-1"));
        let finished = jobs.wait().await.unwrap();
        assert!(finished.ok);
        assert!(finished.text.starts_with("exit code: 2"));
        assert!(finished.text.contains("hello"));
        tool.call(json!({ "command": "sleep 5", "background": true, "timeout": 1 }))
            .await
            .unwrap();
        let finished = jobs.wait().await.unwrap();
        assert!(!finished.ok);
        assert!(finished.text.contains("timed out after 1s"));
    }

    /// A background command cuts long output the same way, keeping its end and saving the whole of it.
    #[tokio::test]
    async fn background_output_keeps_the_tail_and_saves_it_whole() {
        let (sandbox, _dir) = sandbox("bash-bg-tail");
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Bash::with_jobs(sandbox, jobs.clone());
        tool.call(json!({
            "command": "head -c 100000 /dev/zero | tr '\\0' x; echo END",
            "background": true
        }))
        .await
        .unwrap();
        let text = jobs.wait().await.unwrap().text;
        assert!(text.contains(
            "stdout truncated: 100004 bytes in all, showing the first 4000 and the last 26000"
        ));
        assert!(text.contains("xxxEND"));
        let whole = std::fs::read_to_string(saved_path(&text)).unwrap();
        assert_eq!(whole.len(), 100_004);
    }

    /// The foreground copy of the tool refuses background runs and does not offer them.
    #[tokio::test]
    async fn the_foreground_copy_cannot_background() {
        let dir = scratch("bash-fg");
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Bash::with_jobs(Sandbox::new(&dir).unwrap(), jobs);
        assert!(tool.schema()["properties"]["background"].is_object());
        let plain = tool.foreground().unwrap();
        assert!(plain.schema()["properties"]["background"].is_null());
        let result = plain
            .call(json!({ "command": "true", "background": true }))
            .await;
        assert!(matches!(result, Err(ToolError::Failed(_))));
    }

    /// Dropping the runs kills a background command along with the processes it started.
    #[tokio::test]
    async fn dropping_the_runs_kills_the_command_group() {
        let dir = scratch("bash-bg-kill");
        let marker = dir.join("alive");
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Bash::with_jobs(Sandbox::new(&dir).unwrap(), jobs.clone());
        tool.call(json!({
            "command": "(sleep 2; touch alive) & sleep 30",
            "background": true
        }))
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        jobs.stop_all();
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists());
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
