//! One connection to a debug adapter: requests and their answers, the events it sends, and the process it runs in.

use std::collections::HashMap;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, Command};
use tokio::sync::{Notify, oneshot};
use tokio::time::Instant;

use crate::state::State;
use crate::wire::{read_message, write_message};

/// The most of what an adapter printed on its own output and error that is kept, in bytes, to explain why it failed.
const LOG_LIMIT: usize = 8 * 1024;

/// How often a connection to an adapter that is still starting is tried again.
const RETRY: Duration = Duration::from_millis(100);

/// What a request is answered with: the body of a successful answer, or why it failed.
type Answer = Result<Value, String>;

/// Where messages to the adapter go.
type Writer = Arc<tokio::sync::Mutex<Box<dyn AsyncWrite + Send + Unpin>>>;

/// What the adapter has said and what waits on it, which the task reading it fills in.
#[derive(Default)]
pub(crate) struct Heard {
    /// The answers still awaited, by request number.
    pending: Mutex<HashMap<i64, oneshot::Sender<Answer>>>,
    /// What the adapter has said about the program.
    state: Mutex<State>,
    /// Woken each time the adapter sends an event or stops.
    news: Notify,
}

/// A request that was sent and whose answer has not been taken yet.
pub(crate) struct Sent {
    /// The request's name, for messages.
    command: String,
    /// Where the answer comes.
    answer: oneshot::Receiver<Answer>,
}

/// A started debug adapter, or a connection to one already running.
pub(crate) struct Client {
    /// Where messages to the adapter go.
    writer: Writer,
    /// What the adapter has said and what waits on it.
    heard: Arc<Heard>,
    /// The number the next request gets.
    next: AtomicI64,
    /// The adapter's process, when the harness started it; killed with its process group when the client is dropped.
    child: Option<Child>,
    /// The end of what the adapter printed on its own output and error.
    log: Arc<Mutex<String>>,
}

impl Client {
    /// A client speaking with an adapter over `reader` and `writer`, which runs in `child` when the harness started it.
    pub(crate) fn over(
        reader: impl AsyncRead + Send + Unpin + 'static,
        writer: impl AsyncWrite + Send + Unpin + 'static,
        child: Option<Child>,
    ) -> Self {
        let writer: Writer = Arc::new(tokio::sync::Mutex::new(Box::new(writer)));
        let heard = Arc::new(Heard::default());
        tokio::spawn(listen(reader, writer.clone(), heard.clone()));
        Self {
            writer,
            heard,
            next: AtomicI64::new(1),
            child,
            log: Arc::new(Mutex::new(String::new())),
        }
    }

    /// Starts the adapter `program` with `arguments` in `cwd`, speaking over its standard input and output.
    ///
    /// # Errors
    ///
    /// Returns why when the program cannot be started.
    pub(crate) fn spawn_stdio(
        program: &Path,
        arguments: &[String],
        cwd: &Path,
    ) -> Result<Self, String> {
        let mut child = command(program, arguments, cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{} could not be started: {error}", program.display()))?;
        let stdin = child.stdin.take().ok_or("the debug adapter has no input")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("the debug adapter has no output")?;
        let stderr = child.stderr.take();
        let client = Self::over(stdout, stdin, Some(child));
        if let Some(stderr) = stderr {
            tokio::spawn(collect(stderr, client.log.clone()));
        }
        Ok(client)
    }

    /// Starts the adapter `program` with `arguments`, in which `{port}` is replaced by a free port it is told to listen on, in `cwd`, and connects to it, trying for up to `wait`.
    ///
    /// # Errors
    ///
    /// Returns why, with what the adapter printed, when it cannot be started, exits, or does not listen in time.
    pub(crate) async fn spawn_port(
        program: &Path,
        arguments: &[String],
        cwd: &Path,
        wait: Duration,
    ) -> Result<Self, String> {
        let port = free_port()?;
        let arguments: Vec<String> = arguments
            .iter()
            .map(|argument| argument.replace("{port}", &port.to_string()))
            .collect();
        let mut child = command(program, &arguments, cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|error| format!("{} could not be started: {error}", program.display()))?;
        let log = Arc::new(Mutex::new(String::new()));
        if let Some(stdout) = child.stdout.take() {
            tokio::spawn(collect(stdout, log.clone()));
        }
        if let Some(stderr) = child.stderr.take() {
            tokio::spawn(collect(stderr, log.clone()));
        }
        let deadline = Instant::now() + wait;
        let stream = loop {
            if let Ok(stream) = TcpStream::connect(("127.0.0.1", port)).await {
                break stream;
            }
            let exited = child.try_wait().ok().flatten();
            if exited.is_some() || Instant::now() >= deadline {
                let said = lock(&log).trim().to_owned();
                let how = match exited {
                    Some(status) => format!("exited ({status})"),
                    None => format!("did not listen on port {port} in time"),
                };
                return Err(format!("{} {how}{}", program.display(), said_suffix(&said)));
            }
            tokio::time::sleep(RETRY).await;
        };
        let (reader, writer) = stream.into_split();
        let mut client = Self::over(reader, writer, Some(child));
        client.log = log;
        Ok(client)
    }

    /// Connects to an adapter already listening at `address`, a `host:port`, trying for up to `wait`.
    ///
    /// # Errors
    ///
    /// Returns why when nothing answers at the address in time.
    pub(crate) async fn connect(address: &str, wait: Duration) -> Result<Self, String> {
        let deadline = Instant::now() + wait;
        loop {
            match TcpStream::connect(address).await {
                Ok(stream) => {
                    let (reader, writer) = stream.into_split();
                    return Ok(Self::over(reader, writer, None));
                }
                Err(error) if Instant::now() >= deadline => {
                    return Err(format!("could not connect to {address}: {error}"));
                }
                Err(_) => tokio::time::sleep(RETRY).await,
            }
        }
    }

    /// Sends the request `command` with `arguments` without waiting for its answer, which [`Client::answer`] takes.
    ///
    /// # Errors
    ///
    /// Returns why when the request cannot be written.
    pub(crate) async fn send(&self, command: &str, arguments: Value) -> Result<Sent, String> {
        let seq = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        lock(&self.heard.pending).insert(seq, answer);
        let message =
            json!({"seq": seq, "type": "request", "command": command, "arguments": arguments});
        let written = write_message(&mut *self.writer.lock().await, &message).await;
        if let Err(error) = written {
            lock(&self.heard.pending).remove(&seq);
            return Err(format!(
                "the debug adapter could not be told {command}: {error}"
            ));
        }
        Ok(Sent {
            command: command.to_owned(),
            answer: answered,
        })
    }

    /// Waits up to `wait` for the answer to `sent`.
    ///
    /// # Errors
    ///
    /// Returns the adapter's reason when it refuses the request, and says so when it stops or does not answer in time.
    pub(crate) async fn answer(&self, sent: &mut Sent, wait: Duration) -> Answer {
        match tokio::time::timeout(wait, &mut sent.answer).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(format!(
                "the debug adapter stopped before answering {}{}",
                sent.command,
                said_suffix(self.log().trim())
            )),
            Err(_) => Err(format!(
                "the debug adapter did not answer {} within {}s",
                sent.command,
                wait.as_secs()
            )),
        }
    }

    /// Sends the request `command` with `arguments` and waits up to `wait` for its answer's body.
    ///
    /// # Errors
    ///
    /// Same as [`Client::send`] and [`Client::answer`].
    pub(crate) async fn request(&self, command: &str, arguments: Value, wait: Duration) -> Answer {
        let mut sent = self.send(command, arguments).await?;
        self.answer(&mut sent, wait).await
    }

    /// What the adapter has said about the program, locked.
    pub(crate) fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.heard.state)
    }

    /// Waits until `done` holds of what the adapter has said, the adapter stops talking, or `deadline` passes; whether `done` held.
    pub(crate) async fn wait_until(
        &self,
        deadline: Instant,
        done: impl Fn(&State) -> bool,
    ) -> bool {
        loop {
            let news = self.heard.news.notified();
            tokio::pin!(news);
            news.as_mut().enable();
            {
                let state = self.state();
                if done(&state) {
                    return true;
                }
                if state.gone {
                    return false;
                }
            }
            if tokio::time::timeout_at(deadline, news).await.is_err() {
                return done(&self.state());
            }
        }
    }

    /// The end of what the adapter printed on its own output and error.
    pub(crate) fn log(&self) -> String {
        lock(&self.log).clone()
    }

    /// The adapter's process id, when the harness started it.
    pub(crate) fn process_id(&self) -> Option<u32> {
        self.child.as_ref().and_then(Child::id)
    }
}

impl Drop for Client {
    /// Kills the adapter's process group, so that helpers it started, such as a debug server, go with it.
    fn drop(&mut self) {
        if let Some(pid) = self.process_id() {
            kill_group(pid);
        }
    }
}

/// Sends SIGKILL to the process group led by `pid`.
#[cfg(unix)]
fn kill_group(pid: u32) {
    let Ok(pgid) = i32::try_from(pid) else { return };
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

/// Does nothing where there are no process groups; the adapter itself is killed when its handle is dropped.
#[cfg(not(unix))]
fn kill_group(_pid: u32) {}

/// Sends SIGKILL to the process `pid`.
#[cfg(unix)]
pub(crate) fn kill_process(pid: u32) {
    let Ok(pid) = i32::try_from(pid) else { return };
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

/// Does nothing where processes cannot be signalled this way.
#[cfg(not(unix))]
pub(crate) fn kill_process(_pid: u32) {}

/// The command that starts `program` with `arguments` in `cwd`, its error collected, killed when dropped, and in a process group of its own on Unix.
fn command(program: &Path, arguments: &[String], cwd: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .current_dir(cwd)
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    command
}

/// A port on the loopback interface nothing listens on right now.
///
/// # Errors
///
/// Returns why when no port can be had.
fn free_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| format!("no free port for the debug adapter: {error}"))?;
    listener
        .local_addr()
        .map(|address| address.port())
        .map_err(|error| format!("no free port for the debug adapter: {error}"))
}

/// `said` as the end of a message: nothing when it is empty, otherwise what the adapter printed.
fn said_suffix(said: &str) -> String {
    if said.is_empty() {
        String::new()
    } else {
        format!("; it printed:\n{said}")
    }
}

/// Keeps the end of what `stream` prints in `log`, until it ends.
async fn collect(mut stream: impl AsyncRead + Unpin, log: Arc<Mutex<String>>) {
    let mut buffer = [0u8; 4096];
    while let Ok(read) = stream.read(&mut buffer).await {
        if read == 0 {
            break;
        }
        let mut log = lock(&log);
        log.push_str(&String::from_utf8_lossy(&buffer[..read]));
        if log.len() > LOG_LIMIT {
            let mut cut = log.len() - LOG_LIMIT;
            while !log.is_char_boundary(cut) {
                cut += 1;
            }
            log.drain(..cut);
        }
    }
}

/// Reads what the adapter on `reader` says until it stops: hands each answer to whoever waits on it, keeps its events in `heard`, and turns down its own requests through `writer`, since the harness runs nothing in a terminal for it; the requests still waiting when it stops are failed.
async fn listen(reader: impl AsyncRead + Unpin, writer: Writer, heard: Arc<Heard>) {
    let mut reader = BufReader::new(reader);
    let mut seq = 1_000_000;
    while let Some(message) = read_message(&mut reader).await {
        match message["type"].as_str() {
            Some("response") => {
                let Some(id) = message["request_seq"].as_i64() else {
                    continue;
                };
                let Some(waiting) = lock(&heard.pending).remove(&id) else {
                    continue;
                };
                let _ = waiting.send(answer_of(&message));
            }
            Some("event") => {
                let event = message["event"].as_str().unwrap_or_default();
                lock(&heard.state).apply(event, &message["body"]);
                heard.news.notify_waiters();
            }
            Some("request") => {
                seq += 1;
                let refusal = json!({
                    "seq": seq,
                    "type": "response",
                    "request_seq": message["seq"],
                    "command": message["command"],
                    "success": false,
                    "message": "not supported by this client",
                });
                let _ = write_message(&mut *writer.lock().await, &refusal).await;
            }
            _ => {}
        }
    }
    lock(&heard.state).gone = true;
    lock(&heard.pending).clear();
    heard.news.notify_waiters();
}

/// What the response `message` answers: its body when it succeeded, otherwise the reason the adapter gave.
fn answer_of(message: &Value) -> Answer {
    if message["success"].as_bool().unwrap_or(false) {
        return Ok(message.get("body").cloned().unwrap_or(Value::Null));
    }
    let detail = &message["body"]["error"];
    let formatted = detail["format"].as_str().map(|format| {
        let mut text = format.to_owned();
        if let Some(variables) = detail["variables"].as_object() {
            for (name, value) in variables {
                let value = value
                    .as_str()
                    .map_or_else(|| value.to_string(), str::to_owned);
                text = text.replace(&format!("{{{name}}}"), &value);
            }
        }
        text
    });
    let reason = formatted
        .or_else(|| {
            message["message"]
                .as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "the request failed".to_owned());
    Err(reason)
}

/// Locks `mutex`, going on with it when a panic poisoned it.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
/// Tests for talking with an adapter.
mod tests {
    use super::*;

    /// A failed answer gives the detailed message with its variables filled in, or the short one.
    #[test]
    fn failures_carry_the_adapters_reason() {
        let detailed = json!({
            "success": false,
            "message": "launch failed",
            "body": {"error": {"format": "cannot run {path}", "variables": {"path": "/x"}}},
        });
        assert_eq!(answer_of(&detailed), Err("cannot run /x".to_owned()));
        let short = json!({"success": false, "message": "nope"});
        assert_eq!(answer_of(&short), Err("nope".to_owned()));
        let ok = json!({"success": true, "body": {"a": 1}});
        assert_eq!(answer_of(&ok), Ok(json!({"a": 1})));
    }
}
