//! Background runs: work that goes on beyond the tool call that started it, owned by the conversation.
//!
//! The agent can wait for the next run to end, list the runs, read what a command printed so far and cancel a run. A run that ends unreported while the conversation is idle can wake it: the conversation hears of it through [`Jobs::ended`] and takes the runs to report with [`Jobs::take_wakes`].

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use omc_wire::Background;
use regex::Regex;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::ToolError;

/// How many finished runs are kept for `wait`; when more end unreported, the oldest are dropped.
const MAX_UNREPORTED: usize = 32;

/// How many ended runs are kept for listing and reading; when more end, the oldest are forgotten.
const MAX_ENDED: usize = 32;

/// How many of the latest bytes a command printed are kept for reading while it runs.
const FEED_KEEP: usize = 256 * 1024;

/// How many lines a read of a run's output shows when the caller names no number.
pub const DEFAULT_LINES: usize = 50;

/// The most lines a read of a run's output shows.
pub const MAX_LINES: usize = 500;

/// The most bytes a read of a run's output shows, its end kept.
const MAX_READ_BYTES: usize = 16_000;

/// How many lines of the end of a run's output the notice of its end carries.
const NOTICE_LINES: usize = 20;

/// What a background run came to, as `wait` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The handle the run was started under.
    pub id: String,
    /// The command or task that was run.
    pub label: String,
    /// Whether it ended without failing.
    pub ok: bool,
    /// What it printed or answered, or why it failed.
    pub text: String,
}

impl Finished {
    /// The report the model reads: the handle, the status and the output.
    pub fn report(&self) -> String {
        format!("{}\n{}", self.headline(), self.text)
    }

    /// A short report for the notice that wakes the agent: the handle, the status, the first line of the output, which holds a command's exit code, and its last lines.
    pub fn brief(&self) -> String {
        let lines: Vec<&str> = self.text.lines().collect();
        let body = if lines.len() <= NOTICE_LINES + 1 {
            lines.join("\n")
        } else {
            format!(
                "{}\n[… {} lines left out; `wait` with `action` `output` and `id` {} shows more]\n{}",
                lines[0],
                lines.len() - NOTICE_LINES - 1,
                self.id,
                lines[lines.len() - NOTICE_LINES..].join("\n")
            )
        };
        format!("{}\n{}", self.headline(), cap_tail(&body, MAX_READ_BYTES).0)
    }

    /// The handle, the status and the first line of the label.
    fn headline(&self) -> String {
        let status = if self.ok { "completed" } else { "failed" };
        format!(
            "{} {status}: {}",
            self.id,
            self.label.lines().next().unwrap_or_default()
        )
    }
}

/// What a background run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A shell command, whose output can be read while it runs.
    Command,
    /// One or more subagents, which report when they end.
    Subagent,
}

impl Kind {
    /// The word the model reads for the kind.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Command => "command",
            Kind::Subagent => "subagent",
        }
    }
}

/// How a run that is listed stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// It still runs.
    Running,
    /// It ended without failing.
    Completed,
    /// It failed.
    Failed,
    /// It was cancelled.
    Cancelled,
}

impl Status {
    /// The word the model reads for the status.
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Completed => "completed",
            Status::Failed => "failed",
            Status::Cancelled => "cancelled",
        }
    }
}

/// One run as [`Jobs::list`] gives it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listed {
    /// The handle the run was started under.
    pub id: String,
    /// The command or task.
    pub label: String,
    /// What it is.
    pub kind: Kind,
    /// How it stands.
    pub status: Status,
    /// How long it has run, or ran.
    pub elapsed: Duration,
}

impl Listed {
    /// The line the model reads for the run.
    pub fn line(&self) -> String {
        format!(
            "{} {} {} {}: {}",
            self.id,
            self.kind.as_str(),
            self.status.as_str(),
            span(self.elapsed),
            self.label.lines().next().unwrap_or_default()
        )
    }
}

/// What a read of a run's output shows.
#[derive(Debug, Clone)]
pub struct Look {
    /// Only what the run printed since the last read of it.
    pub fresh: bool,
    /// Only the lines this matches, when given.
    pub pattern: Option<Regex>,
    /// How many of the last lines to show, at most [`MAX_LINES`].
    pub lines: usize,
}

impl Default for Look {
    /// The last [`DEFAULT_LINES`] lines of everything kept, unfiltered.
    fn default() -> Self {
        Self {
            fresh: false,
            pattern: None,
            lines: DEFAULT_LINES,
        }
    }
}

/// The latest output of a running command, which the command's readers write and the agent reads.
#[derive(Clone, Default)]
pub struct Feed {
    /// What was kept and how far the agent read.
    inner: Arc<Mutex<FeedState>>,
}

/// What a [`Feed`] keeps.
#[derive(Default)]
struct FeedState {
    /// The latest bytes, at most [`FEED_KEEP`].
    bytes: Vec<u8>,
    /// How many bytes came before the first one kept.
    start: u64,
    /// How many bytes, counted from the very first, the agent has read.
    seen: u64,
}

impl Feed {
    /// An empty feed.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `chunk` to the output, forgetting the oldest bytes beyond [`FEED_KEEP`].
    pub fn push(&self, chunk: &[u8]) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.bytes.extend_from_slice(chunk);
        if state.bytes.len() > FEED_KEEP {
            let cut = state.bytes.len() - FEED_KEEP;
            state.bytes.drain(..cut);
            state.start += cut as u64;
        }
    }

    /// The output kept, or only what came since the last read when `fresh`, marking all of it read; with whether bytes asked for were already forgotten.
    fn take(&self, fresh: bool) -> (String, bool) {
        let mut state = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let wanted = if fresh { state.seen } else { 0 };
        let from = wanted.max(state.start);
        let skip = usize::try_from(from - state.start).unwrap_or(usize::MAX);
        let text =
            String::from_utf8_lossy(state.bytes.get(skip..).unwrap_or_default()).into_owned();
        state.seen = state.start + state.bytes.len() as u64;
        (text, from > wanted)
    }
}

/// What stops a run through its own means, such as stopping a subagent; it says whether it stopped anything.
pub type Stop = Arc<dyn Fn() -> bool + Send + Sync>;

/// What a run is started with, besides its work.
pub struct Launch {
    /// The command or task.
    label: String,
    /// The call that started it, when known.
    call: Option<String>,
    /// What it is.
    kind: Kind,
    /// Where its output is written while it runs, when it can be read.
    feed: Option<Feed>,
    /// How it is stopped other than by dropping its work, when it has such a way.
    stop: Option<Stop>,
}

impl Launch {
    /// A run of `kind` described by `label`, started by the call `call` when known, with no output to read and stopped by dropping its work.
    pub fn new(label: String, call: Option<String>, kind: Kind) -> Self {
        Self {
            label,
            call,
            kind,
            feed: None,
            stop: None,
        }
    }

    /// The same run, whose output so far can be read from `feed`.
    pub fn fed_by(self, feed: Feed) -> Self {
        Self {
            feed: Some(feed),
            ..self
        }
    }

    /// The same run, cancelled by calling `stop`, and by dropping its work only when `stop` stops nothing.
    pub fn stopped_by(self, stop: Stop) -> Self {
        Self {
            stop: Some(stop),
            ..self
        }
    }
}

/// A run that has not ended.
struct Running {
    /// The command or task.
    label: String,
    /// The call that started it, when known.
    call: Option<String>,
    /// What it is.
    kind: Kind,
    /// When it started.
    started: Instant,
    /// How to drop its work.
    handle: AbortHandle,
    /// Where its output is written, when it can be read.
    feed: Option<Feed>,
    /// How it is stopped other than by dropping its work.
    stop: Option<Stop>,
    /// Whether the agent cancelled it, so that its end is neither reported again nor wakes anyone.
    cancelled: bool,
    /// Whether its end may wake the conversation.
    wakes: bool,
}

/// A run that ended, kept for listing and reading.
struct Ended {
    /// The handle it was started under.
    id: String,
    /// The command or task.
    label: String,
    /// What it was.
    kind: Kind,
    /// How it ended.
    status: Status,
    /// How long it ran.
    took: Duration,
    /// What it printed, when it was a command.
    feed: Option<Feed>,
    /// What it answered, or why it failed.
    text: String,
}

/// A run that ended and was not reported yet.
struct Unreported {
    /// What it came to.
    finished: Finished,
    /// Whether its end may wake the conversation.
    wakes: bool,
}

/// What the runs are doing.
#[derive(Default)]
struct State {
    /// The number the next handle gets.
    next: u64,
    /// The runs that have not ended, by handle.
    running: HashMap<String, Running>,
    /// The runs that ended and have not been waited on, oldest first.
    done: VecDeque<Unreported>,
    /// The runs that ended, oldest first, for listing and reading.
    ended: VecDeque<Ended>,
}

/// What the runs and their owner share.
struct Inner {
    /// What the runs are doing.
    state: Mutex<State>,
    /// Woken when a run ends.
    wake: Notify,
    /// Woken when a run that may wake the conversation ends.
    signal: Notify,
    /// Whether runs that end may wake the conversation.
    waking: AtomicBool,
    /// Told when a run starts and when it ends.
    observer: Box<dyn Fn(Background) + Send + Sync>,
}

impl Inner {
    /// Locks the state, going on with it when a panic poisoned it.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records that the run `id` ended with `outcome`, tells the observer and wakes a waiter; a run that was already taken out is ignored.
    ///
    /// A run the agent cancelled is kept for listing only: it is neither reported by `wait` nor wakes the conversation.
    fn finish(&self, id: &str, outcome: Result<String, String>) {
        let mut state = self.lock();
        let Some(run) = state.running.remove(id) else {
            return;
        };
        let (ok, text) = match outcome {
            Ok(text) => (true, text),
            Err(text) => (false, text),
        };
        let status = match (run.cancelled, ok) {
            (true, _) => Status::Cancelled,
            (false, true) => Status::Completed,
            (false, false) => Status::Failed,
        };
        let wakes = run.wakes && !run.cancelled && self.waking.load(Ordering::Relaxed);
        state.ended.push_back(Ended {
            id: id.to_owned(),
            label: run.label.clone(),
            kind: run.kind,
            status,
            took: run.started.elapsed(),
            feed: run.feed,
            text: text.clone(),
        });
        if state.ended.len() > MAX_ENDED {
            state.ended.pop_front();
        }
        if !run.cancelled {
            state.done.push_back(Unreported {
                finished: Finished {
                    id: id.to_owned(),
                    label: run.label.clone(),
                    ok,
                    text,
                },
                wakes,
            });
            if state.done.len() > MAX_UNREPORTED {
                state.done.pop_front();
            }
        }
        (self.observer)(Background {
            id: id.to_owned(),
            label: run.label,
            running: false,
            call: run.call,
        });
        drop(state);
        self.wake.notify_one();
        if wakes {
            self.signal.notify_one();
        }
    }
}

/// Makes sure a run is recorded as ended even when its future panics or is dropped before it first ran.
struct Settle {
    /// What the run reports to.
    inner: Arc<Inner>,
    /// The run's handle.
    id: String,
    /// Whether the run reported its outcome.
    settled: bool,
}

impl Drop for Settle {
    /// Reports a failure when the run ended without an outcome.
    fn drop(&mut self) {
        if !self.settled {
            self.inner
                .finish(&self.id, Err("the run ended unexpectedly".to_owned()));
        }
    }
}

/// The background runs of a conversation.
///
/// Runs belong to the conversation and not to the turn that started them: they go on when the turn is cancelled, and every one still running is stopped when the conversation ends or the value is dropped.
pub struct Jobs {
    /// What the runs share.
    inner: Arc<Inner>,
}

impl Jobs {
    /// Creates a conversation's runs; `observer` is told when a run starts and when it ends. Runs that end may wake the conversation until [`Jobs::set_waking`] turns that off.
    pub fn new(observer: impl Fn(Background) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                wake: Notify::new(),
                signal: Notify::new(),
                waking: AtomicBool::new(true),
                observer: Box::new(observer),
            }),
        }
    }

    /// Starts `work` in the background under a new handle, which is returned at once, as a [`Kind::Command`] with no output to read.
    ///
    /// `label` says what the run is and `call` is the call that started it. The work ends with its output, or with the reason it failed.
    pub fn spawn(
        &self,
        label: String,
        call: Option<String>,
        work: impl Future<Output = Result<String, String>> + Send + 'static,
    ) -> String {
        self.spawn_with(Launch::new(label, call, Kind::Command), work)
    }

    /// Starts `work` in the background as `launch` describes it, under a new handle, which is returned at once.
    ///
    /// The work ends with its output, or with the reason it failed.
    pub fn spawn_with(
        &self,
        launch: Launch,
        work: impl Future<Output = Result<String, String>> + Send + 'static,
    ) -> String {
        let mut state = self.inner.lock();
        state.next += 1;
        let id = format!("bg-{}", state.next);
        let inner = self.inner.clone();
        let own = id.clone();
        let mut settle = Settle {
            inner,
            id: own.clone(),
            settled: false,
        };
        let task = tokio::spawn(async move {
            let outcome = work.await;
            settle.settled = true;
            settle.inner.finish(&own, outcome);
        });
        (self.inner.observer)(Background {
            id: id.clone(),
            label: launch.label.clone(),
            running: true,
            call: launch.call.clone(),
        });
        state.running.insert(
            id.clone(),
            Running {
                label: launch.label,
                call: launch.call,
                kind: launch.kind,
                started: Instant::now(),
                handle: task.abort_handle(),
                feed: launch.feed,
                stop: launch.stop,
                cancelled: false,
                wakes: true,
            },
        );
        id
    }

    /// Waits until a run ends and returns the one that ended first of those not yet reported; a run that ended earlier is returned at once.
    ///
    /// Dropping the future, as a cancelled turn does, takes nothing away.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] at once when nothing is running and nothing finished is waiting to be reported.
    pub async fn wait(&self) -> Result<Finished, ToolError> {
        loop {
            {
                let mut state = self.inner.lock();
                if let Some(unreported) = state.done.pop_front() {
                    return Ok(unreported.finished);
                }
                if state.running.values().all(|run| run.cancelled) {
                    return Err(ToolError::Failed(
                        "nothing is running in the background".to_owned(),
                    ));
                }
            }
            self.inner.wake.notified().await;
        }
    }

    /// Every run, those still running first, each group oldest first.
    pub fn list(&self) -> Vec<Listed> {
        let state = self.inner.lock();
        let mut running: Vec<(&String, &Running)> = state.running.iter().collect();
        running.sort_by_key(|(_, run)| run.started);
        let mut listed: Vec<Listed> = running
            .into_iter()
            .map(|(id, run)| Listed {
                id: id.clone(),
                label: run.label.clone(),
                kind: run.kind,
                status: if run.cancelled {
                    Status::Cancelled
                } else {
                    Status::Running
                },
                elapsed: run.started.elapsed(),
            })
            .collect();
        listed.extend(state.ended.iter().map(|ended| Listed {
            id: ended.id.clone(),
            label: ended.label.clone(),
            kind: ended.kind,
            status: ended.status,
            elapsed: ended.took,
        }));
        listed
    }

    /// What the run `id` printed so far, as `look` asks, under a line saying how it stands; a subagent that still runs has nothing to show until it ends, and one that ended shows its result.
    ///
    /// Reading marks what the run printed as read, for the next read that asks for only what is new.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no run `id`, or it ended too long ago to be kept.
    pub fn output(&self, id: &str, look: &Look) -> Result<String, ToolError> {
        let (heading, feed, text) = {
            let state = self.inner.lock();
            if let Some(run) = state.running.get(id) {
                let status = if run.cancelled {
                    Status::Cancelled
                } else {
                    Status::Running
                };
                (
                    heading(id, run.kind, status, run.started.elapsed(), &run.label),
                    run.feed.clone(),
                    None,
                )
            } else if let Some(ended) = state.ended.iter().find(|ended| ended.id == id) {
                (
                    heading(id, ended.kind, ended.status, ended.took, &ended.label),
                    ended.feed.clone(),
                    Some(ended.text.clone()),
                )
            } else {
                return Err(unknown(id));
            }
        };
        let (body, forgotten) = match (feed, text) {
            (Some(feed), _) => feed.take(look.fresh),
            (None, Some(text)) => (text, false),
            (None, None) => {
                return Ok(format!(
                    "{heading}\nA subagent's output comes with its result when it ends."
                ));
            }
        };
        let mut shown = shown(&body, look);
        if forgotten {
            shown = format!("[older output was not kept]\n{shown}");
        }
        Ok(format!("{heading}\n{shown}"))
    }

    /// Cancels the run `id`: stops it through its own means when it has them, and otherwise, or when they stop nothing, drops its work, which kills a command with its whole process group. Its end is not reported by `wait` and wakes no one.
    ///
    /// Returns what the model reads: the run, and for a command the end of what it printed.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no run `id`, it already ended, or it is already being cancelled.
    pub fn cancel(&self, id: &str) -> Result<String, ToolError> {
        let (stop, handle, feed, kind, started, label) = {
            let mut state = self.inner.lock();
            let ended = state.ended.iter().any(|ended| ended.id == id);
            match state.running.get_mut(id) {
                Some(run) if run.cancelled => {
                    return Err(ToolError::Failed(format!(
                        "{id} is already being cancelled"
                    )));
                }
                Some(run) => {
                    run.cancelled = true;
                    run.wakes = false;
                    (
                        run.stop.clone(),
                        run.handle.clone(),
                        run.feed.clone(),
                        run.kind,
                        run.started,
                        run.label.clone(),
                    )
                }
                None if ended => {
                    return Err(ToolError::Failed(format!(
                        "{id} already ended; `wait` reports how"
                    )));
                }
                None => return Err(unknown(id)),
            }
        };
        if !stop.is_some_and(|stop| stop()) {
            handle.abort();
        }
        self.inner.wake.notify_one();
        let heading = heading(id, kind, Status::Cancelled, started.elapsed(), &label);
        Ok(match feed {
            Some(feed) => format!(
                "{heading}\nKilled with the processes it started. What it printed last:\n{}",
                shown(&feed.take(false).0, &Look::default())
            ),
            None => format!("{heading}\nStopped; `wait` will not report it."),
        })
    }

    /// Keeps every run started so far, and every one that ended unreported, from waking the conversation, as when the reader cancelled the conversation's work; `wait` still reports them.
    pub fn silence(&self) {
        let mut state = self.inner.lock();
        for run in state.running.values_mut() {
            run.wakes = false;
        }
        for unreported in &mut state.done {
            unreported.wakes = false;
        }
    }

    /// Takes the runs that ended unreported and may wake the conversation, oldest first, so that `wait` no longer reports them; nothing when waking is off.
    pub fn take_wakes(&self) -> Vec<Finished> {
        if !self.waking() {
            return Vec::new();
        }
        let mut state = self.inner.lock();
        let (woken, kept): (Vec<Unreported>, Vec<Unreported>) = state
            .done
            .drain(..)
            .partition(|unreported| unreported.wakes);
        state.done = kept.into();
        woken
            .into_iter()
            .map(|unreported| unreported.finished)
            .collect()
    }

    /// Waits until a run ends that may wake the conversation; an end that came while nobody waited is returned at once.
    pub async fn ended(&self) {
        self.inner.signal.notified().await;
    }

    /// Whether runs that end may wake the conversation.
    pub fn waking(&self) -> bool {
        self.inner.waking.load(Ordering::Relaxed)
    }

    /// Lets runs that end wake the conversation when `on`, and keeps them from it otherwise.
    pub fn set_waking(&self, on: bool) {
        self.inner.waking.store(on, Ordering::Relaxed);
    }

    /// Stops every run that is still going and forgets the runs that ended.
    pub fn stop_all(&self) {
        let stopped: Vec<(String, Running)> = {
            let mut state = self.inner.lock();
            state.done.clear();
            state.ended.clear();
            state.running.drain().collect()
        };
        for (id, run) in stopped {
            run.handle.abort();
            (self.inner.observer)(Background {
                id,
                label: run.label,
                running: false,
                call: run.call,
            });
        }
    }
}

impl Drop for Jobs {
    /// Stops what is still running.
    fn drop(&mut self) {
        self.stop_all();
    }
}

/// The error for a handle that names no run.
fn unknown(id: &str) -> ToolError {
    ToolError::Failed(format!(
        "no background run {id}; `wait` with `action` `list` shows the runs"
    ))
}

/// The line that says which run is shown and how it stands.
fn heading(id: &str, kind: Kind, status: Status, elapsed: Duration, label: &str) -> String {
    Listed {
        id: id.to_owned(),
        label: label.to_owned(),
        kind,
        status,
        elapsed,
    }
    .line()
}

/// `elapsed` in a few characters: seconds, minutes and seconds, or hours and minutes.
fn span(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m{:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h{:02}m", seconds / 3600, seconds % 3600 / 60),
    }
}

/// The lines of `text` that `look` asks for: those its pattern matches, the last of them only, at most [`MAX_READ_BYTES`] of their end, with a note of what was left out.
fn shown(text: &str, look: &Look) -> String {
    let matching: Vec<&str> = text
        .lines()
        .filter(|line| {
            look.pattern
                .as_ref()
                .is_none_or(|pattern| pattern.is_match(line))
        })
        .collect();
    if matching.is_empty() {
        return match (&look.pattern, look.fresh) {
            (Some(_), _) => "(no line matches)".to_owned(),
            (None, true) => "(nothing new)".to_owned(),
            (None, false) => "(no output yet)".to_owned(),
        };
    }
    let keep = look.lines.clamp(1, MAX_LINES);
    let from = matching.len().saturating_sub(keep);
    let (body, cut) = cap_tail(&matching[from..].join("\n"), MAX_READ_BYTES);
    if from == 0 && !cut {
        return body;
    }
    let what = if look.pattern.is_some() {
        "matching lines"
    } else {
        "lines"
    };
    let note = if cut {
        format!(
            "[showing the last {MAX_READ_BYTES} bytes of the last {} {what} of {}]",
            matching.len() - from,
            matching.len()
        )
    } else {
        format!(
            "[showing the last {} of {} {what}]",
            matching.len() - from,
            matching.len()
        )
    };
    format!("{note}\n{body}")
}

/// The end of `text`, at most `most` bytes of it cut at a character boundary, with whether anything was cut.
fn cap_tail(text: &str, most: usize) -> (String, bool) {
    if text.len() <= most {
        return (text.to_owned(), false);
    }
    let mut start = text.len() - most;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    (text[start..].to_owned(), true)
}

#[cfg(test)]
/// Tests for background runs.
mod tests {
    use super::*;

    /// A set of runs and the notices its observer received.
    fn jobs() -> (Jobs, Arc<Mutex<Vec<Background>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (Jobs::new(move |b| sink.lock().unwrap().push(b)), seen)
    }

    /// A run that ends after `millis` with `text`.
    async fn after(millis: u64, text: &'static str) -> Result<String, String> {
        tokio::time::sleep(Duration::from_millis(millis)).await;
        Ok(text.to_owned())
    }

    /// Records in a flag that it was dropped.
    struct Flag(Arc<Mutex<bool>>);

    impl Drop for Flag {
        /// Records that the future holding the flag was dropped.
        fn drop(&mut self) {
            *self.0.lock().unwrap() = true;
        }
    }

    /// A run that never ends on its own and sets `dropped` when its work is dropped.
    fn forever(dropped: &Arc<Mutex<bool>>) -> impl Future<Output = Result<String, String>> + use<> {
        let flag = Flag(dropped.clone());
        async move {
            let _flag = flag;
            std::future::pending::<Result<String, String>>().await
        }
    }

    /// Two overlapping runs are reported in the order they end, and each is reported once.
    #[tokio::test]
    async fn overlapping_runs_finish_in_completion_order() {
        let (jobs, _) = jobs();
        let slow = jobs.spawn("slow".into(), None, after(300, "one"));
        let fast = jobs.spawn("fast".into(), None, after(50, "two"));
        let first = jobs.wait().await.unwrap();
        assert_eq!(
            (first.id.as_str(), first.text.as_str()),
            (fast.as_str(), "two")
        );
        let second = jobs.wait().await.unwrap();
        assert_eq!(
            (second.id.as_str(), second.text.as_str()),
            (slow.as_str(), "one")
        );
        assert!(jobs.wait().await.is_err());
    }

    /// With nothing running and nothing finished, waiting fails at once.
    #[tokio::test]
    async fn waiting_with_nothing_running_is_an_error() {
        let (jobs, _) = jobs();
        let result = tokio::time::timeout(Duration::from_secs(1), jobs.wait()).await;
        assert!(matches!(result, Ok(Err(ToolError::Failed(_)))));
    }

    /// A run that ended before anyone waited is reported by the next wait, with its failure status.
    #[tokio::test]
    async fn a_run_that_ended_earlier_is_reported_later() {
        let (jobs, _) = jobs();
        jobs.spawn("bad".into(), None, async { Err("boom".to_owned()) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let finished = jobs.wait().await.unwrap();
        assert!(!finished.ok);
        assert!(finished.report().contains("failed: bad\nboom"));
    }

    /// The observer hears of each run when it starts and when it ends, with the call that started it.
    #[tokio::test]
    async fn the_observer_hears_of_starts_and_ends() {
        let (jobs, seen) = jobs();
        let id = jobs.spawn("work".into(), Some("c1".into()), after(10, "x"));
        jobs.wait().await.unwrap();
        let seen = seen.lock().unwrap();
        let expected = |running| Background {
            id: id.clone(),
            label: "work".into(),
            running,
            call: Some("c1".into()),
        };
        assert_eq!(*seen, vec![expected(true), expected(false)]);
    }

    /// A wait that is dropped, as a cancelled turn drops it, leaves the runs going and reportable.
    #[tokio::test]
    async fn a_cancelled_wait_leaves_the_runs_going() {
        let (jobs, _) = jobs();
        jobs.spawn("work".into(), None, after(100, "late"));
        let cancelled = tokio::time::timeout(Duration::from_millis(10), jobs.wait()).await;
        assert!(cancelled.is_err());
        assert_eq!(jobs.wait().await.unwrap().text, "late");
    }

    /// Dropping the runs stops what is still going.
    #[tokio::test]
    async fn dropping_stops_the_runs() {
        let (jobs, seen) = jobs();
        let dropped = Arc::new(Mutex::new(false));
        jobs.spawn("forever".into(), None, forever(&dropped));
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(jobs);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(*dropped.lock().unwrap());
        assert!(!seen.lock().unwrap().last().unwrap().running);
    }

    /// Output read so far shows the latest lines, and a fresh read shows only what came since the last read.
    #[tokio::test]
    async fn output_so_far_and_what_is_new() {
        let (jobs, _) = jobs();
        let feed = Feed::new();
        let dropped = Arc::new(Mutex::new(false));
        let id = jobs.spawn_with(
            Launch::new("server".into(), None, Kind::Command).fed_by(feed.clone()),
            forever(&dropped),
        );
        let fresh = Look {
            fresh: true,
            ..Look::default()
        };
        assert!(
            jobs.output(&id, &fresh)
                .unwrap()
                .ends_with("\n(nothing new)")
        );
        feed.push(b"one\ntwo\n");
        let text = jobs.output(&id, &Look::default()).unwrap();
        assert!(text.starts_with(&format!("{id} command running ")));
        assert!(text.ends_with(": server\none\ntwo"));
        feed.push(b"three\n");
        assert!(
            jobs.output(&id, &fresh)
                .unwrap()
                .ends_with(": server\nthree")
        );
        assert!(jobs.output(&id, &fresh).unwrap().ends_with("(nothing new)"));
        assert!(
            jobs.output(&id, &Look::default())
                .unwrap()
                .ends_with("one\ntwo\nthree")
        );
        assert!(matches!(
            jobs.output("bg-9", &Look::default()),
            Err(ToolError::Failed(_))
        ));
    }

    /// A pattern keeps only the lines it matches.
    #[tokio::test]
    async fn a_pattern_keeps_matching_lines() {
        let (jobs, _) = jobs();
        let feed = Feed::new();
        let dropped = Arc::new(Mutex::new(false));
        let id = jobs.spawn_with(
            Launch::new("tests".into(), None, Kind::Command).fed_by(feed.clone()),
            forever(&dropped),
        );
        feed.push(b"ok a\nFAIL b\nok c\nFAIL d\n");
        let look = Look {
            pattern: Some(Regex::new("^FAIL").unwrap()),
            ..Look::default()
        };
        assert!(
            jobs.output(&id, &look)
                .unwrap()
                .ends_with(": tests\nFAIL b\nFAIL d")
        );
        let none = Look {
            pattern: Some(Regex::new("panic").unwrap()),
            ..Look::default()
        };
        assert!(
            jobs.output(&id, &none)
                .unwrap()
                .ends_with("(no line matches)")
        );
    }

    /// A read shows only the last lines asked for, no more than the most allowed, and no more than the most bytes, saying what it left out; a feed keeps only its latest bytes.
    #[tokio::test]
    async fn reads_are_bounded() {
        let (jobs, _) = jobs();
        let feed = Feed::new();
        let dropped = Arc::new(Mutex::new(false));
        let id = jobs.spawn_with(
            Launch::new("loud".into(), None, Kind::Command).fed_by(feed.clone()),
            forever(&dropped),
        );
        let many: String = (0..1000).map(|n| format!("line {n}\n")).collect();
        feed.push(many.as_bytes());
        let text = jobs.output(&id, &Look::default()).unwrap();
        assert!(text.contains("[showing the last 50 of 1000 lines]\nline 950\n"));
        assert!(text.ends_with("line 999"));
        let three = Look {
            lines: 3,
            ..Look::default()
        };
        assert!(
            jobs.output(&id, &three)
                .unwrap()
                .ends_with("[showing the last 3 of 1000 lines]\nline 997\nline 998\nline 999")
        );
        let huge = Look {
            lines: 100_000,
            ..Look::default()
        };
        assert!(
            jobs.output(&id, &huge)
                .unwrap()
                .contains("[showing the last 500 of 1000 lines]")
        );
        feed.push(&vec![b'x'; 40_000]);
        let text = jobs.output(&id, &Look::default()).unwrap();
        assert!(text.contains(&format!("[showing the last {MAX_READ_BYTES} bytes")));
        assert!(text.len() < MAX_READ_BYTES + 200);
        feed.push(&vec![b'y'; FEED_KEEP]);
        let fresh = Look {
            fresh: true,
            ..Look::default()
        };
        feed.push(b"\nlast\n");
        let text = jobs.output(&id, &fresh).unwrap();
        assert!(text.contains("[older output was not kept]"));
        assert!(text.ends_with("last"));
    }

    /// Cancelling a run drops its work, keeps it out of `wait` and lists it as cancelled; cancelling it again, or a run that is not there, fails.
    #[tokio::test]
    async fn cancelling_drops_the_work() {
        let (jobs, seen) = jobs();
        let dropped = Arc::new(Mutex::new(false));
        let id = jobs.spawn("forever".into(), None, forever(&dropped));
        let said = jobs.cancel(&id).unwrap();
        assert!(said.starts_with(&format!("{id} command cancelled ")));
        assert!(matches!(jobs.cancel(&id), Err(ToolError::Failed(_))));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(*dropped.lock().unwrap());
        assert!(!seen.lock().unwrap().last().unwrap().running);
        assert!(jobs.wait().await.is_err());
        assert_eq!(jobs.list()[0].status, Status::Cancelled);
        assert!(matches!(jobs.cancel(&id), Err(ToolError::Failed(_))));
        assert!(matches!(jobs.cancel("bg-9"), Err(ToolError::Failed(_))));
        assert!(jobs.take_wakes().is_empty());
    }

    /// A run with a way of its own to stop is stopped that way, not dropped, and its end is not reported.
    #[tokio::test]
    async fn cancelling_uses_the_runs_own_stop() {
        let (jobs, _) = jobs();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let tx = Arc::new(Mutex::new(Some(tx)));
        let stop: Stop = Arc::new(move || {
            tx.lock()
                .unwrap()
                .take()
                .is_some_and(|tx| tx.send(()).is_ok())
        });
        let id = jobs.spawn_with(
            Launch::new("helper".into(), None, Kind::Subagent).stopped_by(stop),
            async move {
                let _ = rx.await;
                Err("stopped".to_owned())
            },
        );
        let said = jobs.cancel(&id).unwrap();
        assert!(said.contains("subagent cancelled"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(jobs.wait().await.is_err());
        let listed = jobs.list();
        assert_eq!(
            (listed[0].status, listed[0].kind),
            (Status::Cancelled, Kind::Subagent)
        );
        assert!(
            jobs.output(&id, &Look::default())
                .unwrap()
                .ends_with("\nstopped")
        );
    }

    /// The list shows running runs first, then ended ones, each with its kind, status and time.
    #[tokio::test]
    async fn listing_shows_every_run() {
        let (jobs, _) = jobs();
        let dropped = Arc::new(Mutex::new(false));
        let done = jobs.spawn("quick".into(), None, after(0, "x"));
        jobs.wait().await.unwrap();
        let going = jobs.spawn_with(
            Launch::new("helper".into(), None, Kind::Subagent),
            forever(&dropped),
        );
        let lines: Vec<String> = jobs.list().iter().map(Listed::line).collect();
        assert_eq!(
            lines,
            vec![
                format!("{going} subagent running 0s: helper"),
                format!("{done} command completed 0s: quick"),
            ]
        );
        assert!(
            jobs.output(&going, &Look::default())
                .unwrap()
                .ends_with("comes with its result when it ends.")
        );
    }

    /// A run that ends unreported signals the conversation once and is taken to wake it, after which `wait` no longer reports it; silenced runs and runs ended while waking is off stay for `wait` only.
    #[tokio::test]
    async fn ends_wake_unless_silenced_or_off() {
        let (jobs, _) = jobs();
        jobs.spawn("a".into(), None, after(10, "x"));
        tokio::time::timeout(Duration::from_secs(1), jobs.ended())
            .await
            .unwrap();
        let woken = jobs.take_wakes();
        assert_eq!(woken.len(), 1);
        assert!(woken[0].brief().starts_with("bg-1 completed: a\nx"));
        assert!(jobs.take_wakes().is_empty());
        assert!(jobs.wait().await.is_err());

        jobs.spawn("b".into(), None, after(10, "y"));
        jobs.silence();
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(jobs.take_wakes().is_empty());
        assert_eq!(jobs.wait().await.unwrap().text, "y");

        jobs.set_waking(false);
        jobs.spawn("c".into(), None, after(10, "z"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        jobs.set_waking(true);
        assert!(jobs.take_wakes().is_empty());
        assert!(
            tokio::time::timeout(Duration::from_millis(50), jobs.ended())
                .await
                .is_err()
        );
        assert_eq!(jobs.wait().await.unwrap().text, "z");
    }

    /// The brief report keeps the first line and the last lines of long output.
    #[test]
    fn the_brief_report_keeps_the_ends() {
        let text: Vec<String> = (0..100).map(|n| format!("l{n}")).collect();
        let finished = Finished {
            id: "bg-1".into(),
            label: "make".into(),
            ok: true,
            text: text.join("\n"),
        };
        let brief = finished.brief();
        assert!(brief.starts_with("bg-1 completed: make\nl0\n[… 79 lines left out;"));
        assert!(brief.ends_with("l80\nl81\nl82\nl83\nl84\nl85\nl86\nl87\nl88\nl89\nl90\nl91\nl92\nl93\nl94\nl95\nl96\nl97\nl98\nl99"));
    }
}
