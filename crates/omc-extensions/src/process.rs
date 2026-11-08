//! One running extension program, spoken to in JSON-RPC 2.0 messages, one per line, on its standard streams.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{mpsc, oneshot};

use crate::config::Extension;

/// How long a program that was asked to shut down has to end before it is killed.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// The answers still awaited, by the id of their request.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, Failure>>>>>;

/// Why a request got no answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Failure {
    /// The program did not answer in time.
    TimedOut(Duration),
    /// The program ended, or could not be written to, before it answered.
    Died(String),
    /// The program answered with an error.
    Refused(String),
}

impl std::fmt::Display for Failure {
    /// The failure in words, for the model or the reader.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut(limit) => write!(f, "it gave no answer within {}s", limit.as_secs_f64()),
            Self::Died(reason) => write!(f, "it stopped: {reason}"),
            Self::Refused(reason) => write!(f, "it answered with an error: {reason}"),
        }
    }
}

/// A running extension.
pub(crate) struct Connection {
    /// The program, killed when this is dropped.
    child: tokio::sync::Mutex<Child>,
    /// Where lines to write to the program go; dropping it closes the program's input.
    writer: Mutex<Option<mpsc::UnboundedSender<String>>>,
    /// The answers still awaited.
    pending: Pending,
    /// The id the next request gets.
    next: AtomicU64,
    /// Whether the program still runs and can be written to.
    alive: Arc<AtomicBool>,
}

impl Connection {
    /// Starts `extension` in its own folder, or in `folder` when it names none, with its output read on tasks of their own: answers go to the requests awaiting them, `log` notifications and everything it writes to standard error to crab's standard error under its name.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program cannot be started.
    pub(crate) fn start(extension: &Extension, folder: &Path) -> Result<Self, String> {
        let mut command = Command::new(&extension.program);
        command
            .args(&extension.args)
            .envs(extension.env.iter().map(|(name, value)| (name, value)))
            .current_dir(extension.folder.as_deref().unwrap_or(folder))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {}: {error}", extension.program))?;
        let stdin = child.stdin.take().ok_or("no input pipe")?;
        let stdout = child.stdout.take().ok_or("no output pipe")?;
        let stderr = child.stderr.take().ok_or("no error pipe")?;
        let alive = Arc::new(AtomicBool::new(true));
        let pending: Pending = Arc::default();
        let (writer, mut lines) = mpsc::unbounded_channel::<String>();
        let name = extension.name.clone();
        let writing = alive.clone();
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = lines.recv().await {
                let written = async {
                    stdin.write_all(line.as_bytes()).await?;
                    stdin.write_all(b"\n").await?;
                    stdin.flush().await
                };
                if written.await.is_err() {
                    writing.store(false, Ordering::SeqCst);
                    break;
                }
            }
        });
        tokio::spawn(read_answers(
            name.clone(),
            stdout,
            pending.clone(),
            alive.clone(),
            writer.clone(),
        ));
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("extension {name}: {line}");
            }
        });
        Ok(Self {
            child: tokio::sync::Mutex::new(child),
            writer: Mutex::new(Some(writer)),
            pending,
            next: AtomicU64::new(1),
            alive,
        })
    }

    /// Whether the program still runs and can be written to.
    pub(crate) fn alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Writes `message` to the program.
    ///
    /// # Errors
    ///
    /// Fails when the program's input is closed.
    fn send(&self, message: &Value) -> Result<(), Failure> {
        let writer = self.writer.lock().unwrap_or_else(|held| held.into_inner());
        let sent = writer
            .as_ref()
            .is_some_and(|writer| writer.send(message.to_string()).is_ok());
        if sent {
            Ok(())
        } else {
            self.alive.store(false, Ordering::SeqCst);
            Err(Failure::Died("its input is closed".to_owned()))
        }
    }

    /// Asks the program `method` with `params` and waits at most `limit` for its answer.
    ///
    /// # Errors
    ///
    /// Fails when the program does not answer in time, ends first, or answers with an error.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
        limit: Duration,
    ) -> Result<Value, Failure> {
        if !self.alive() {
            return Err(Failure::Died("it is not running".to_owned()));
        }
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        let (answer, answered) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .insert(id, answer);
        if let Err(failure) =
            self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
        {
            self.forget(id);
            return Err(failure);
        }
        match tokio::time::timeout(limit, answered).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(Failure::Died("it ended before it answered".to_owned())),
            Err(_) => {
                self.forget(id);
                Err(Failure::TimedOut(limit))
            }
        }
    }

    /// Stops waiting for the answer to the request `id`.
    fn forget(&self, id: u64) {
        self.pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .remove(&id);
    }

    /// Tells the program `method` with `params`, without waiting for anything; a program that has stopped is not told.
    pub(crate) fn notify(&self, method: &str, params: Value) {
        if self.alive() {
            let _ = self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
        }
    }

    /// Asks the program to end with a `shutdown` notification and by closing its input, and kills it when it has not ended within [`SHUTDOWN_GRACE`].
    pub(crate) async fn shutdown(&self) {
        self.notify("shutdown", json!({}));
        self.writer
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .take();
        let mut child = self.child.lock().await;
        if tokio::time::timeout(SHUTDOWN_GRACE, child.wait())
            .await
            .is_err()
        {
            let _ = child.kill().await;
        }
        self.alive.store(false, Ordering::SeqCst);
    }
}

/// Reads what the extension `name` writes on `stdout` until it ends: answers go to the requests in `pending` awaiting them, `log` notifications to crab's standard error, and requests, which crab answers none of, get an error through `writer`. When the output ends, the program counts as stopped in `alive` and every request still waiting fails.
async fn read_answers(
    name: String,
    stdout: tokio::process::ChildStdout,
    pending: Pending,
    alive: Arc<AtomicBool>,
    writer: mpsc::UnboundedSender<String>,
) {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            eprintln!("extension {name}: not a JSON message: {line}");
            continue;
        };
        let id = message.get("id").and_then(Value::as_u64);
        match (id, message.get("method").and_then(Value::as_str)) {
            (Some(id), None) => {
                let answer = match message.get("error") {
                    Some(error) => Err(Failure::Refused(
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .map_or_else(|| error.to_string(), str::to_owned),
                    )),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let waiting = pending
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .remove(&id);
                if let Some(waiting) = waiting {
                    let _ = waiting.send(answer);
                }
            }
            (None, Some("log")) => {
                let params = message.get("params").cloned().unwrap_or_default();
                let text = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                match params.get("level").and_then(Value::as_str) {
                    Some(level) => eprintln!("extension {name} ({level}): {text}"),
                    None => eprintln!("extension {name}: {text}"),
                }
            }
            (Some(id), Some(method)) => {
                let _ = writer.send(
                    json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("crab answers no `{method}` request")}})
                        .to_string(),
                );
            }
            (None, Some(_)) | (None, None) => {}
        }
    }
    alive.store(false, Ordering::SeqCst);
    let waiting: Vec<_> = pending
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .drain()
        .collect();
    for (_, waiting) in waiting {
        let _ = waiting.send(Err(Failure::Died("its output ended".to_owned())));
    }
}
