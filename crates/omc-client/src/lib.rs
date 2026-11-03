//! The application's end of a harness: starts it, sends it commands and collects its events.
//!
//! Everything here is blocking and uses plain threads, so it fits an
//! application with no async runtime of its own.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command as Process, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use omc_wire::{Command, Event, Opening, Reach, ToolServer};
use omc_wire::{decode, encode};

/// How many characters of the harness's error stream are kept to explain a stop.
const ERROR_TAIL: usize = 400;

/// A running harness.
pub struct Harness {
    /// The process, shared with whoever ends it from another thread.
    child: Arc<Mutex<Child>>,
    /// What commands are written to.
    stdin: ChildStdin,
    /// The events the harness wrote, one per line.
    events: Receiver<Event>,
    /// The last of what the harness wrote to its error stream.
    errors: Arc<Mutex<String>>,
}

/// What listening to the harness for a moment came to.
#[derive(Debug)]
pub enum Heard {
    /// The harness reported this.
    Event(Event),
    /// Nothing came in the time given.
    Silence,
    /// The harness has stopped, and this is what it said last.
    Closed(String),
}

impl Harness {
    /// Starts `process` with its standard streams piped.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program cannot be started.
    pub fn start(mut process: Process) -> Result<Self, String> {
        let mut child = process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("could not start: {error}"))?;
        let stdin = child.stdin.take().ok_or("no input pipe")?;
        let stdout = child.stdout.take().ok_or("no output pipe")?;
        let errors = Arc::new(Mutex::new(String::new()));
        if let Some(stderr) = child.stderr.take() {
            let kept = Arc::clone(&errors);
            std::thread::spawn(move || keep_tail(stderr, &kept));
        }
        let (sender, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(event) = decode::<Event>(&line)
                    && sender.send(event).is_err()
                {
                    break;
                }
            }
        });
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            stdin,
            events,
            errors,
        })
    }

    /// Sends `command` to the harness.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the harness can no longer be written to.
    pub fn send(&mut self, command: &Command) -> Result<(), String> {
        let line = encode(command).map_err(|error| error.to_string())?;
        self.stdin
            .write_all(line.as_bytes())
            .and_then(|()| self.stdin.flush())
            .map_err(|error| format!("could not write: {error}"))
    }

    /// The next event, waiting up to `patience` for one.
    pub fn listen(&self, patience: Duration) -> Heard {
        match self.events.recv_timeout(patience) {
            Ok(event) => Heard::Event(event),
            Err(RecvTimeoutError::Timeout) => Heard::Silence,
            Err(RecvTimeoutError::Disconnected) => Heard::Closed(self.stopped()),
        }
    }

    /// The events that are already waiting, without waiting for more.
    pub fn pending(&self) -> Vec<Event> {
        let mut found = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            found.push(event);
        }
        found
    }

    /// Whether the harness has stopped and everything it said was taken.
    pub fn is_closed(&self) -> bool {
        matches!(self.events.try_recv(), Err(TryRecvError::Disconnected))
    }

    /// Why the harness stopped: its last words on the error stream, when it said any.
    pub fn stopped(&self) -> String {
        std::thread::sleep(Duration::from_millis(200));
        match self.errors.lock().map(|kept| kept.trim().to_owned()) {
            Ok(said) if !said.is_empty() => format!("stopped: {said}"),
            _ => "stopped".to_owned(),
        }
    }

    /// The process, for ending it from another thread.
    pub fn process(&self) -> Arc<Mutex<Child>> {
        Arc::clone(&self.child)
    }
}

impl Drop for Harness {
    /// Ends the process and collects it.
    fn drop(&mut self) {
        end(&self.child);
    }
}

/// Ends the process `child` and collects it.
pub fn end(child: &Mutex<Child>) {
    if let Ok(mut child) = child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Reads `stream` line by line, keeping the last [`ERROR_TAIL`] characters in `kept`.
fn keep_tail(stream: impl std::io::Read, kept: &Mutex<String>) {
    for line in BufReader::new(stream).lines().map_while(Result::ok) {
        if let Ok(mut kept) = kept.lock() {
            kept.push_str(&line);
            kept.push('\n');
            let extra = kept.chars().count().saturating_sub(ERROR_TAIL);
            let cut = kept.char_indices().nth(extra).map_or(0, |(at, _)| at);
            kept.drain(..cut);
        }
    }
}
