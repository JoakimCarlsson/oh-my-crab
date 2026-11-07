//! A tool server that is a program, spoken to line by line.

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};

use omc_wire::Reach;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// What a request to a program that has stopped fails with.
pub(crate) const STOPPED: &str = "the tool server stopped";

/// A running program that speaks the protocol on its standard streams.
pub(crate) struct Program {
    /// The program, ended when this is dropped.
    _child: Child,
    /// What requests are written to.
    stdin: Mutex<ChildStdin>,
    /// What answers are read from.
    lines: Mutex<Lines<BufReader<ChildStdout>>>,
    /// Whether the program was found to have stopped: its output ended or it could not be written to.
    stopped: AtomicBool,
}

impl Program {
    /// Starts the program `reach` describes, in `folder` when there is one and in crab's own folder otherwise.
    ///
    /// # Errors
    ///
    /// Fails with the reason when `reach` is not a command or the program cannot be started.
    pub(crate) fn start(reach: &Reach, folder: Option<&Path>) -> Result<Self, String> {
        let Reach::Command { program, args, env } = reach else {
            return Err("not a command".to_owned());
        };
        let mut command = Command::new(program);
        command
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(folder) = folder {
            command.current_dir(folder);
        }
        let mut child = command
            .spawn()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        let stdin = child.stdin.take().ok_or("no input pipe")?;
        let stdout = child.stdout.take().ok_or("no output pipe")?;
        Ok(Self {
            _child: child,
            stdin: Mutex::new(stdin),
            lines: Mutex::new(BufReader::new(stdout).lines()),
            stopped: AtomicBool::new(false),
        })
    }

    /// Whether the program was found to have stopped.
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }

    /// Writes `message` as one line.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program cannot be written to.
    pub(crate) async fn send(&self, message: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().await;
        let mut line = message.to_string();
        line.push('\n');
        let written = match stdin.write_all(line.as_bytes()).await {
            Ok(()) => stdin.flush().await,
            Err(error) => Err(error),
        };
        written.map_err(|error| {
            self.stopped.store(true, Ordering::Relaxed);
            format!("{STOPPED}: {error}")
        })
    }

    /// Sends the request `method` under `id` and returns its result; a `ping` the program sends meanwhile is answered, and other requests it sends are refused.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program stops or answers with an error.
    pub(crate) async fn request(
        &self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let mut lines = self.lines.lock().await;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if let Some(asked) = message.get("method").and_then(Value::as_str) {
                if let Some(their) = message.get("id") {
                    self.send(&reply(their, asked)).await?;
                }
                continue;
            }
            if message["id"] == json!(id) {
                return crate::answer(&message);
            }
        }
        self.stopped.store(true, Ordering::Relaxed);
        Err(STOPPED.to_owned())
    }
}

/// The answer to the request `method` a program sent under `id`: an empty result for `ping`, and "method not found" for anything else, since crab offers the program nothing else.
fn reply(id: &Value, method: &str) -> Value {
    if method == "ping" {
        json!({ "jsonrpc": "2.0", "id": id, "result": {} })
    } else {
        json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found" } })
    }
}

#[cfg(test)]
/// Tests for answering what a program asks.
mod tests {
    use super::reply;
    use serde_json::json;

    /// A ping is answered with an empty result, anything else refused.
    #[test]
    fn pings_are_answered() {
        assert_eq!(reply(&json!(7), "ping")["result"], json!({}));
        assert_eq!(reply(&json!("a"), "roots/list")["error"]["code"], -32601);
    }
}
