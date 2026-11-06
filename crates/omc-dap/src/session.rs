//! One debugging session: an adapter started or connected to, the program it debugs, its breakpoints, and the actions the model takes on it.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use omc_tools::Sandbox;
use serde_json::{Map, Value, json};
use tokio::time::Instant;

use crate::adapters::{Adapter, Entry, Transport, configured, language_of, normalize_language};
use crate::client::{Client, kill_process};
use crate::state::Stop;
use crate::summary::{
    clip, excerpt, frame_path, frames, output_block, place, reference_of, stop_heading,
    thread_name, threads, variable_line,
};

/// How long an adapter may take to answer the request that starts it.
const INITIALIZE_WAIT: Duration = Duration::from_secs(20);

/// How long an adapter may take to say it is ready for breakpoints once the program is being launched.
const READY_WAIT: Duration = Duration::from_secs(30);

/// How long launching or attaching may take, which for some adapters includes building the program.
const LAUNCH_WAIT: Duration = Duration::from_secs(120);

/// How long an adapter listening on a port may take to start listening.
const LISTEN_WAIT: Duration = Duration::from_secs(10);

/// How long any other request may take.
const REQUEST_WAIT: Duration = Duration::from_secs(10);

/// How long asking about an exception may take, since the stop is reported without it.
const DETAIL_WAIT: Duration = Duration::from_secs(3);

/// How many frames a stack shows unless asked for more.
const STACK_FRAMES: usize = 20;

/// How many frames a report of a stop shows.
const STOP_FRAMES: usize = 6;

/// How many of the top frame's variables a report of a stop shows.
const STOP_LOCALS: usize = 15;

/// How many lines of new output a report shows.
const REPORT_OUTPUT: usize = 20;

/// How many lines of output the `output` action shows.
const OUTPUT_LINES: usize = 400;

/// The most lines a listing of variables shows, however deep and wide it is asked to go.
const VARIABLE_LINES: usize = 400;

/// A breakpoint asked for at a line of a file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LineBreakpoint {
    /// The line asked for.
    pub(crate) line: i64,
    /// The condition under which it stops, in the program's language.
    pub(crate) condition: Option<String>,
    /// How many hits it takes before it stops, as the adapter understands it, such as `3` or `>= 3`.
    pub(crate) hit: Option<String>,
    /// The message logged instead of stopping, with expressions in braces.
    pub(crate) log: Option<String>,
    /// The id the adapter gave it.
    pub(crate) id: Option<i64>,
    /// Whether the adapter could place it.
    pub(crate) verified: bool,
    /// The line the adapter placed it on, which may differ from the one asked for.
    pub(crate) placed: Option<i64>,
    /// Why the adapter could not place it.
    pub(crate) message: Option<String>,
}

/// A breakpoint asked for on a function.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct FunctionBreakpoint {
    /// The function's name.
    pub(crate) name: String,
    /// The condition under which it stops.
    pub(crate) condition: Option<String>,
    /// How many hits it takes before it stops.
    pub(crate) hit: Option<String>,
    /// The id the adapter gave it.
    pub(crate) id: Option<i64>,
    /// Whether the adapter could place it.
    pub(crate) verified: bool,
    /// Why the adapter could not place it.
    pub(crate) message: Option<String>,
}

/// A breakpoint to set: at a line of a file, or on a function.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Spot {
    /// At a line of the file, resolved inside the working directory.
    Line(PathBuf, LineBreakpoint),
    /// On a function.
    Function(FunctionBreakpoint),
}

impl Spot {
    /// A breakpoint at `line` of `file`.
    pub(crate) fn line(file: PathBuf, line: i64) -> Self {
        Self::Line(
            file,
            LineBreakpoint {
                line,
                ..LineBreakpoint::default()
            },
        )
    }

    /// A breakpoint on the function `name`.
    pub(crate) fn function(name: &str) -> Self {
        Self::Function(FunctionBreakpoint {
            name: name.to_owned(),
            ..FunctionBreakpoint::default()
        })
    }
}

/// What starts a session: launching a program, or attaching to a process or to an adapter listening at an address.
#[derive(Clone, Debug, Default)]
pub(crate) struct Start {
    /// The program, resolved inside the working directory; nothing when the settings name what to run.
    pub(crate) program: Option<PathBuf>,
    /// Its arguments.
    pub(crate) args: Vec<String>,
    /// The folder it runs in.
    pub(crate) cwd: PathBuf,
    /// Variables added to its environment.
    pub(crate) env: Map<String, Value>,
    /// The process to attach to.
    pub(crate) pid: Option<u32>,
    /// The `host:port` of an adapter to connect to instead of starting one.
    pub(crate) address: Option<String>,
    /// The language, when the model named it.
    pub(crate) language: Option<String>,
    /// The adapter, when the model named it.
    pub(crate) adapter: Option<String>,
    /// Settings added to the launch or attach request, over the adapter's own.
    pub(crate) config: Map<String, Value>,
    /// Whether the program stops as it starts.
    pub(crate) stop_on_entry: bool,
    /// The breakpoints set before the program runs.
    pub(crate) breakpoints: Vec<Spot>,
    /// The exception filters to set, when the model named them.
    pub(crate) exceptions: Option<Vec<String>>,
    /// How long to wait for the program to stop or end once it runs.
    pub(crate) timeout: Duration,
}

impl Start {
    /// Whether this attaches rather than launches.
    fn attaches(&self) -> bool {
        self.pid.is_some() || self.address.is_some()
    }
}

/// The frames of a stopped thread, as last asked for.
struct Frames {
    /// How many times the program had stopped when they were asked for, which tells whether they still hold.
    stops: u64,
    /// The thread.
    thread: i64,
    /// The frames, innermost first.
    list: Vec<Value>,
}

/// A step or a run of the program.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Motion {
    /// Run until something stops it.
    Continue,
    /// Run to the next line, over calls.
    Over,
    /// Run into the call on this line.
    In,
    /// Run until the current function returns.
    Out,
}

impl Motion {
    /// The request that makes the motion.
    fn command(self) -> &'static str {
        match self {
            Self::Continue => "continue",
            Self::Over => "next",
            Self::In => "stepIn",
            Self::Out => "stepOut",
        }
    }
}

/// A debugging session.
pub(crate) struct Session {
    /// The handle the model calls it by.
    pub(crate) id: String,
    /// The adapter's id.
    pub(crate) adapter: String,
    /// What is debugged, for listings.
    pub(crate) label: String,
    /// The connection to the adapter.
    client: Client,
    /// What the adapter said it can do.
    capabilities: Value,
    /// Whether the program was launched by the session, rather than attached to, so that it is ended with it.
    launched: bool,
    /// The working directory, for paths shown and source read.
    sandbox: Sandbox,
    /// The breakpoints at lines, by file.
    lines: BTreeMap<PathBuf, Vec<LineBreakpoint>>,
    /// The breakpoints on functions.
    functions: Vec<FunctionBreakpoint>,
    /// The exception filters set.
    exceptions: Vec<String>,
    /// The frames last asked for.
    frames: Option<Frames>,
    /// How many stops the model has been told of.
    reported_stops: u64,
    /// Whether the model has been told the program ended.
    reported_end: bool,
}

impl Drop for Session {
    /// Kills a program the session launched that has not ended, in case its adapter leaves it behind; the adapter goes when its connection is dropped.
    fn drop(&mut self) {
        if !self.launched {
            return;
        }
        let state = self.client.state();
        if state.exit_code.is_none()
            && let Some(pid) = state.pid
        {
            kill_process(pid);
        }
    }
}

impl Session {
    /// Starts the session `id` as `start` says, in the working directory of `sandbox`, and reports how it began: where the program stopped, that it ended, or that it is running.
    ///
    /// # Errors
    ///
    /// Returns why when no adapter can be found or started, or it refuses to launch or attach.
    pub(crate) async fn start(
        id: String,
        sandbox: Sandbox,
        start: Start,
    ) -> Result<(Self, String), String> {
        let (adapter, client) = connect(&start, sandbox.root()).await?;
        Self::over(id, sandbox, adapter, client, start).await
    }

    /// Starts the session `id` like [`Session::start`], with `client` speaking with `adapter`, or with an adapter of unknown kind.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter refuses to launch or attach.
    pub(crate) async fn over(
        id: String,
        sandbox: Sandbox,
        adapter: Option<Adapter>,
        client: Client,
        start: Start,
    ) -> Result<(Self, String), String> {
        let label = match (&start.program, start.pid, &start.address) {
            (_, Some(pid), _) => format!("process {pid}"),
            (_, _, Some(address)) => format!("adapter at {address}"),
            (Some(program), ..) => sandbox.display(program),
            (None, ..) => "configured program".to_owned(),
        };
        let mut session = Self {
            id,
            adapter: adapter
                .as_ref()
                .map_or_else(|| "remote".to_owned(), |a| a.id.clone()),
            label,
            client,
            capabilities: Value::Null,
            launched: !start.attaches(),
            sandbox,
            lines: BTreeMap::new(),
            functions: Vec::new(),
            exceptions: Vec::new(),
            frames: None,
            reported_stops: 0,
            reported_end: false,
        };
        let report = session.begin(adapter.as_ref(), start).await?;
        Ok((session, report))
    }

    /// Runs the start of the protocol: introduces the client, launches or attaches, sets the breakpoints once the adapter is ready, and waits for the program to stop or end.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter fails a step.
    async fn begin(&mut self, adapter: Option<&Adapter>, start: Start) -> Result<String, String> {
        let adapter_id = adapter.map_or("remote", |adapter| adapter.id.as_str());
        self.capabilities = self
            .client
            .request(
                "initialize",
                json!({
                    "clientID": "crab",
                    "clientName": "crab",
                    "adapterID": adapter_id,
                    "locale": "en",
                    "linesStartAt1": true,
                    "columnsStartAt1": true,
                    "pathFormat": "path",
                    "supportsVariableType": true,
                    "supportsVariablePaging": false,
                    "supportsRunInTerminalRequest": false,
                    "supportsMemoryReferences": false,
                    "supportsProgressReporting": false,
                    "supportsInvalidatedEvent": false,
                    "supportsStartDebuggingRequest": false,
                }),
                INITIALIZE_WAIT,
            )
            .await
            .map_err(|reason| self.with_log(format!("{adapter_id} did not start: {reason}")))?;
        let mut breakpoints = start.breakpoints.clone();
        let mut arguments = request_arguments(adapter, &start);
        if start.stop_on_entry {
            match adapter.map(|adapter| &adapter.entry) {
                Some(Entry::Flag(flag)) => {
                    arguments.insert(flag.clone(), Value::Bool(true));
                }
                Some(Entry::Function(name)) => breakpoints.push(Spot::function(name)),
                None => {
                    arguments.insert("stopOnEntry".to_owned(), Value::Bool(true));
                }
            }
        }
        let command = if start.attaches() { "attach" } else { "launch" };
        {
            let mut state = self.client.state();
            state.listening = true;
            let _ = state.console.unread();
        }
        let mut sent = self.client.send(command, Value::Object(arguments)).await?;
        let mut answered = false;
        let deadline = Instant::now() + READY_WAIT;
        tokio::select! {
            _ = self.client.wait_until(deadline, |state| state.initialized) => {}
            answer = self.client.answer(&mut sent, READY_WAIT) => {
                answered = true;
                answer.map_err(|reason| self.with_log(format!("{command} failed: {reason}")))?;
                let soon = Instant::now() + Duration::from_secs(2);
                self.client.wait_until(soon, |state| state.initialized).await;
            }
        }
        if self.client.state().gone {
            return Err(self.with_log(format!("{adapter_id} stopped while starting")));
        }
        let mut notes = Vec::new();
        for spot in breakpoints {
            if let Err(reason) = self.add(spot).await {
                notes.push(format!("A breakpoint was not set: {reason}"));
            }
        }
        let filters = start
            .exceptions
            .clone()
            .unwrap_or_else(|| self.default_filters());
        if (!filters.is_empty() || start.exceptions.is_some())
            && let Err(reason) = self.set_exceptions(filters).await
        {
            notes.push(format!("Exception breakpoints were not set: {reason}"));
        }
        if self.capabilities["supportsConfigurationDoneRequest"]
            .as_bool()
            .unwrap_or(false)
        {
            self.client
                .request("configurationDone", json!({}), REQUEST_WAIT)
                .await
                .map_err(|reason| self.with_log(format!("configuration failed: {reason}")))?;
        }
        if !answered {
            self.client
                .answer(&mut sent, LAUNCH_WAIT)
                .await
                .map_err(|reason| self.with_log(format!("{command} failed: {reason}")))?;
        }
        let mut report = vec![format!(
            "Session {} started: {} {} {}.",
            self.id,
            adapter_id,
            if start.attaches() {
                "attached to"
            } else {
                "debugging"
            },
            self.label
        )];
        report.extend(notes);
        if let Some(listing) = self.breakpoint_listing() {
            report.push(listing);
        }
        report.push(self.settle(0, start.timeout).await);
        Ok(report.join("\n"))
    }

    /// The exception filters the adapter turns on unless told otherwise.
    fn default_filters(&self) -> Vec<String> {
        self.capabilities["exceptionBreakpointFilters"]
            .as_array()
            .map(|filters| {
                filters
                    .iter()
                    .filter(|filter| filter["default"].as_bool().unwrap_or(false))
                    .filter_map(|filter| filter["filter"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `message` with the end of what the adapter printed on its own output, which often says why it failed.
    fn with_log(&self, message: String) -> String {
        let log = self.client.log();
        let log = log.trim();
        if log.is_empty() {
            message
        } else {
            let tail: Vec<&str> = log.lines().rev().take(15).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            format!("{message}\nThe adapter printed:\n{}", tail.join("\n"))
        }
    }

    /// What happened since the model was last told, as text to put before an action's own report: a stop or the end of the program it has not heard of.
    pub(crate) async fn notices(&mut self) -> Option<String> {
        let (stops, stopped, over) = {
            let state = self.client.state();
            (
                state.stops,
                state.stop.is_some(),
                state.over() || state.exit_code.is_some(),
            )
        };
        if stops > self.reported_stops && stopped {
            return Some(format!(
                "Since your last call the program stopped.\n{}",
                self.report_stop().await
            ));
        }
        if over && !self.reported_end {
            return Some(self.report_end());
        }
        None
    }

    /// Waits up to `wait` for the program to stop or end after it had stopped `since` times, and reports which, or that it is still running.
    async fn settle(&mut self, since: u64, wait: Duration) -> String {
        let deadline = Instant::now() + wait;
        let changed = self
            .client
            .wait_until(deadline, |state| {
                (state.stops > since && state.stop.is_some())
                    || state.over()
                    || state.exit_code.is_some()
            })
            .await;
        let (stopped, over) = {
            let state = self.client.state();
            (
                state.stop.is_some(),
                state.over() || state.exit_code.is_some(),
            )
        };
        if changed && stopped {
            return self.report_stop().await;
        }
        if over {
            if !over_fully(&self.client) {
                let soon = Instant::now() + Duration::from_millis(500);
                self.client
                    .wait_until(soon, |state| state.terminated || state.gone)
                    .await;
            }
            return self.report_end();
        }
        let mut report = format!(
            "Still running after {}s; nothing stopped it. Call debug with action wait to keep waiting, pause to interrupt it, or output to see what it printed.",
            wait.as_secs_f32()
        );
        let unread = self.client.state().output.unread();
        if let Some(block) = output_block("Program output", &unread, REPORT_OUTPUT) {
            report.push('\n');
            report.push_str(&block);
        }
        report
    }

    /// Reports where the program is stopped: why, the top frames with the source line, the top frame's local variables, and output it has not shown.
    async fn report_stop(&mut self) -> String {
        let (stop, stops) = {
            let state = self.client.state();
            (state.stop.clone().unwrap_or_default(), state.stops)
        };
        self.reported_stops = stops;
        let list = self.thread_list().await.unwrap_or_default();
        let thread = match stop.thread {
            Some(thread) => Some(thread),
            None => list.first().and_then(|thread| thread["id"].as_i64()),
        };
        let mut report = vec![stop_heading(
            &stop,
            &thread.map_or_else(|| "?".to_owned(), |id| thread_name(&list, id)),
        )];
        if stop.reason == "exception"
            && let Some(thread) = thread
            && let Some(detail) = self.exception_detail(thread, &report[0]).await
        {
            report.push(detail);
        }
        if stop.reason == "signal"
            && stop.description.is_none()
            && let Some(signal) = self.signal().await
        {
            report.push(format!("Signal: {signal}"));
        }
        if let Some(thread) = thread {
            match self.frames_of(thread).await {
                Ok(list) if !list.is_empty() => {
                    let top = &list[0];
                    report.push(format!("At {}", place(top, &self.sandbox)));
                    if let (Some(path), Some(line)) = (frame_path(top), top["line"].as_i64())
                        && let Some(lines) = excerpt(path, line, &self.sandbox)
                    {
                        report.push(lines);
                    }
                    let shown = &list[..list.len().min(STOP_FRAMES)];
                    let mut stack = format!("Stack:\n{}", frames(shown, &self.sandbox));
                    if list.len() > shown.len() {
                        stack.push_str(&format!(
                            "\n  … {} more (action stack)",
                            list.len() - shown.len()
                        ));
                    }
                    report.push(stack);
                    if let Some(locals) = self.locals(top).await {
                        report.push(locals);
                    }
                }
                Ok(_) => report.push("No stack frames.".to_owned()),
                Err(reason) => report.push(format!("The stack could not be read: {reason}")),
            }
        }
        report.extend(self.unread_blocks());
        report.join("\n")
    }

    /// The adapter's account of the exception the thread `thread` stopped on, when it says more than `heading`, the report's first line.
    async fn exception_detail(&self, thread: i64, heading: &str) -> Option<String> {
        if !self.capabilities["supportsExceptionInfoRequest"]
            .as_bool()
            .unwrap_or(false)
        {
            return None;
        }
        let info = self
            .client
            .request("exceptionInfo", json!({"threadId": thread}), DETAIL_WAIT)
            .await
            .ok()?;
        let parts: Vec<String> = [
            info["exceptionId"].as_str(),
            info["description"].as_str(),
            info["details"]["message"].as_str(),
        ]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty() && !heading.contains(*part))
        .map(|part| clip(part, 500))
        .collect();
        (!parts.is_empty()).then(|| format!("Exception: {}", parts.join(" — ")))
    }

    /// The signal the program stopped on, for an adapter such as gdb's that says only that it was a signal: read from gdb's `$_siginfo`, and named when it is a usual one.
    async fn signal(&self) -> Option<String> {
        let answer = self
            .client
            .request(
                "evaluate",
                json!({"expression": "$_siginfo.si_signo", "context": "watch"}),
                DETAIL_WAIT,
            )
            .await
            .ok()?;
        let number: i32 = answer["result"].as_str()?.trim().parse().ok()?;
        Some(match signal_name(number) {
            Some(name) => format!("{name} ({number})"),
            None => format!("signal {number}"),
        })
    }

    /// The variables of `frame`'s argument and local scopes as short blocks, at most [`STOP_LOCALS`] in all; nothing when it has none.
    async fn locals(&self, frame: &Value) -> Option<String> {
        let frame_id = frame["id"].as_i64()?;
        let scopes = self.scopes_of(frame_id).await.ok()?;
        let mut wanted: Vec<&Value> = scopes.iter().filter(|scope| is_local(scope)).collect();
        if wanted.is_empty() {
            wanted.extend(scopes.iter().find(|scope| !is_costly(scope)));
        }
        let mut blocks = Vec::new();
        let mut room = STOP_LOCALS;
        for scope in wanted {
            let Some(reference) = reference_of(scope) else {
                continue;
            };
            let Ok(variables) = self.children(reference).await else {
                continue;
            };
            if variables.is_empty() {
                continue;
            }
            let mut block = format!("{}:", scope["name"].as_str().unwrap_or("Locals"));
            for variable in variables.iter().take(room) {
                block.push('\n');
                block.push_str(&variable_line(variable, 1));
            }
            if variables.len() > room {
                block.push_str(&format!(
                    "\n  … {} more (action variables)",
                    variables.len() - room
                ));
            }
            room = room.saturating_sub(variables.len());
            blocks.push(block);
            if room == 0 {
                break;
            }
        }
        (!blocks.is_empty()).then(|| blocks.join("\n"))
    }

    /// The program's and the debugger's output the model has not seen, as blocks.
    fn unread_blocks(&mut self) -> Vec<String> {
        let mut state = self.client.state();
        let program = state.output.unread();
        let console = state.console.unread();
        drop(state);
        [
            output_block("Program output", &program, REPORT_OUTPUT),
            output_block("Debugger messages", &console, 10),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// Reports that the program ended, with its exit code and the output the model has not seen.
    fn report_end(&mut self) -> String {
        self.reported_end = true;
        let code = self.client.state().exit_code;
        let mut report = vec![match code {
            Some(code) => format!("The program exited with code {code}."),
            None => "The debugging session ended.".to_owned(),
        }];
        report.extend(self.unread_blocks());
        report.push(format!(
            "Session {} can still show its output; disconnect it when done.",
            self.id
        ));
        report.join("\n")
    }

    /// Whether the program is over, ended or its adapter gone.
    fn is_over(&self) -> bool {
        let state = self.client.state();
        state.over() || state.exit_code.is_some()
    }

    /// Where the program is stopped, failing with what it is doing instead when it is not.
    ///
    /// # Errors
    ///
    /// Says that the program ended or is running.
    fn stopped(&self) -> Result<Stop, String> {
        if self.is_over() {
            return Err("the program has ended; launch it again to debug it further".to_owned());
        }
        self.client.state().stop.clone().ok_or_else(|| {
            "the program is running, not stopped; use action wait to wait for it to stop or pause to interrupt it".to_owned()
        })
    }

    /// The thread an action is about: `thread` when given, else the one that stopped, else the first.
    ///
    /// # Errors
    ///
    /// Returns why when the threads cannot be listed or there are none.
    async fn thread_for(&self, thread: Option<i64>) -> Result<i64, String> {
        if let Some(thread) = thread {
            return Ok(thread);
        }
        if let Some(thread) = self
            .client
            .state()
            .stop
            .as_ref()
            .and_then(|stop| stop.thread)
        {
            return Ok(thread);
        }
        self.thread_list()
            .await?
            .first()
            .and_then(|thread| thread["id"].as_i64())
            .ok_or_else(|| "the program has no threads".to_owned())
    }

    /// The program's threads.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter does not list them.
    async fn thread_list(&self) -> Result<Vec<Value>, String> {
        let answer = self
            .client
            .request("threads", json!({}), REQUEST_WAIT)
            .await?;
        Ok(answer["threads"].as_array().cloned().unwrap_or_default())
    }

    /// The frames of the stopped thread `thread`, asked for once per stop.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter does not give them.
    async fn frames_of(&mut self, thread: i64) -> Result<Vec<Value>, String> {
        let stops = self.client.state().stops;
        if let Some(frames) = &self.frames
            && frames.stops == stops
            && frames.thread == thread
        {
            return Ok(frames.list.clone());
        }
        let answer = self
            .client
            .request(
                "stackTrace",
                json!({"threadId": thread, "startFrame": 0, "levels": 100}),
                REQUEST_WAIT,
            )
            .await?;
        let list = answer["stackFrames"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        self.frames = Some(Frames {
            stops,
            thread,
            list: list.clone(),
        });
        Ok(list)
    }

    /// The frame numbered `index` of the stopped thread `thread`, `0` the innermost.
    ///
    /// # Errors
    ///
    /// Returns why when the program is not stopped or has no such frame.
    async fn frame(&mut self, thread: Option<i64>, index: usize) -> Result<Value, String> {
        self.stopped()?;
        let thread = self.thread_for(thread).await?;
        let list = self.frames_of(thread).await?;
        list.get(index).cloned().ok_or_else(|| {
            format!(
                "thread {thread} has {} frames; frame {index} does not exist",
                list.len()
            )
        })
    }

    /// The scopes of the frame with the adapter's id `frame_id`.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter does not give them.
    async fn scopes_of(&self, frame_id: i64) -> Result<Vec<Value>, String> {
        let answer = self
            .client
            .request("scopes", json!({"frameId": frame_id}), REQUEST_WAIT)
            .await?;
        Ok(answer["scopes"].as_array().cloned().unwrap_or_default())
    }

    /// The children listed under `reference`.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter does not give them.
    async fn children(&self, reference: i64) -> Result<Vec<Value>, String> {
        let answer = self
            .client
            .request(
                "variables",
                json!({"variablesReference": reference}),
                REQUEST_WAIT,
            )
            .await?;
        Ok(answer["variables"].as_array().cloned().unwrap_or_default())
    }

    /// Writes the children of `reference` into `out`, `indent` levels in, at most `limit` of them and expanding theirs `depth` levels further, never past [`VARIABLE_LINES`] lines in all.
    fn tree<'a>(
        &'a self,
        reference: i64,
        depth: usize,
        limit: usize,
        indent: usize,
        out: &'a mut Vec<String>,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        Box::pin(async move {
            let children = match self.children(reference).await {
                Ok(children) => children,
                Err(reason) => {
                    out.push(format!("{}[not read: {reason}]", "  ".repeat(indent)));
                    return;
                }
            };
            for child in children.iter().take(limit) {
                if out.len() >= VARIABLE_LINES {
                    out.push(format!(
                        "{}… stopped at {VARIABLE_LINES} lines; expand a ref to see more",
                        "  ".repeat(indent)
                    ));
                    return;
                }
                out.push(variable_line(child, indent));
                if depth > 0
                    && let Some(inner) = reference_of(child)
                {
                    self.tree(inner, depth - 1, limit, indent + 1, out).await;
                }
            }
            if children.len() > limit {
                out.push(format!(
                    "{}… {} more (raise limit, or expand [ref {reference}])",
                    "  ".repeat(indent),
                    children.len() - limit
                ));
            }
        })
    }

    /// Runs the program with `motion` on the thread `thread`, or the one that stopped, and waits up to `wait` for it to stop again or end.
    ///
    /// # Errors
    ///
    /// Returns why when the program is not stopped or the adapter refuses.
    pub(crate) async fn resume(
        &mut self,
        motion: Motion,
        thread: Option<i64>,
        wait: Duration,
    ) -> Result<String, String> {
        self.stopped()?;
        let thread = self.thread_for(thread).await?;
        let since = self.client.state().stops;
        self.reported_stops = since;
        self.client
            .request(motion.command(), json!({"threadId": thread}), REQUEST_WAIT)
            .await?;
        self.frames = None;
        Ok(self.settle(since, wait).await)
    }

    /// Interrupts the running program and waits up to `wait` for it to stop.
    ///
    /// # Errors
    ///
    /// Returns why when the program ended or the adapter refuses.
    pub(crate) async fn pause(
        &mut self,
        thread: Option<i64>,
        wait: Duration,
    ) -> Result<String, String> {
        if self.is_over() {
            return Err("the program has ended".to_owned());
        }
        if self.client.state().stop.is_some() {
            return Ok(format!("Already stopped.\n{}", self.report_stop().await));
        }
        let thread = self.thread_for(thread).await?;
        let since = self.client.state().stops;
        self.client
            .request("pause", json!({"threadId": thread}), REQUEST_WAIT)
            .await?;
        Ok(self.settle(since, wait).await)
    }

    /// Waits up to `wait` for the running program to stop or end, and reports where it is.
    pub(crate) async fn wait(&mut self, wait: Duration) -> String {
        let (stopped, stops) = {
            let state = self.client.state();
            (state.stop.is_some(), state.stops)
        };
        if self.is_over() {
            return self.report_end();
        }
        if stopped {
            return format!("Already stopped.\n{}", self.report_stop().await);
        }
        self.settle(stops, wait).await
    }

    /// The program's threads, the stopped one marked.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter does not list them.
    pub(crate) async fn threads(&self) -> Result<String, String> {
        let list = self.thread_list().await?;
        let stopped = self
            .client
            .state()
            .stop
            .as_ref()
            .and_then(|stop| stop.thread);
        Ok(format!("Threads (* stopped):\n{}", threads(&list, stopped)))
    }

    /// The stack of `thread`, or the one that stopped, at most `count` frames.
    ///
    /// # Errors
    ///
    /// Returns why when the program is not stopped or the adapter does not give it.
    pub(crate) async fn stack(
        &mut self,
        thread: Option<i64>,
        count: Option<usize>,
    ) -> Result<String, String> {
        self.stopped()?;
        let thread = self.thread_for(thread).await?;
        let list = self.frames_of(thread).await?;
        let count = count.unwrap_or(STACK_FRAMES).max(1);
        let shown = &list[..list.len().min(count)];
        let mut text = format!(
            "Stack of thread {thread} (frame numbers are what frame takes):\n{}",
            frames(shown, &self.sandbox)
        );
        if list.len() > shown.len() {
            text.push_str(&format!("\n  … {} more frames", list.len() - shown.len()));
        }
        Ok(text)
    }

    /// The scopes of frame `index` of `thread`, with the references that list their variables.
    ///
    /// # Errors
    ///
    /// Returns why when the program is not stopped or there is no such frame.
    pub(crate) async fn scopes(
        &mut self,
        thread: Option<i64>,
        index: usize,
    ) -> Result<String, String> {
        let frame = self.frame(thread, index).await?;
        let frame_id = frame["id"].as_i64().ok_or("the frame has no id")?;
        let scopes = self.scopes_of(frame_id).await?;
        let lines: Vec<String> = scopes
            .iter()
            .map(|scope| {
                let mut line = format!(
                    "  {} [ref {}]",
                    scope["name"].as_str().unwrap_or("?"),
                    scope["variablesReference"].as_i64().unwrap_or(0)
                );
                if scope["expensive"].as_bool().unwrap_or(false) {
                    line.push_str(" (expensive)");
                }
                line
            })
            .collect();
        Ok(format!(
            "Scopes of frame #{index} {}:\n{}",
            place(&frame, &self.sandbox),
            lines.join("\n")
        ))
    }

    /// The variables under `reference` when given; otherwise those of the scopes of frame `index` of `thread`, only the scope named `scope` when given, and the cheap ones otherwise; each expanded `depth` levels with at most `limit` children per level.
    ///
    /// # Errors
    ///
    /// Returns why when the program is not stopped, there is no such frame or scope, or the adapter does not give them.
    pub(crate) async fn variables(
        &mut self,
        thread: Option<i64>,
        index: usize,
        scope: Option<&str>,
        reference: Option<i64>,
        depth: usize,
        limit: usize,
    ) -> Result<String, String> {
        self.stopped()?;
        let mut out = Vec::new();
        if let Some(reference) = reference {
            out.push(format!("Children of [ref {reference}]:"));
            self.tree(reference, depth, limit, 1, &mut out).await;
            return Ok(out.join("\n"));
        }
        let frame = self.frame(thread, index).await?;
        let frame_id = frame["id"].as_i64().ok_or("the frame has no id")?;
        let scopes = self.scopes_of(frame_id).await?;
        out.push(format!("Frame #{index} {}", place(&frame, &self.sandbox)));
        let mut matched = false;
        for found in &scopes {
            let name = found["name"].as_str().unwrap_or("?");
            let expensive = is_costly(found);
            let wanted = match scope {
                Some(scope) => name.eq_ignore_ascii_case(scope),
                None => true,
            };
            if !wanted {
                continue;
            }
            matched = true;
            let reference = found["variablesReference"].as_i64().unwrap_or(0);
            if expensive && scope.is_none() {
                out.push(format!(
                    "{name} [ref {reference}]: not listed, being costly to read; ask for it with scope"
                ));
                continue;
            }
            out.push(format!("{name}:"));
            self.tree(reference, depth, limit, 1, &mut out).await;
        }
        if !matched {
            let names: Vec<&str> = scopes.iter().filter_map(|s| s["name"].as_str()).collect();
            return Err(format!(
                "frame #{index} has no scope {}; it has {}",
                scope.unwrap_or("?"),
                names.join(", ")
            ));
        }
        Ok(out.join("\n"))
    }

    /// Evaluates `expression` in frame `index` of `thread`, or globally when the program is running, and shows the result expanded `depth` levels.
    ///
    /// The expression is evaluated as a watch expression, never as a debugger command.
    ///
    /// # Errors
    ///
    /// Returns why when there is no such frame or the expression fails.
    pub(crate) async fn evaluate(
        &mut self,
        expression: &str,
        thread: Option<i64>,
        index: usize,
        depth: usize,
        limit: usize,
    ) -> Result<String, String> {
        let mut arguments = json!({"expression": expression, "context": "watch"});
        let mut place_text = String::new();
        if self.client.state().stop.is_some() {
            let frame = self.frame(thread, index).await?;
            arguments["frameId"] = frame["id"].clone();
            place_text = format!(" in frame #{index} {}", place(&frame, &self.sandbox));
        }
        let result = self
            .client
            .request("evaluate", arguments, REQUEST_WAIT)
            .await?;
        let shown = json!({
            "name": expression,
            "type": result["type"],
            "value": result["result"],
            "variablesReference": result["variablesReference"],
        });
        let mut out = vec![format!("Evaluated{place_text}:"), variable_line(&shown, 1)];
        if depth > 0
            && let Some(reference) = reference_of(&shown)
        {
            self.tree(reference, depth - 1, limit, 2, &mut out).await;
        }
        Ok(out.join("\n"))
    }

    /// Sets the breakpoint `spot`, beside the others, and reports where the adapter placed it.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter cannot do what the breakpoint asks for or refuses it.
    pub(crate) async fn add(&mut self, spot: Spot) -> Result<String, String> {
        self.check_support(&spot)?;
        match spot {
            Spot::Line(file, breakpoint) => {
                let line = breakpoint.line;
                let list = self.lines.entry(file.clone()).or_default();
                list.retain(|known| known.line != line);
                list.push(breakpoint);
                list.sort_by_key(|known| known.line);
                self.send_lines(&file).await?;
                let placed = self.lines[&file]
                    .iter()
                    .find(|known| known.line == line)
                    .map(|known| self.line_text(&file, known))
                    .unwrap_or_default();
                Ok(placed)
            }
            Spot::Function(breakpoint) => {
                let name = breakpoint.name.clone();
                self.functions.retain(|known| known.name != name);
                self.functions.push(breakpoint);
                self.send_functions().await?;
                Ok(self
                    .functions
                    .iter()
                    .find(|known| known.name == name)
                    .map(|known| function_text(known, self.said(known.id).as_ref()))
                    .unwrap_or_default())
            }
        }
    }

    /// Fails when the adapter cannot do what `spot` asks for.
    ///
    /// # Errors
    ///
    /// Names the feature the adapter lacks.
    fn check_support(&self, spot: &Spot) -> Result<(), String> {
        let has = |name: &str| self.capabilities[name].as_bool().unwrap_or(false);
        let (condition, hit, log) = match spot {
            Spot::Line(_, breakpoint) => (
                breakpoint.condition.is_some(),
                breakpoint.hit.is_some(),
                breakpoint.log.is_some(),
            ),
            Spot::Function(breakpoint) => {
                if !has("supportsFunctionBreakpoints") {
                    return Err(format!("{} has no function breakpoints", self.adapter));
                }
                (
                    breakpoint.condition.is_some(),
                    breakpoint.hit.is_some(),
                    false,
                )
            }
        };
        if condition && !has("supportsConditionalBreakpoints") {
            return Err(format!("{} has no conditional breakpoints", self.adapter));
        }
        if hit && !has("supportsHitConditionalBreakpoints") {
            return Err(format!("{} has no hit counts on breakpoints", self.adapter));
        }
        if log && !has("supportsLogPoints") {
            return Err(format!("{} has no logpoints", self.adapter));
        }
        Ok(())
    }

    /// Tells the adapter every breakpoint in `file` and takes in where it placed them.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter refuses them.
    async fn send_lines(&mut self, file: &Path) -> Result<(), String> {
        let list = self.lines.get(file).cloned().unwrap_or_default();
        let breakpoints: Vec<Value> = list
            .iter()
            .map(|breakpoint| {
                let mut entry = json!({"line": breakpoint.line});
                if let Some(condition) = &breakpoint.condition {
                    entry["condition"] = json!(condition);
                }
                if let Some(hit) = &breakpoint.hit {
                    entry["hitCondition"] = json!(hit);
                }
                if let Some(log) = &breakpoint.log {
                    entry["logMessage"] = json!(log);
                }
                entry
            })
            .collect();
        let name = file
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let answer = self
            .client
            .request(
                "setBreakpoints",
                json!({
                    "source": {"path": file, "name": name},
                    "breakpoints": breakpoints,
                    "lines": list.iter().map(|b| b.line).collect::<Vec<_>>(),
                }),
                REQUEST_WAIT,
            )
            .await?;
        let placed = answer["breakpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(list) = self.lines.get_mut(file) {
            for (breakpoint, said) in list.iter_mut().zip(placed) {
                breakpoint.id = said["id"].as_i64();
                breakpoint.verified = said["verified"].as_bool().unwrap_or(false);
                breakpoint.placed = said["line"].as_i64();
                breakpoint.message = said["message"].as_str().map(str::to_owned);
            }
            if list.is_empty() {
                self.lines.remove(file);
            }
        }
        Ok(())
    }

    /// Tells the adapter every function breakpoint and takes in where it placed them.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter refuses them.
    async fn send_functions(&mut self) -> Result<(), String> {
        let breakpoints: Vec<Value> = self
            .functions
            .iter()
            .map(|breakpoint| {
                let mut entry = json!({"name": breakpoint.name});
                if let Some(condition) = &breakpoint.condition {
                    entry["condition"] = json!(condition);
                }
                if let Some(hit) = &breakpoint.hit {
                    entry["hitCondition"] = json!(hit);
                }
                entry
            })
            .collect();
        let answer = self
            .client
            .request(
                "setFunctionBreakpoints",
                json!({"breakpoints": breakpoints}),
                REQUEST_WAIT,
            )
            .await?;
        let placed = answer["breakpoints"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        for (breakpoint, said) in self.functions.iter_mut().zip(placed) {
            breakpoint.id = said["id"].as_i64();
            breakpoint.verified = said["verified"].as_bool().unwrap_or(false);
            breakpoint.message = said["message"].as_str().map(str::to_owned);
        }
        Ok(())
    }

    /// Removes the breakpoints at `line` of `file`, every one in `file` when `line` is not given, the one on `function`, or every breakpoint when `all` is set, and lists those left.
    ///
    /// # Errors
    ///
    /// Returns why when nothing matches or the adapter refuses.
    pub(crate) async fn clear(
        &mut self,
        file: Option<PathBuf>,
        line: Option<i64>,
        function: Option<&str>,
        all: bool,
    ) -> Result<String, String> {
        let mut files = Vec::new();
        let mut functions = false;
        if all {
            for list in self.lines.values_mut() {
                list.clear();
            }
            files = self.lines.keys().cloned().collect();
            functions = !self.functions.is_empty();
            self.functions.clear();
        } else if let Some(name) = function {
            let before = self.functions.len();
            self.functions.retain(|known| known.name != name);
            if self.functions.len() == before {
                return Err(format!("there is no breakpoint on {name}"));
            }
            functions = true;
        } else if let Some(file) = file {
            let list = self.lines.get_mut(&file).ok_or_else(|| {
                format!("there is no breakpoint in {}", self.sandbox.display(&file))
            })?;
            let before = list.len();
            match line {
                Some(line) => {
                    list.retain(|known| known.line != line && known.placed != Some(line));
                }
                None => list.clear(),
            }
            if list.len() == before {
                return Err(format!(
                    "there is no breakpoint at {}:{}",
                    self.sandbox.display(&file),
                    line.unwrap_or_default()
                ));
            }
            files.push(file);
        } else {
            return Err(
                "say which breakpoint to clear: file and line, function, or all".to_owned(),
            );
        }
        for file in files {
            self.send_lines(&file).await?;
            self.lines.retain(|_, list| !list.is_empty());
        }
        if functions {
            self.send_functions().await?;
        }
        Ok(self
            .breakpoint_listing()
            .unwrap_or_else(|| "No breakpoints are set.".to_owned()))
    }

    /// Sets the exception filters to `filters`, replacing those set before, and reports which the adapter has.
    ///
    /// # Errors
    ///
    /// Returns why when the adapter has no exception breakpoints, does not know a filter, or refuses.
    pub(crate) async fn set_exceptions(&mut self, filters: Vec<String>) -> Result<String, String> {
        let known: Vec<(String, String)> = self.capabilities["exceptionBreakpointFilters"]
            .as_array()
            .map(|list| {
                list.iter()
                    .filter_map(|filter| {
                        Some((
                            filter["filter"].as_str()?.to_owned(),
                            filter["label"].as_str().unwrap_or_default().to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let offered = || {
            known
                .iter()
                .map(|(id, label)| format!("{id} ({label})"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if known.is_empty() {
            return Err(format!("{} has no exception breakpoints", self.adapter));
        }
        if let Some(unknown) = filters
            .iter()
            .find(|filter| !known.iter().any(|(id, _)| id == *filter))
        {
            return Err(format!(
                "{} has no exception filter {unknown}; it has {}",
                self.adapter,
                offered()
            ));
        }
        self.client
            .request(
                "setExceptionBreakpoints",
                json!({"filters": filters}),
                REQUEST_WAIT,
            )
            .await?;
        self.exceptions = filters;
        Ok(format!(
            "Exception breakpoints: {}.\nFilters the adapter has: {}.",
            if self.exceptions.is_empty() {
                "none".to_owned()
            } else {
                self.exceptions.join(", ")
            },
            offered()
        ))
    }

    /// The breakpoints set, one line each; nothing when there are none.
    pub(crate) fn breakpoint_listing(&self) -> Option<String> {
        let mut lines = Vec::new();
        for (file, list) in &self.lines {
            for breakpoint in list {
                lines.push(format!("  {}", self.line_text(file, breakpoint)));
            }
        }
        for breakpoint in &self.functions {
            lines.push(format!(
                "  {}",
                function_text(breakpoint, self.said(breakpoint.id).as_ref())
            ));
        }
        if !self.exceptions.is_empty() {
            lines.push(format!("  exceptions: {}", self.exceptions.join(", ")));
        }
        (!lines.is_empty()).then(|| format!("Breakpoints:\n{}", lines.join("\n")))
    }

    /// What the adapter last said of the breakpoint with the id `id` in an event since it was set, when it said anything.
    fn said(&self, id: Option<i64>) -> Option<Value> {
        id.and_then(|id| self.client.state().breakpoints.get(&id).cloned())
    }

    /// One breakpoint in `file` as text: where it is, what it does and whether the adapter placed it, as the adapter last said.
    fn line_text(&self, file: &Path, breakpoint: &LineBreakpoint) -> String {
        let said = self.said(breakpoint.id);
        let verified = said
            .as_ref()
            .and_then(|said| said["verified"].as_bool())
            .unwrap_or(breakpoint.verified);
        let placed = said
            .as_ref()
            .and_then(|said| said["line"].as_i64())
            .or(breakpoint.placed)
            .unwrap_or(breakpoint.line);
        let mut text = format!(
            "{}{}:{}",
            breakpoint
                .id
                .map_or_else(String::new, |id| format!("#{id} ")),
            self.sandbox.display(file),
            breakpoint.line
        );
        if placed != breakpoint.line {
            text.push_str(&format!(" (placed on line {placed})"));
        }
        extras(
            &mut text,
            breakpoint.condition.as_deref(),
            breakpoint.hit.as_deref(),
            breakpoint.log.as_deref(),
        );
        status(&mut text, verified, breakpoint.message.as_deref());
        text
    }

    /// The program output kept: what was not shown yet, or all of it when `all` is set, with the debugger's own messages.
    pub(crate) fn output(&mut self, all: bool) -> String {
        let mut state = self.client.state();
        let program = if all {
            let _ = state.output.unread();
            crate::state::Unread {
                text: state.output.all().to_owned(),
                dropped: 0,
            }
        } else {
            state.output.unread()
        };
        let console = state.console.unread();
        drop(state);
        let blocks: Vec<String> = [
            output_block("Program output", &program, OUTPUT_LINES),
            output_block("Debugger messages", &console, 40),
        ]
        .into_iter()
        .flatten()
        .collect();
        if blocks.is_empty() {
            "No new output.".to_owned()
        } else {
            blocks.join("\n")
        }
    }

    /// A one-line account of the session for listings.
    pub(crate) fn describe(&self) -> String {
        let state = self.client.state();
        let what = if state.over() || state.exit_code.is_some() {
            match state.exit_code {
                Some(code) => format!("exited with code {code}"),
                None => "ended".to_owned(),
            }
        } else if let Some(stop) = &state.stop {
            format!("stopped ({})", stop.reason)
        } else {
            "running".to_owned()
        };
        format!("{} {} {}: {what}", self.id, self.adapter, self.label)
    }

    /// Ends the session: tells the adapter to disconnect, ending a program it launched or `terminate` says to end, and leaving one it attached to running otherwise; waits at most `wait`.
    pub(crate) async fn disconnect(&mut self, terminate: Option<bool>, wait: Duration) -> String {
        let terminate = terminate.unwrap_or(self.launched);
        let over = self.is_over();
        let mut report = Vec::new();
        if !over {
            let arguments = json!({"terminateDebuggee": terminate, "restart": false});
            match self.client.request("disconnect", arguments, wait).await {
                Ok(_) => {}
                Err(reason) => report.push(format!("The adapter did not answer cleanly: {reason}")),
            }
            let soon = Instant::now() + Duration::from_millis(300);
            self.client.wait_until(soon, |state| state.gone).await;
        }
        let blocks = self.unread_blocks();
        report.insert(
            0,
            format!(
                "Session {} ended{}.",
                self.id,
                if over {
                    ""
                } else if terminate {
                    "; the program was stopped"
                } else {
                    "; the program was left running"
                }
            ),
        );
        report.extend(blocks);
        report.join("\n")
    }
}

/// Whether the adapter has said the session ended or stopped talking, after the program exited.
fn over_fully(client: &Client) -> bool {
    client.state().over()
}

/// Whether `scope` holds a frame's arguments or local variables, by the adapter's hint or its name.
fn is_local(scope: &Value) -> bool {
    matches!(
        scope["presentationHint"].as_str(),
        Some("locals" | "arguments")
    ) || scope["name"].as_str().is_some_and(|name| {
        let name = name.to_ascii_lowercase();
        name.starts_with("local") || name.starts_with("argument")
    })
}

/// Whether listing `scope` is costly or seldom useful, so that it is listed only when asked for by name: the adapter says it is expensive, or it holds the registers.
fn is_costly(scope: &Value) -> bool {
    scope["expensive"].as_bool().unwrap_or(false)
        || scope["name"]
            .as_str()
            .is_some_and(|name| name.eq_ignore_ascii_case("registers"))
}

/// The usual name of the Linux signal numbered `number`.
fn signal_name(number: i32) -> Option<&'static str> {
    Some(match number {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        5 => "SIGTRAP",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        9 => "SIGKILL",
        10 => "SIGUSR1",
        11 => "SIGSEGV",
        12 => "SIGUSR2",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => return None,
    })
}

/// One function breakpoint as text: its function, what it does and whether the adapter placed it, as it last `said` in an event when it did.
fn function_text(breakpoint: &FunctionBreakpoint, said: Option<&Value>) -> String {
    let mut text = format!(
        "{}function {}",
        breakpoint
            .id
            .map_or_else(String::new, |id| format!("#{id} ")),
        breakpoint.name
    );
    extras(
        &mut text,
        breakpoint.condition.as_deref(),
        breakpoint.hit.as_deref(),
        None,
    );
    let verified = said
        .and_then(|said| said["verified"].as_bool())
        .unwrap_or(breakpoint.verified);
    status(&mut text, verified, breakpoint.message.as_deref());
    text
}

/// Adds a breakpoint's condition, hit count and log message to `text`.
fn extras(text: &mut String, condition: Option<&str>, hit: Option<&str>, log: Option<&str>) {
    if let Some(condition) = condition {
        text.push_str(&format!(" if {condition}"));
    }
    if let Some(hit) = hit {
        text.push_str(&format!(" hits {hit}"));
    }
    if let Some(log) = log {
        text.push_str(&format!(" logs \"{log}\""));
    }
}

/// Adds whether a breakpoint was placed, and the adapter's reason when it was not, to `text`.
fn status(text: &mut String, verified: bool, message: Option<&str>) {
    if verified {
        return;
    }
    text.push_str(" — pending");
    if let Some(message) = message.filter(|message| !message.is_empty()) {
        text.push_str(&format!(": {}", clip(message, 200)));
    }
}

/// The arguments of the launch or attach request `start` makes with `adapter`: the adapter's own settings, the program's, and the model's settings over them.
fn request_arguments(adapter: Option<&Adapter>, start: &Start) -> Map<String, Value> {
    let mut arguments = Map::new();
    let mut extend = |settings: &Map<String, Value>| {
        for (key, value) in settings {
            arguments.insert(key.clone(), value.clone());
        }
    };
    if let Some(adapter) = adapter {
        if start.address.is_some() {
            extend(&adapter.remote);
        } else if start.pid.is_some() {
            extend(&adapter.attach);
        } else {
            extend(&adapter.launch);
            if start
                .program
                .as_deref()
                .is_some_and(|program| language_of(program) == "native")
            {
                extend(&adapter.binary);
            }
        }
    }
    arguments.insert("name".to_owned(), json!("crab"));
    if let Some(pid) = start.pid {
        arguments.insert("pid".to_owned(), json!(pid));
        arguments.insert("processId".to_owned(), json!(pid));
    } else if start.address.is_none() {
        if let Some(program) = &start.program {
            arguments.insert("program".to_owned(), json!(program));
        }
        arguments.insert("args".to_owned(), json!(start.args));
        arguments.insert("cwd".to_owned(), json!(start.cwd));
        if !start.env.is_empty() {
            arguments.insert("env".to_owned(), Value::Object(start.env.clone()));
        }
    }
    for (key, value) in &start.config {
        arguments.insert(key.clone(), value.clone());
    }
    arguments
}

/// The adapter for `start` and a connection to it: to the address it names, or to the adapter it names or the first one installed for its language, started in `root`.
///
/// # Errors
///
/// Returns why when no adapter fits or is installed, naming what to install, or it cannot be started.
async fn connect(start: &Start, root: &Path) -> Result<(Option<Adapter>, Client), String> {
    let adapters = configured()?;
    let language = start
        .language
        .as_deref()
        .map(normalize_language)
        .or_else(|| start.program.as_deref().map(|p| language_of(p).to_owned()))
        .unwrap_or_else(|| "native".to_owned());
    let candidates: Vec<&Adapter> = match &start.adapter {
        Some(id) => {
            let named: Vec<&Adapter> = adapters
                .iter()
                .filter(|adapter| adapter.id == *id)
                .collect();
            if named.is_empty() {
                let ids: Vec<&str> = adapters.iter().map(|adapter| adapter.id.as_str()).collect();
                return Err(format!(
                    "there is no adapter {id}; the known ones are {}",
                    ids.join(", ")
                ));
            }
            named
        }
        None => adapters
            .iter()
            .filter(|adapter| adapter.debugs(&language))
            .collect(),
    };
    if let Some(address) = &start.address {
        let client = Client::connect(address, LISTEN_WAIT).await?;
        return Ok((candidates.first().map(|adapter| (*adapter).clone()), client));
    }
    if candidates.is_empty() {
        return Err(format!(
            "no debug adapter is known for {language}; name the language with language, or add an adapter for it to {}",
            crate::adapters::adapters_file().map_or_else(
                || "debuggers.json".to_owned(),
                |path| path.display().to_string()
            )
        ));
    }
    for adapter in &candidates {
        let Some((program, arguments)) = adapter.command(root).await else {
            continue;
        };
        let client = match adapter.transport {
            Transport::Stdio => Client::spawn_stdio(&program, &arguments, root)?,
            Transport::Port => Client::spawn_port(&program, &arguments, root, LISTEN_WAIT).await?,
        };
        return Ok((Some((*adapter).clone()), client));
    }
    let wanted: Vec<String> = candidates
        .iter()
        .map(|adapter| format!("- {}: install {}", adapter.id, adapter.install))
        .collect();
    Err(format!(
        "no debug adapter for {language} is installed. Install one of these, or point OMC_DAP_<ID> (as in OMC_DAP_LLDB_DAP) at its command:\n{}",
        wanted.join("\n")
    ))
}

#[cfg(test)]
/// Tests for the arguments of launching and attaching.
mod tests {
    use super::*;
    use crate::adapters::default_adapters;

    /// A launch carries the adapter's settings, the program's, the binary settings for a compiled program, and the model's over them all.
    #[test]
    fn launch_arguments_layer_settings() {
        let dlv = default_adapters()
            .into_iter()
            .find(|adapter| adapter.id == "dlv")
            .unwrap();
        let mut start = Start {
            program: Some(PathBuf::from("/w/app")),
            args: vec!["-v".into()],
            cwd: PathBuf::from("/w"),
            ..Start::default()
        };
        start.config.insert("buildFlags".into(), json!("-race"));
        let arguments = request_arguments(Some(&dlv), &start);
        assert_eq!(arguments["mode"], "exec");
        assert_eq!(arguments["program"], "/w/app");
        assert_eq!(arguments["args"], json!(["-v"]));
        assert_eq!(arguments["buildFlags"], "-race");
        assert!(arguments.get("env").is_none());
        start.program = Some(PathBuf::from("/w/main.go"));
        assert_eq!(request_arguments(Some(&dlv), &start)["mode"], "debug");
    }

    /// Attaching to a process names it both ways adapters read it, and attaching through an address uses the remote settings.
    #[test]
    fn attach_arguments() {
        let dlv = default_adapters()
            .into_iter()
            .find(|adapter| adapter.id == "dlv")
            .unwrap();
        let by_pid = Start {
            pid: Some(42),
            ..Start::default()
        };
        let arguments = request_arguments(Some(&dlv), &by_pid);
        assert_eq!(
            (arguments["pid"].clone(), arguments["processId"].clone()),
            (json!(42), json!(42))
        );
        assert_eq!(arguments["mode"], "local");
        assert!(arguments.get("program").is_none());
        let remote = Start {
            address: Some("127.0.0.1:1".into()),
            ..Start::default()
        };
        assert_eq!(request_arguments(Some(&dlv), &remote)["mode"], "remote");
    }

    /// Breakpoints read back with where they are, what they do and whether they were placed.
    #[test]
    fn breakpoint_texts() {
        let breakpoint = FunctionBreakpoint {
            name: "main".into(),
            condition: Some("argc > 1".into()),
            id: Some(3),
            message: Some("no symbol".into()),
            ..FunctionBreakpoint::default()
        };
        assert_eq!(
            function_text(&breakpoint, None),
            "#3 function main if argc > 1 — pending: no symbol"
        );
        let placed = json!({"id": 3, "verified": true});
        assert_eq!(
            function_text(&breakpoint, Some(&placed)),
            "#3 function main if argc > 1"
        );
    }
}
