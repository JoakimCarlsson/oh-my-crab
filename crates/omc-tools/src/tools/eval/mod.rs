//! The `eval` tool: run Python or JavaScript in a kernel that keeps its state between calls.
//!
//! Each language has one kernel, a long-lived interpreter process started on the first call and kept for the life of the tool. A small driver script inside it reads one request per line, runs the code in a namespace it keeps, and answers with a line that starts with a marker only it knows. Lines without the marker, written by programs the code starts, are kept as output. A call that runs past its timeout kills the kernel, losing its state; the next call starts a fresh one.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Timeout in seconds when the caller gives none.
const DEFAULT_TIMEOUT_SECS: u64 = 60;

/// Largest timeout in seconds a caller may ask for.
const MAX_TIMEOUT_SECS: u64 = 600;

/// Most bytes of output handed back; longer output keeps its start and end.
const OUTPUT_LIMIT: usize = 50_000;

/// The driver run by the Python kernel.
const PYTHON_DRIVER: &str = include_str!("kernel.py");

/// The driver run by the JavaScript kernel.
const NODE_DRIVER: &str = include_str!("kernel.js");

/// The environment variable the drivers read their answer marker from.
const MARKER_ENV: &str = "OMC_EVAL_MARKER";

/// The environment variable naming the Python interpreter, `python3` when unset.
const PYTHON_ENV: &str = "OMC_EVAL_PYTHON";

/// The environment variable naming the Node.js interpreter, `node` when unset.
const NODE_ENV: &str = "OMC_EVAL_NODE";

/// What the model is told about the tool.
const DESCRIPTION: &str = include_str!("../../prompts/eval.md");

/// A language the tool runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Language {
    /// Python 3.
    Python,
    /// JavaScript under Node.js.
    #[serde(alias = "js", alias = "node")]
    Javascript,
}

impl Language {
    /// The name the model calls the language by.
    fn name(self) -> &'static str {
        match self {
            Self::Python => "python",
            Self::Javascript => "javascript",
        }
    }

    /// The interpreter to start, from its environment variable or the usual command.
    fn interpreter(self) -> String {
        let (variable, fallback) = match self {
            Self::Python => (PYTHON_ENV, "python3"),
            Self::Javascript => (NODE_ENV, "node"),
        };
        std::env::var(variable)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| fallback.to_owned())
    }

    /// The command that starts a kernel of the language, its driver given inline.
    fn command(self) -> Command {
        let mut command = Command::new(self.interpreter());
        match self {
            Self::Python => command.arg("-u").arg("-c").arg(PYTHON_DRIVER),
            Self::Javascript => command.arg("-e").arg(NODE_DRIVER),
        };
        command
    }
}

/// Arguments of an `eval` call.
#[derive(Deserialize)]
struct Args {
    /// The language to run the code in.
    language: Language,
    /// The code to run; empty with `reset` only restarts the kernel.
    #[serde(default)]
    code: String,
    /// Whether to throw the kernel's state away before running the code.
    #[serde(default)]
    reset: bool,
    /// Timeout in seconds.
    timeout: Option<u64>,
}

/// What a driver answers for one request.
#[derive(Deserialize)]
struct Answer {
    /// What the code printed.
    #[serde(default)]
    output: String,
    /// How the value of the final expression shows, when there is one.
    value: Option<String>,
    /// The error the code raised, with its trace.
    error: Option<String>,
}

/// A running interpreter with its driver.
struct Kernel {
    /// The interpreter process.
    child: Child,
    /// Where requests are written.
    stdin: ChildStdin,
    /// The lines the interpreter writes.
    stdout: Lines<BufReader<ChildStdout>>,
    /// What the interpreter wrote to standard error since the last call.
    stderr: Arc<std::sync::Mutex<String>>,
    /// The prefix of the line that carries an answer.
    marker: String,
}

/// How running one request ended without an answer.
enum Lost {
    /// The timeout passed.
    TimedOut,
    /// The interpreter stopped or its pipes broke, as said.
    Died(String),
}

impl Kernel {
    /// Starts a kernel of `language` in `root`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the interpreter cannot be started.
    fn start(language: Language, root: &std::path::Path) -> Result<Self, ToolError> {
        let marker = format!(
            "\u{1}omc-eval-{}-{}\u{2}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default()
        );
        let mut command = language.command();
        command
            .current_dir(root)
            .env(MARKER_ENV, &marker)
            .env("PYTHONIOENCODING", "utf-8")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let mut child = command.spawn().map_err(|error| {
            ToolError::Failed(format!(
                "cannot start the {} kernel with `{}`: {error}",
                language.name(),
                language.interpreter()
            ))
        })?;
        let (Some(stdin), Some(stdout), Some(mut stderr_pipe)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err(ToolError::Failed(
                "the kernel's pipes are missing".to_owned(),
            ));
        };
        let stderr = Arc::new(std::sync::Mutex::new(String::new()));
        let sink = stderr.clone();
        tokio::spawn(async move {
            let mut chunk = [0u8; 4096];
            while let Ok(read) = stderr_pipe.read(&mut chunk).await {
                if read == 0 {
                    break;
                }
                if let Ok(mut text) = sink.lock() {
                    text.push_str(&String::from_utf8_lossy(&chunk[..read]));
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout).lines(),
            stderr,
            marker,
        })
    }

    /// Sends `code` and waits up to `timeout` for the answer, collecting the stray lines written meanwhile into `stray`.
    async fn run(
        &mut self,
        code: &str,
        timeout: Duration,
        stray: &mut String,
    ) -> Result<Answer, Lost> {
        let request = format!("{}\n", json!({ "code": code }));
        let exchange = async {
            self.stdin
                .write_all(request.as_bytes())
                .await
                .map_err(|error| Lost::Died(format!("cannot send the code: {error}")))?;
            self.stdin
                .flush()
                .await
                .map_err(|error| Lost::Died(format!("cannot send the code: {error}")))?;
            loop {
                match self.stdout.next_line().await {
                    Ok(Some(line)) => match line.strip_prefix(&self.marker) {
                        Some(answer) => {
                            return serde_json::from_str(answer).map_err(|error| {
                                Lost::Died(format!("the kernel answered nonsense: {error}"))
                            });
                        }
                        None => {
                            stray.push_str(&line);
                            stray.push('\n');
                        }
                    },
                    Ok(None) => return Err(Lost::Died("the kernel exited".to_owned())),
                    Err(error) => {
                        return Err(Lost::Died(format!("cannot read the kernel: {error}")));
                    }
                }
            }
        };
        tokio::time::timeout(timeout, exchange)
            .await
            .unwrap_or(Err(Lost::TimedOut))
    }

    /// Takes what the interpreter wrote to standard error since the last time.
    fn take_stderr(&self) -> String {
        self.stderr
            .lock()
            .map(|mut text| std::mem::take(&mut *text))
            .unwrap_or_default()
    }

    /// Kills the interpreter and everything it started.
    async fn kill(mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.child.id().and_then(|pid| i32::try_from(pid).ok()) {
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        let _ = self.child.kill().await;
    }
}

/// Runs Python or JavaScript code in kernels that keep their state between calls.
pub struct Eval {
    /// The directory kernels start in.
    sandbox: Sandbox,
    /// The running kernel of each language, each behind its own lock so that calls to one language wait their turn.
    kernels: Arc<HashMap<Language, Mutex<Option<Kernel>>>>,
}

impl Eval {
    /// Creates the tool starting its kernels in the root of `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        let kernels = [Language::Python, Language::Javascript]
            .into_iter()
            .map(|language| (language, Mutex::new(None)))
            .collect();
        Self {
            sandbox,
            kernels: Arc::new(kernels),
        }
    }
}

/// Cuts `text` to at most [`OUTPUT_LIMIT`] bytes, keeping its start and end and saying how much was left out.
fn truncate(text: &str) -> String {
    if text.len() <= OUTPUT_LIMIT {
        return text.to_owned();
    }
    let half = OUTPUT_LIMIT / 2;
    let mut head = half;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - half;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    format!(
        "{}\n[… {} bytes of output left out …]\n{}",
        &text[..head],
        tail - head,
        &text[tail..]
    )
}

/// Puts the parts of a finished call together as the text the model reads.
fn report(output: &str, value: Option<&str>, error: Option<&str>) -> String {
    let mut text = output.to_owned();
    for part in [value, error].into_iter().flatten() {
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(part);
    }
    if text.trim().is_empty() {
        text = "(no output)".to_owned();
    }
    truncate(&text)
}

#[async_trait]
impl Tool for Eval {
    /// Returns `eval`.
    fn name(&self) -> &'static str {
        "eval"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &str {
        DESCRIPTION
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "javascript"],
                    "description": "The kernel to run the code in."
                },
                "code": {
                    "type": "string",
                    "description": "The code to run. A final expression's value is shown."
                },
                "reset": {
                    "type": "boolean",
                    "description": "Throw the kernel's state away first. Defaults to false."
                },
                "timeout": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_TIMEOUT_SECS,
                    "description": "Timeout in seconds; defaults to 60. A call that runs past it kills the kernel and its state."
                }
            },
            "required": ["language"]
        })
    }

    /// Running code can change anything, so every call asks.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Approvals cover one language's kernel.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        let language = args["language"].as_str().unwrap_or("unknown");
        Some(format!("eval {language}"))
    }

    /// Describes a call by its language and the first line of its code.
    fn describe(&self, args: &Value) -> String {
        let language = args["language"].as_str().unwrap_or_default();
        let first = args["code"]
            .as_str()
            .and_then(|code| code.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or("");
        let reset = if args["reset"].as_bool() == Some(true) {
            " (reset)"
        } else {
            ""
        };
        format!("{language}{reset}: {first}").trim_end().to_owned()
    }

    /// Runs the code in the language's kernel, starting it first when it is not running.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let timeout = Duration::from_secs(
            args.timeout
                .unwrap_or(DEFAULT_TIMEOUT_SECS)
                .clamp(1, MAX_TIMEOUT_SECS),
        );
        let Some(slot) = self.kernels.get(&args.language) else {
            return Err(ToolError::Failed("no kernel for that language".to_owned()));
        };
        let mut slot = slot.lock().await;
        if args.reset
            && let Some(kernel) = slot.take()
        {
            kernel.kill().await;
        }
        if args.code.trim().is_empty() {
            return if args.reset {
                Ok(ToolOutput::new(format!(
                    "The {} kernel was reset.",
                    args.language.name()
                )))
            } else {
                Err(ToolError::InvalidArguments("the code is empty".to_owned()))
            };
        }
        let mut kernel = match slot.take() {
            Some(kernel) => kernel,
            None => Kernel::start(args.language, self.sandbox.root())?,
        };
        let mut stray = String::new();
        let outcome = kernel.run(&args.code, timeout, &mut stray).await;
        let stderr = kernel.take_stderr();
        stray.push_str(&stderr);
        match outcome {
            Ok(answer) => {
                *slot = Some(kernel);
                let output = format!("{stray}{}", answer.output);
                let text = report(&output, answer.value.as_deref(), answer.error.as_deref());
                match answer.error {
                    Some(_) => Err(ToolError::Failed(text)),
                    None => Ok(ToolOutput::new(text)),
                }
            }
            Err(lost) => {
                kernel.kill().await;
                let why = match lost {
                    Lost::TimedOut => format!(
                        "The code ran past the {}s timeout; the kernel was stopped and its state is lost.",
                        timeout.as_secs()
                    ),
                    Lost::Died(why) => {
                        format!("{why}; the kernel was stopped and its state is lost.")
                    }
                };
                Err(ToolError::Failed(report(&stray, None, Some(&why))))
            }
        }
    }
}

#[cfg(test)]
/// Tests for the `eval` tool.
mod tests {
    use super::*;

    /// Whether `program` can be started, so that tests needing it can be skipped where it is missing.
    fn available(program: &str) -> bool {
        std::process::Command::new(program)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    /// A tool working in the crate's folder.
    fn tool() -> Eval {
        Eval::new(Sandbox::new(env!("CARGO_MANIFEST_DIR")).unwrap())
    }

    /// A Python variable set in one call is there in the next, and reset clears it.
    #[tokio::test]
    async fn python_state_persists_between_calls() {
        if !available("python3") {
            return;
        }
        let tool = tool();
        let first = tool
            .call(json!({"language": "python", "code": "x = 40\nprint('set')"}))
            .await
            .unwrap();
        assert_eq!(first.text.trim(), "set");
        let second = tool
            .call(json!({"language": "python", "code": "x + 2"}))
            .await
            .unwrap();
        assert_eq!(second.text.trim(), "42");
        tool.call(json!({"language": "python", "reset": true}))
            .await
            .unwrap();
        let gone = tool
            .call(json!({"language": "python", "code": "x"}))
            .await
            .unwrap_err();
        assert!(gone.to_string().contains("NameError"), "{gone}");
    }

    /// A JavaScript binding set in one call is there in the next, and awaited values are shown.
    #[tokio::test]
    async fn javascript_state_persists_between_calls() {
        if !available("node") {
            return;
        }
        let tool = tool();
        let first = tool
            .call(json!({"language": "javascript", "code": "let total = 40; console.log('set')"}))
            .await
            .unwrap();
        assert_eq!(first.text.trim(), "set");
        let second = tool
            .call(json!({"language": "javascript", "code": "total + 2"}))
            .await
            .unwrap();
        assert_eq!(second.text.trim(), "42");
        let awaited = tool
            .call(json!({"language": "javascript", "code": "await Promise.resolve(total)"}))
            .await
            .unwrap();
        assert_eq!(awaited.text.trim(), "40");
    }

    /// A call past its timeout kills the kernel, and the next call starts a fresh one.
    #[tokio::test]
    async fn timeout_stops_the_kernel() {
        if !available("python3") {
            return;
        }
        let tool = tool();
        tool.call(json!({"language": "python", "code": "y = 1"}))
            .await
            .unwrap();
        let error = tool
            .call(
                json!({"language": "python", "code": "import time\ntime.sleep(30)", "timeout": 1}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("timeout"), "{error}");
        let fresh = tool
            .call(json!({"language": "python", "code": "'y' in globals()"}))
            .await
            .unwrap();
        assert_eq!(fresh.text.trim(), "False");
    }

    /// Long output keeps its start and end and says how much was cut.
    #[test]
    fn long_output_is_cut() {
        let text = "a".repeat(OUTPUT_LIMIT) + &"b".repeat(OUTPUT_LIMIT);
        let cut = truncate(&text);
        assert!(cut.len() < text.len());
        assert!(cut.starts_with('a') && cut.ends_with('b'));
        assert!(cut.contains("bytes of output left out"));
    }
}
