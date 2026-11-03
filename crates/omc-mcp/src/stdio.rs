//! A tool server that is a program, spoken to line by line.

use std::process::Stdio;

use omc_wire::Reach;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::Mutex;

/// A running program that speaks the protocol on its standard streams.
pub(crate) struct Program {
    /// The program, ended when this is dropped.
    _child: Child,
    /// What requests are written to.
    stdin: Mutex<ChildStdin>,
    /// What answers are read from.
    lines: Mutex<Lines<BufReader<ChildStdout>>>,
}

impl Program {
    /// Starts the program `reach` describes.
    ///
    /// # Errors
    ///
    /// Fails with the reason when `reach` is not a command or the program cannot be started.
    pub(crate) fn start(reach: &Reach) -> Result<Self, String> {
        let Reach::Command { program, args, env } = reach else {
            return Err("not a command".to_owned());
        };
        let mut child = Command::new(program)
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("could not start {program}: {error}"))?;
        let stdin = child.stdin.take().ok_or("no input pipe")?;
        let stdout = child.stdout.take().ok_or("no output pipe")?;
        Ok(Self {
            _child: child,
            stdin: Mutex::new(stdin),
            lines: Mutex::new(BufReader::new(stdout).lines()),
        })
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
        stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
        stdin.flush().await.map_err(|error| error.to_string())
    }

    /// Sends the request `method` under `id` and returns its result.
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
        while let Some(line) = lines.next_line().await.map_err(|error| error.to_string())? {
            let Ok(message) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if message["id"] == json!(id) && message.get("method").is_none() {
                return crate::answer(&message);
            }
        }
        Err("the tool server stopped".to_owned())
    }
}
