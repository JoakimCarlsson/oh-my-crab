//! The `script` tool: the model writes a short JavaScript program that calls the agent's other tools, and only what the program prints and returns reaches the model's context.
//!
//! The program runs in QuickJS, embedded, on a thread of its own. It has no file system, network, process or timer access: the tools, printing and returning a value are all it can reach. Every tool call it makes goes through the agent's [`Caller`], so it is checked against the tool's schema, shown to the hooks, approved and reported as a child of the `script` call exactly like a call the model makes itself. A run is bounded in wall time, memory and number of tool calls, and is stopped when the turn running it is cancelled.

mod engine;
mod signature;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::FuturesUnordered;
use omc_context::render;
use omc_tools::{Caller, NoCaller, Registry, Spill, Tool, ToolError, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Finished, Limits, Link, Reply, THREAD_STACK};

/// The name the model calls the tool by.
pub const NAME: &str = "script";

/// The tools a script never gets: itself, starting subagents, and the ones that talk to the reader, steer the conversation or only make sense to the model.
pub const WITHHELD: &[&str] = &[
    NAME,
    "task",
    "ask",
    "exit_plan_mode",
    "todo",
    "context_notes",
    "new_context",
    "checkpoint",
    "rewind",
    "wait",
    "set_title",
    "submit_result",
    "recall",
    "skill",
    "remember",
    "memory_edit",
];

/// The wall time a script may run when the call gives no `timeout`, in seconds.
pub const DEFAULT_SECONDS: u64 = 60;

/// The most wall time a call may give a script, in seconds.
pub const MAX_SECONDS: u64 = 300;

/// The memory a script may allocate, in MiB.
pub const MEMORY_MIB: usize = 64;

/// The most tool calls one script may make.
pub const MAX_CALLS: usize = 500;

/// The most printed text kept, in bytes, before the rest is dropped.
const PRINTED_LIMIT: usize = 1 << 20;

/// The longest result shown to the model, in bytes; a longer one is cut in the middle and saved whole.
pub const OUTPUT_LIMIT: usize = 20 * 1024;

/// How often the time the calls waited for the reader is passed on to the engine.
const WAIT_TICK: Duration = Duration::from_millis(100);

/// What the model is told about the tool, before the tools are listed.
const PROMPT: &str = include_str!("prompts/script.md");

/// Arguments of a `script` call.
#[derive(Deserialize)]
struct Args {
    /// The body of the async function to run.
    code: String,
    /// The wall time it may run, in seconds.
    timeout: Option<u64>,
}

/// Runs a JavaScript program that calls the agent's other tools.
pub struct Script {
    /// Where output too long to show is saved whole.
    spill: Spill,
    /// The tools a script may call, in name order.
    exposed: Vec<String>,
    /// Whether every one of them only looks.
    read_only: bool,
    /// What the model is told, with the tools listed.
    description: String,
}

impl Script {
    /// The tool, saving long output in `spill`, with no tools to call until it is [`Tool::fitted`] to an agent's tools.
    pub fn new(spill: Spill) -> Self {
        Self::with_tools(spill, &Registry::new())
    }

    /// The tool, saving long output in `spill`, letting scripts call the tools of `tools` that are not [`WITHHELD`].
    pub fn with_tools(spill: Spill, tools: &Registry) -> Self {
        let usable: Vec<&Arc<dyn Tool>> = tools
            .iter()
            .filter(|tool| !WITHHELD.contains(&tool.name()))
            .collect();
        let listing: Vec<String> = usable
            .iter()
            .map(|tool| signature::signature(tool.name(), &tool.schema(), tool.data_shape()))
            .collect();
        let listing = if listing.is_empty() {
            "(none)".to_owned()
        } else {
            listing.join("\n")
        };
        let description = render(
            PROMPT,
            &[
                ("tools", listing.as_str()),
                ("seconds", DEFAULT_SECONDS.to_string().as_str()),
                ("most_seconds", MAX_SECONDS.to_string().as_str()),
                ("memory", MEMORY_MIB.to_string().as_str()),
                ("calls", MAX_CALLS.to_string().as_str()),
                ("output", (OUTPUT_LIMIT / 1024).to_string().as_str()),
            ],
        );
        Self {
            spill,
            exposed: usable.iter().map(|tool| tool.name().to_owned()).collect(),
            read_only: usable.iter().all(|tool| tool.read_only()),
            description,
        }
    }

    /// The tools a script may call, in name order.
    pub fn exposed(&self) -> &[String] {
        &self.exposed
    }
}

/// Marks a run as abandoned when it is dropped, so that the engine stops even in the middle of a loop that calls nothing.
struct Abandon(Arc<AtomicBool>);

impl Drop for Abandon {
    /// Sets the flag the engine watches.
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// The value a script gets for a successful call that returned `output`: its data when it has some, and its text otherwise.
fn value_of(output: ToolOutput) -> Value {
    output.data.unwrap_or(Value::String(output.text))
}

/// Runs `source` on an engine thread of its own with the tools `names`, within `limits`, carrying out its tool calls through `caller` as they come, several at once when the script starts several, and says how it ended.
///
/// When the returned future is dropped, as when the turn is cancelled, the calls still running are dropped with it and the engine is told to stop.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the engine thread cannot be started or ends without saying how the script ended.
async fn execute(
    source: String,
    names: Vec<String>,
    limits: Limits,
    caller: &dyn Caller,
) -> Result<Finished, ToolError> {
    let (requests, mut incoming) = tokio::sync::mpsc::unbounded_channel();
    let (answers, replies) = std::sync::mpsc::channel();
    let (done, mut finished) = tokio::sync::oneshot::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let waited = Arc::new(AtomicU64::new(0));
    let _abandon = Abandon(cancelled.clone());
    let link = Link {
        requests,
        replies,
        cancelled,
        waited: waited.clone(),
    };
    std::thread::Builder::new()
        .name("script".to_owned())
        .stack_size(THREAD_STACK)
        .spawn(move || {
            let _ = done.send(engine::run(&source, &names, limits, link));
        })
        .map_err(|error| ToolError::Failed(format!("the script could not start: {error}")))?;
    let mut running = FuturesUnordered::new();
    let mut ticker = tokio::time::interval(WAIT_TICK);
    loop {
        tokio::select! {
            ended = &mut finished => {
                return ended.map_err(|_| ToolError::Failed("the script engine stopped without a result".to_owned()));
            }
            Some(request) = incoming.recv() => {
                running.push(async move {
                    let outcome = caller.call(&request.name, request.args).await;
                    Reply { id: request.id, outcome: outcome.map(value_of) }
                });
            }
            Some(reply) = running.next(), if !running.is_empty() => {
                let _ = answers.send(reply);
            }
            _ = ticker.tick() => {
                let millis = u64::try_from(caller.waited().as_millis()).unwrap_or(u64::MAX);
                waited.store(millis, Ordering::Relaxed);
            }
        }
    }
}

/// What the model reads for a run that ended as `finished`, cut to [`OUTPUT_LIMIT`] with the whole saved in `spill`: what it printed and what it returned, or why it failed with what it printed before.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the script failed or was stopped.
fn report(spill: &Spill, finished: Finished) -> Result<ToolOutput, ToolError> {
    let Finished {
        printed,
        dropped,
        value,
        error,
        calls,
    } = finished;
    let mut parts: Vec<String> = Vec::new();
    let printed = printed.trim_end();
    if !printed.is_empty() {
        parts.push(printed.to_owned());
    }
    if dropped > 0 {
        parts.push(format!(
            "[{dropped} more bytes of printed output were dropped past the first {} KiB]",
            PRINTED_LIMIT / 1024
        ));
    }
    if let Some(error) = error {
        let plural = if calls == 1 { "" } else { "s" };
        let mut text = format!("the script failed after {calls} tool call{plural}: {error}");
        if !parts.is_empty() {
            text.push_str("\n\nPrinted before it failed:\n");
            text.push_str(&parts.join("\n"));
        }
        return Err(ToolError::Failed(spill.cap(NAME, text, OUTPUT_LIMIT)));
    }
    if let Some(value) = value {
        if parts.is_empty() {
            parts.push(value);
        } else {
            parts.push(format!("Returned:\n{value}"));
        }
    }
    let text = if parts.is_empty() {
        "The script finished without printing or returning anything.".to_owned()
    } else {
        parts.join("\n\n")
    };
    Ok(ToolOutput::new(spill.cap(NAME, text, OUTPUT_LIMIT)))
}

/// The line a call is shown by: the first comment of `code` when it starts with one, or else its first line, cut short, with how many lines it has.
fn headline(code: &str) -> String {
    let lines: Vec<&str> = code
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let Some(first) = lines.first() else {
        return "empty script".to_owned();
    };
    let shown = first
        .strip_prefix("//")
        .map_or(*first, str::trim)
        .chars()
        .take(100)
        .collect::<String>();
    match lines.len() {
        1 => shown,
        count => format!("{shown} ({count} lines)"),
    }
}

#[async_trait]
impl Tool for Script {
    /// Returns `script`.
    fn name(&self) -> &'static str {
        NAME
    }

    /// Describes the tool, its limits and the tools a script can call.
    fn description(&self) -> &str {
        &self.description
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "code": { "type": "string", "description": "The body of an async JavaScript function; `tools`, `print` and `console.log` are in scope." },
                "timeout": { "type": "integer", "minimum": 1, "maximum": MAX_SECONDS, "description": format!("Wall time limit in seconds. Defaults to {DEFAULT_SECONDS}.") }
            },
            "required": ["code"]
        })
    }

    /// Whether every tool a script may call only looks.
    fn read_only(&self) -> bool {
        self.read_only
    }

    /// Shows the script's opening comment, or its first line.
    fn describe(&self, args: &Value) -> String {
        headline(args["code"].as_str().unwrap_or_default())
    }

    /// The same tool, letting scripts call the tools of `tools`.
    fn fitted(&self, tools: &Registry) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self::with_tools(self.spill.clone(), tools)))
    }

    /// Runs the script without any tool to call, as a call made outside the agent loop.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_through("", args, &NoCaller).await
    }

    /// Runs the script, its tool calls carried out by `caller`, and returns what it printed and returned.
    ///
    /// A script can call the tools it was fitted to that `caller` also has.
    async fn call_through(
        &self,
        _id: &str,
        args: Value,
        caller: &dyn Caller,
    ) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let names: Vec<String> = self
            .exposed
            .iter()
            .filter(|name| caller.has(name))
            .cloned()
            .collect();
        let limits = Limits {
            time: Duration::from_secs(
                args.timeout
                    .unwrap_or(DEFAULT_SECONDS)
                    .clamp(1, MAX_SECONDS),
            ),
            memory: MEMORY_MIB << 20,
            calls: MAX_CALLS,
            printed: PRINTED_LIMIT,
        };
        let finished = execute(args.code, names, limits, caller).await?;
        report(&self.spill, finished)
    }
}

#[cfg(test)]
mod tests;
