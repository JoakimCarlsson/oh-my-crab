//! The engine a script runs in: QuickJS on a thread of its own, under limits on time, memory and tool calls, whose only ways out are the two host functions the prelude wraps, one that starts a tool call and one that prints.
//!
//! The engine never runs a tool itself. A tool call is handed to the async side as a [`Request`] and the promise the script holds stays pending until the matching [`Reply`] comes back, so the script can have several calls running at once.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use rquickjs::context::EvalOptions;
use rquickjs::promise::PromiseState;
use rquickjs::{Context, Ctx, Exception, Function, Object, Promise, Runtime, Value};
use serde_json::Value as Json;
use tokio::sync::mpsc::UnboundedSender;

/// The script that sets up what a script can reach, run before it.
const PRELUDE: &str = include_str!("prelude.js");

/// How long the engine waits for a reply before it looks at the clock again.
const TICK: Duration = Duration::from_millis(20);

/// The most stack QuickJS may use, in bytes, before a script fails with a stack overflow.
const JS_STACK: usize = 1 << 20;

/// The stack of the thread the engine runs on, in bytes: room for [`JS_STACK`] and the engine around it.
pub(crate) const THREAD_STACK: usize = 8 << 20;

/// What a script fails with when it waits on something that nothing will ever settle.
const NEVER_SETTLES: &str = "the script is waiting on a promise that nothing will settle; await the tool calls it makes, not promises of its own that never resolve";

/// How far one run of a script may go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Limits {
    /// The wall time it may run, not counting time spent waiting for the reader to approve a call.
    pub(crate) time: Duration,
    /// The memory the engine may allocate, in bytes.
    pub(crate) memory: usize,
    /// The most tool calls it may make.
    pub(crate) calls: usize,
    /// The most printed text kept, in bytes; what is printed beyond is dropped and counted.
    pub(crate) printed: usize,
}

/// A tool call the script started.
#[derive(Debug)]
pub(crate) struct Request {
    /// Its number within the run, counted from one.
    pub(crate) id: u32,
    /// The tool.
    pub(crate) name: String,
    /// The arguments, as the script gave them.
    pub(crate) args: Json,
}

/// How a tool call the script started ended.
#[derive(Debug)]
pub(crate) struct Reply {
    /// The number of the call.
    pub(crate) id: u32,
    /// Its result as a value for the script, or the text saying why it did not succeed.
    pub(crate) outcome: Result<Json, String>,
}

/// How the engine reaches the side that runs its tool calls.
pub(crate) struct Link {
    /// Where the calls the script starts go.
    pub(crate) requests: UnboundedSender<Request>,
    /// Where their outcomes come back; closed when the run is abandoned.
    pub(crate) replies: Receiver<Reply>,
    /// Set when the run is abandoned, such as when the turn is cancelled.
    pub(crate) cancelled: Arc<AtomicBool>,
    /// How long, in milliseconds, the calls have waited for the reader so far, which the time limit leaves out.
    pub(crate) waited: Arc<AtomicU64>,
}

/// How a run ended.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Finished {
    /// What the script printed, one line per print, up to the limit.
    pub(crate) printed: String,
    /// How many bytes of printed text were dropped past the limit.
    pub(crate) dropped: usize,
    /// What the script returned, shown as text, when it returned something.
    pub(crate) value: Option<String>,
    /// Why the script failed, when it did.
    pub(crate) error: Option<String>,
    /// How many tool calls it made.
    pub(crate) calls: usize,
}

/// Why a run was stopped from outside the script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// The run was abandoned.
    Cancelled,
    /// The run went past its time limit.
    Late,
}

/// Tells whether a run is to stop.
#[derive(Clone)]
struct Clock {
    /// When the run started.
    started: Instant,
    /// The wall time it may run.
    limit: Duration,
    /// How long, in milliseconds, it waited for the reader, which is added to the limit.
    waited: Arc<AtomicU64>,
    /// Set when the run is abandoned.
    cancelled: Arc<AtomicBool>,
}

impl Clock {
    /// Why the run is to stop now, or nothing while it may go on.
    fn stop(&self) -> Option<Stop> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Some(Stop::Cancelled);
        }
        let allowed = self.limit + Duration::from_millis(self.waited.load(Ordering::Relaxed));
        (self.started.elapsed() > allowed).then_some(Stop::Late)
    }

    /// What the script fails with when it is stopped for `stop`.
    fn message(&self, stop: Stop) -> String {
        match stop {
            Stop::Cancelled => "the script was stopped: the turn was cancelled".to_owned(),
            Stop::Late => format!(
                "the script ran past its time limit of {} s and was stopped",
                self.limit.as_secs()
            ),
        }
    }
}

/// What the two host functions share with the engine.
struct Host {
    /// Where the calls the script starts go.
    requests: UnboundedSender<Request>,
    /// The most tool calls the script may make.
    most: usize,
    /// How many it made.
    calls: usize,
    /// How many have not come back yet.
    outstanding: usize,
    /// The most printed bytes kept.
    room: usize,
    /// What was printed.
    printed: String,
    /// How many printed bytes were dropped.
    dropped: usize,
}

impl Host {
    /// Starts a call of the tool `name` with the arguments `json` and returns its number.
    ///
    /// # Errors
    ///
    /// Returns what the script is to be thrown when the call limit is reached, the arguments are not JSON, or the run was abandoned.
    fn start(&mut self, name: String, json: &str) -> Result<u32, String> {
        if self.calls >= self.most {
            return Err(format!(
                "this script reached its limit of {} tool calls; do the rest in another script or directly",
                self.most
            ));
        }
        let args: Json = serde_json::from_str(json)
            .map_err(|error| format!("the arguments of tools.{name} are not JSON: {error}"))?;
        self.calls += 1;
        let id = u32::try_from(self.calls).unwrap_or(u32::MAX);
        self.requests
            .send(Request { id, name, args })
            .map_err(|_| "the script was stopped: the turn was cancelled".to_owned())?;
        self.outstanding += 1;
        Ok(id)
    }

    /// Adds `text` and a newline to what was printed, or counts it as dropped once the limit is reached.
    fn print(&mut self, text: &str) {
        if self.dropped == 0 && self.printed.len() + text.len() < self.room {
            self.printed.push_str(text);
            self.printed.push('\n');
        } else {
            self.dropped += text.len() + 1;
        }
    }
}

/// Runs `source` as the body of an async function with the tools `names` callable, within `limits`, talking to the async side through `link`, and says how it ended.
///
/// Blocks the thread until the script settles, fails, runs out of time or is abandoned; meant to run on a thread of its own with a stack of [`THREAD_STACK`] bytes.
pub(crate) fn run(source: &str, names: &[String], limits: Limits, link: Link) -> Finished {
    let Link {
        requests,
        replies,
        cancelled,
        waited,
    } = link;
    let clock = Clock {
        started: Instant::now(),
        limit: limits.time,
        waited,
        cancelled,
    };
    let host = Rc::new(RefCell::new(Host {
        requests,
        most: limits.calls,
        calls: 0,
        outstanding: 0,
        room: limits.printed,
        printed: String::new(),
        dropped: 0,
    }));
    let outcome = start(source, names, limits, &clock, &replies, &host);
    let host = host.borrow();
    let (value, error) = match outcome {
        Ok(value) => (value, None),
        Err(error) => (None, Some(error)),
    };
    Finished {
        printed: host.printed.clone(),
        dropped: host.dropped,
        value,
        error,
        calls: host.calls,
    }
}

/// Makes the engine within `limits`, watched by `clock`, and runs `source` in it with the tools `names`, its tool calls coming back on `replies` and its host functions sharing `host`.
///
/// # Errors
///
/// Returns why the script failed or was stopped, or why the engine could not start.
fn start(
    source: &str,
    names: &[String],
    limits: Limits,
    clock: &Clock,
    replies: &Receiver<Reply>,
    host: &Rc<RefCell<Host>>,
) -> Result<Option<String>, String> {
    let runtime =
        Runtime::new().map_err(|error| format!("the script engine could not start: {error}"))?;
    runtime.set_memory_limit(limits.memory);
    runtime.set_max_stack_size(JS_STACK);
    let watch = clock.clone();
    runtime.set_interrupt_handler(Some(Box::new(move || watch.stop().is_some())));
    let context = Context::full(&runtime)
        .map_err(|error| format!("the script engine could not start: {error}"))?;
    context.with(|ctx| session(&ctx, source, names, clock, replies, host))
}

/// Sets up the prelude in `ctx`, runs `source` and drives it until it settles, settling its tool calls as their replies come back on `replies`.
///
/// # Errors
///
/// Returns why the script failed or was stopped.
fn session<'js>(
    ctx: &Ctx<'js>,
    source: &str,
    names: &[String],
    clock: &Clock,
    replies: &Receiver<Reply>,
    host: &Rc<RefCell<Host>>,
) -> Result<Option<String>, String> {
    let broken = |error: rquickjs::Error| format!("the script engine failed: {error}");
    let helpers = prelude(ctx, names, host).map_err(broken)?;
    let settle: Function = helpers.get("settle").map_err(broken)?;
    let render: Function = helpers.get("render").map_err(broken)?;
    let failure: Function = helpers.get("failure").map_err(broken)?;
    let fail = |error: rquickjs::Error| match clock.stop() {
        Some(stop) => clock.message(stop),
        None => thrown(ctx, &failure, error),
    };
    let mut options = EvalOptions::default();
    options.strict = false;
    options.filename = Some("script.js".to_owned());
    let wrapped = format!("(async () => {{{source}\n}})()");
    let started: Value = ctx.eval_with_options(wrapped, options).map_err(fail)?;
    let promise: Promise = started
        .into_promise()
        .ok_or_else(|| "the script did not start as an async function".to_owned())?;
    loop {
        while ctx.execute_pending_job() {}
        if let Some(stop) = clock.stop() {
            return Err(clock.message(stop));
        }
        match promise.state() {
            PromiseState::Resolved => {
                let value: Value = promise
                    .result()
                    .unwrap_or_else(|| Ok(Value::new_undefined(ctx.clone())))
                    .map_err(fail)?;
                return render.call((value,)).map_err(fail);
            }
            PromiseState::Rejected => {
                let error = promise
                    .result::<Value>()
                    .map_or(rquickjs::Error::Exception, |result| {
                        result.err().unwrap_or(rquickjs::Error::Exception)
                    });
                return Err(fail(error));
            }
            PromiseState::Pending => {}
        }
        if host.borrow().outstanding == 0 {
            return Err(NEVER_SETTLES.to_owned());
        }
        match replies.recv_timeout(TICK) {
            Ok(reply) => {
                host.borrow_mut().outstanding -= 1;
                let (ok, json) = match reply.outcome {
                    Ok(value) => (true, serde_json::to_string(&value)),
                    Err(message) => (false, serde_json::to_string(&message)),
                };
                let json = json.unwrap_or_else(|_| "null".to_owned());
                settle.call::<_, ()>((reply.id, ok, json)).map_err(fail)?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return Err(clock.message(Stop::Cancelled)),
        }
    }
}

/// Runs the prelude in `ctx` with host functions sharing `host` and the tools `names`, and returns the helpers it gives back.
///
/// # Errors
///
/// Returns what the engine fails with, which only happens when it is out of memory.
fn prelude<'js>(
    ctx: &Ctx<'js>,
    names: &[String],
    host: &Rc<RefCell<Host>>,
) -> rquickjs::Result<Object<'js>> {
    let functions = Object::new(ctx.clone())?;
    let calls = host.clone();
    functions.set(
        "call",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, name: String, json: String| -> rquickjs::Result<u32> {
                calls
                    .borrow_mut()
                    .start(name, &json)
                    .map_err(|message| Exception::throw_message(&ctx, &message))
            },
        )?,
    )?;
    let prints = host.clone();
    functions.set(
        "print",
        Function::new(ctx.clone(), move |text: String| {
            prints.borrow_mut().print(&text);
        })?,
    )?;
    let mut options = EvalOptions::default();
    options.filename = Some("prelude.js".to_owned());
    let setup: Function = ctx.eval_with_options(PRELUDE, options)?;
    setup.call((functions, names.to_vec()))
}

/// Describes `error`, thrown in `ctx`, with the prelude's `failure` helper when it is a JavaScript exception.
fn thrown<'js>(ctx: &Ctx<'js>, failure: &Function<'js>, error: rquickjs::Error) -> String {
    if !matches!(error, rquickjs::Error::Exception) {
        return error.to_string();
    }
    let caught = ctx.catch();
    failure
        .call::<_, String>((caught,))
        .unwrap_or_else(|_| "the script threw something that cannot be shown".to_owned())
}
