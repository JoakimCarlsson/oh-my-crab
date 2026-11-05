//! The tool-calling loop.

use std::sync::Arc;

use futures_util::future::join_all;
use omc_providers::{
    ChatModel, Effort, Message, Part, ProviderError, Reply, Request, Sink, Tokens, ToolCall,
    ToolResult, ToolSpec,
};
use omc_tools::{Checkpoint, FileChange, Notebook, Registry, sanitize};
use thiserror::Error;

use crate::{ApprovalRequest, Approvals, Scribe};

/// Something the UI should show while a turn runs.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// A new block of the reply starts, of the kind given; the chunks that follow belong to it.
    Started(Part),
    /// A chunk of streamed assistant text.
    Delta(String),
    /// A chunk of what the model is thinking.
    Thought(String),
    /// What a model request cost.
    Used(Tokens),
    /// The model asked for a tool call.
    ToolStart {
        /// Id of the call.
        id: String,
        /// Tool name.
        name: String,
        /// One-line description of the call.
        summary: String,
    },
    /// A subagent was started by the tool call `id`.
    Spawned {
        /// Id of the `task` call that started it.
        id: String,
        /// The kind of subagent.
        agent: String,
    },
    /// Something a subagent did, as the `task` call `parent` ran it.
    ///
    /// A call that starts several subagents reports each as a `task` step of its own, whose id is the subagent's place in the list counted from one; what that subagent does is nested once more, under its step.
    Nested {
        /// Id of the `task` call, or of the step within it.
        parent: String,
        /// The kind of subagent.
        agent: String,
        /// What it did.
        event: Box<AgentEvent>,
    },
    /// A tool call finished, successfully or not.
    ToolDone {
        /// Id of the call.
        id: String,
        /// Tool name.
        name: String,
        /// Result text or error description.
        text: String,
        /// Whether the call failed.
        is_error: bool,
        /// The files the call changed.
        changes: Vec<FileChange>,
    },
}

/// Why a turn ended without a final answer.
#[derive(Debug, Error)]
pub enum AgentError {
    /// The model request failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),
    /// The model answered the prompt with nothing: no text and no tool calls.
    #[error("the model returned an empty answer; the provider may have filtered or dropped it")]
    Empty,
}

/// The tools and system prompt to use for the next request, when they can change while a turn runs.
pub struct Live {
    /// The tools the model may call.
    pub registry: Arc<Registry>,
    /// The system prompt.
    pub system: Arc<str>,
}

/// What a turn runs with.
pub struct Context<'a> {
    /// The model that answers.
    pub model: &'a dyn ChatModel,
    /// The tools it may call.
    pub registry: &'a Registry,
    /// What is allowed without asking.
    pub approvals: &'a Approvals,
    /// The system prompt.
    pub system: &'a str,
    /// How hard the model is asked to think.
    pub effort: Option<Effort>,
    /// Gives the tools and system prompt to use for the next request when they can change during the turn, instead of `registry` and `system`.
    pub refresh: Option<&'a (dyn Fn() -> Live + Send + Sync)>,
    /// The notebook whose reset requests are carried out when a batch of tool calls has finished, by dropping the history for a message holding the notes.
    pub notebook: Option<&'a Notebook>,
    /// The checkpoint whose rewind requests are carried out when a batch of tool calls has finished, by replacing the history since the checkpoint with the report. It is forgotten when the turn ends.
    pub checkpoint: Option<&'a Checkpoint>,
    /// The size of the model's context window in tokens, when it is known; the agent is reminded to save its notes when a request fills most of it.
    pub window: Option<u64>,
}

/// Passes the chunks of a reply on as events.
struct Chunks<'a> {
    /// Where the events go.
    events: &'a (dyn Fn(AgentEvent) + Send + Sync),
}

impl Sink for Chunks<'_> {
    /// Reports that a new block of the reply starts.
    fn start(&mut self, part: Part) {
        (self.events)(AgentEvent::Started(part));
    }

    /// Reports text as it streams in.
    fn text(&mut self, chunk: &str) {
        (self.events)(AgentEvent::Delta(chunk.to_owned()));
    }

    /// Reports thinking as it streams in.
    fn thinking(&mut self, chunk: &str) {
        (self.events)(AgentEvent::Thought(chunk.to_owned()));
    }
}

/// The share of the context window, in percent, at which the agent is reminded to save its notes.
const REMINDER_PERCENT: u64 = 80;

/// Added to a batch of tool results when the context window is nearly full.
const CONTEXT_REMINDER: &str = "

<context-window-reminder>
The current context window is nearing its rollover threshold. Before important details fall out of the live tail, use context_notes to save a concise durable notebook: current task state, decisions, changed files, evidence, blockers, and exact next steps. Keep the notebook a compact index with one line per item, no copied logs or file bodies, and replace stale entries instead of appending so it stays well under the 16 KiB cap. Do not call new_context until the notebook is current.
</context-window-reminder>";

/// Replaces the history since a checkpoint with `report`: the history is cut back to `length` messages, which end with the results of the `checkpoint` call, and a user message holding the report follows.
///
/// The `rewind` call is dropped with the exploration, but the other calls of its batch have already run, so their calls and results are kept after the checkpoint, before the report. A journal that cannot be written to is reported on standard error and does not stop the turn.
fn rewind(history: &mut Vec<Message>, length: usize, report: &str, scribe: Option<&Scribe>) {
    let [.., Message::Assistant(reply), Message::ToolResults(results)] = history.as_slice() else {
        return;
    };
    let siblings: Vec<ToolResult> = results
        .iter()
        .filter(|result| result.name != "rewind")
        .cloned()
        .collect();
    let calls: Vec<ToolCall> = reply
        .tool_calls
        .iter()
        .filter(|call| siblings.iter().any(|result| result.call_id == call.id))
        .cloned()
        .collect();
    let mut kept = Vec::new();
    if !calls.is_empty() {
        kept.push(Message::Assistant(Reply {
            tool_calls: calls,
            tokens: None,
            ..reply.clone()
        }));
        kept.push(Message::ToolResults(siblings));
    }
    kept.push(Message::User(format!(
        "Checkpoint called and rewound. The messages since the checkpoint were removed from the conversation and your report stands in their place. The investigation is finished; carry on from the report, and call checkpoint again only to start a new one.\n\nReport:\n{report}"
    )));
    history.truncate(length);
    history.extend(kept.iter().cloned());
    if let Some(Err(error)) = scribe.map(|scribe| scribe.replace(length, &kept)) {
        eprintln!("history not saved: {error}");
    }
}

/// Runs one user turn: sends `prompt`, executes tool calls and loops until the
/// model answers without calling a tool.
///
/// Tools that need approval run only when `approvals` allows the call; a denied
/// call is answered to the model with an error result. New messages are appended to `history`. When the turn fails, `history` is
/// restored to what it was before the call, so it never holds a tool call
/// without its result. Tool failures are not errors here: they are sent to the
/// model as results. `events` observes streamed text and tool activity.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when a model request fails, and
/// [`AgentError::Empty`] when the model answers the prompt with neither text
/// nor tool calls.
///
/// A turn has no request limit: it ends when the model stops calling tools or
/// when the task running it is aborted.
pub async fn run_turn(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    prompt: String,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Result<(), AgentError> {
    run_turn_recorded(context, history, prompt, events, None).await
}

/// Runs a turn like [`run_turn`], and appends what is added to `history` to `scribe`'s journal as it happens: the prompt, each model reply and each batch of tool results.
///
/// When the turn fails, the journal is cut back to the history before the turn, or to the notes message when the turn started a fresh context window. The checkpoint, when there is one, is forgotten when the turn ends. The journal is synced when the turn ends. A journal that cannot be written to is reported on standard error and does not stop the turn.
///
/// # Errors
///
/// Same as [`run_turn`].
pub async fn run_turn_recorded(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    prompt: String,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) -> Result<(), AgentError> {
    let start = history.len();
    let resets = context.notebook.map_or(0, Notebook::resets);
    history.push(Message::User(prompt));
    record(scribe, &history[start]);
    let result = drive(context, history, events, scribe).await;
    if let Some(checkpoint) = context.checkpoint {
        checkpoint.clear();
    }
    if result.is_err() {
        let keep = if context.notebook.map_or(0, Notebook::resets) == resets {
            start
        } else {
            1
        };
        history.truncate(keep);
        let reason = result
            .as_ref()
            .err()
            .map(ToString::to_string)
            .unwrap_or_default();
        if let Some(Err(error)) = scribe.map(|scribe| scribe.cut_losing(keep, &reason)) {
            eprintln!("history not saved: {error}");
        }
    }
    if let Some(Err(error)) = scribe.map(Scribe::sync) {
        eprintln!("history not saved: {error}");
    }
    result
}

/// Appends `message` to the journal of `scribe`, when there is one, and reports a failure on standard error.
fn record(scribe: Option<&Scribe>, message: &Message) {
    if let Some(Err(error)) = scribe.map(|scribe| scribe.append(message)) {
        eprintln!("history not saved: {error}");
    }
}

/// Whether `error` is the provider refusing the request itself, as it does
/// when the model takes no image input, rather than a limit, a sign-in
/// problem or a failure to reach it.
fn refuses_request(error: &ProviderError) -> bool {
    let ProviderError::Status { status, .. } = error else {
        return false;
    };
    status.starts_with('4')
        && !["401", "403", "429"]
            .iter()
            .any(|code| status.starts_with(code))
}

/// Takes every image out of the tool results in `history`, leaving a note in
/// each result that had some, so that a model without image input can go on;
/// whether there was any image to take out.
fn strip_images(history: &mut [Message]) -> bool {
    let mut any = false;
    for message in history.iter_mut() {
        let Message::ToolResults(results) = message else {
            continue;
        };
        for result in results
            .iter_mut()
            .filter(|result| !result.images.is_empty())
        {
            result.text.push_str(&format!(
                "\n[{} image(s) removed: the model does not accept image input]",
                result.images.len()
            ));
            result.images.clear();
            any = true;
        }
    }
    any
}

/// Whether `reply` holds nothing for the reader or the turn: no text beyond
/// whitespace and no tool calls.
fn is_empty(reply: &Reply) -> bool {
    reply.text.trim().is_empty() && reply.tool_calls.is_empty()
}

/// Repeats model request and tool execution until no tool is requested, recording each step with `scribe`.
///
/// # Errors
///
/// Same as [`run_turn`], and [`AgentError::Empty`] when the first reply of
/// the turn is empty.
///
/// When the provider refuses a request while the history holds images, the
/// images are taken out, with a note left in their place, and the request is
/// sent once more, since the usual cause is a model without image input.
async fn drive(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) -> Result<(), AgentError> {
    let mut saved = context.notebook.map_or(0, Notebook::revision);
    let mut reminded = false;
    let mut first = true;
    let mut stripped = false;
    loop {
        let live = context.refresh.map(|refresh| refresh());
        let registry = live
            .as_ref()
            .map_or(context.registry, |live| &*live.registry);
        let system = live.as_ref().map_or(context.system, |live| &*live.system);
        let specs = tool_specs(registry);
        let mut chunks = Chunks { events };
        let request = Request {
            system,
            messages: history,
            tools: &specs,
            effort: context.effort,
        };
        let reply = match context.model.complete(request, &mut chunks).await {
            Ok(reply) => reply,
            Err(error) if !stripped && refuses_request(&error) && strip_images(history) => {
                stripped = true;
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        if std::mem::take(&mut first) && is_empty(&reply) {
            return Err(AgentError::Empty);
        }
        let nearly_full = reply
            .tokens
            .zip(context.window)
            .is_some_and(|(tokens, window)| tokens.context() >= window / 100 * REMINDER_PERCENT);
        if let Some(tokens) = reply.tokens {
            events(AgentEvent::Used(tokens));
        }
        let calls = reply.tool_calls.clone();
        history.push(Message::Assistant(reply));
        record(scribe, &history[history.len() - 1]);
        if calls.is_empty() {
            return Ok(());
        }
        let changed: std::sync::Mutex<Vec<(String, Vec<FileChange>)>> =
            std::sync::Mutex::new(Vec::new());
        let capture = |event: AgentEvent| {
            if let AgentEvent::ToolDone { id, changes, .. } = &event
                && !changes.is_empty()
            {
                changed
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .push((id.clone(), changes.clone()));
            }
            events(event);
        };
        let mut results = run_calls(registry, context.approvals, &calls, &capture).await;
        for (call, files) in changed
            .into_inner()
            .unwrap_or_else(|held| held.into_inner())
        {
            if let Some(Err(error)) = scribe.map(|scribe| scribe.changes(&call, &files)) {
                eprintln!("history not saved: {error}");
            }
        }
        if nearly_full && !reminded && context.notebook.is_some() {
            reminded = true;
            if let Some(last) = results.last_mut() {
                last.text.push_str(CONTEXT_REMINDER);
            }
        }
        history.push(Message::ToolResults(results));
        record(scribe, &history[history.len() - 1]);
        if let Some(checkpoint) = context.checkpoint {
            checkpoint.settle(history.len());
            if let Some((length, report)) = checkpoint.take_rewind() {
                rewind(history, length, &report, scribe);
            }
        }
        if let Some(notebook) = context.notebook
            && notebook.revision() != saved
        {
            saved = notebook.revision();
            if let Some(Err(error)) = scribe.map(|scribe| scribe.notes(&notebook.read())) {
                eprintln!("notes not saved: {error}");
            }
        }
        if let Some(opening) = context.notebook.and_then(Notebook::take_reset) {
            reminded = false;
            if let Some(checkpoint) = context.checkpoint {
                checkpoint.clear();
            }
            let opening = Message::User(opening);
            if let Some(Err(error)) = scribe.map(|scribe| scribe.restart(&opening)) {
                eprintln!("history not saved: {error}");
            }
            history.clear();
            history.push(opening);
        }
    }
}

/// Describes every registered tool in the provider-neutral form.
fn tool_specs(registry: &Registry) -> Vec<ToolSpec> {
    registry
        .iter()
        .map(|tool| ToolSpec {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            schema: tool.schema(),
        })
        .collect()
}

/// Runs the calls of one model reply and returns their results in call order.
///
/// Every call is announced first. Calls that need approval are asked about one
/// at a time, in order. Approved calls then run one after another so edits
/// cannot race, while calls that need no approval run concurrently.
async fn run_calls(
    registry: &Registry,
    approvals: &Approvals,
    calls: &[ToolCall],
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Vec<ToolResult> {
    let mut slots: Vec<Option<ToolResult>> = vec![None; calls.len()];
    let mut concurrent = Vec::new();
    let mut sequential = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        let tool = registry.get(&call.name);
        let summary = sanitize(&tool.map_or_else(
            || call.arguments.to_string(),
            |t| t.describe(&call.arguments),
        ));
        events(AgentEvent::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            summary: summary.clone(),
        });
        match tool {
            None => {
                let message = format!("unknown tool: {}", call.name);
                slots[index] = Some(finish(call, message, true, Vec::new(), events));
            }
            Some(tool) if let Some(scope) = tool.approval_scope(&call.arguments) => {
                let request = ApprovalRequest {
                    tool: call.name.clone(),
                    scope,
                    summary,
                };
                if approvals.allows(&request).await {
                    sequential.push(index);
                } else {
                    let message = "the user denied this tool call".to_string();
                    slots[index] = Some(finish(call, message, true, Vec::new(), events));
                }
            }
            Some(_) => concurrent.push(index),
        }
    }
    let done = join_all(
        concurrent
            .iter()
            .map(|&index| run_call(registry, &calls[index], events)),
    )
    .await;
    for (index, result) in concurrent.into_iter().zip(done) {
        slots[index] = Some(result);
    }
    for index in sequential {
        slots[index] = Some(run_call(registry, &calls[index], events).await);
    }
    slots.into_iter().flatten().collect()
}

/// Runs one approved tool call and reports its outcome.
///
/// A failing tool yields a result with `is_error` set.
async fn run_call(
    registry: &Registry,
    call: &ToolCall,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> ToolResult {
    match registry
        .call_as(&call.id, &call.name, call.arguments.clone())
        .await
    {
        Ok(output) => {
            let images = output.images.into_iter().map(image).collect();
            let mut result = finish(call, output.text, false, output.changes, events);
            result.images = images;
            result
        }
        Err(error) => finish(call, error.to_string(), true, Vec::new(), events),
    }
}

/// Converts an image a tool returned into the form sent to the model.
fn image(image: omc_tools::Image) -> omc_providers::Image {
    omc_providers::Image {
        media_type: image.media_type,
        data: image.data,
    }
}

/// Reports the outcome of `call` through `events` and builds its result.
fn finish(
    call: &ToolCall,
    text: String,
    is_error: bool,
    changes: Vec<FileChange>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> ToolResult {
    events(AgentEvent::ToolDone {
        id: call.id.clone(),
        name: call.name.clone(),
        text: text.clone(),
        is_error,
        changes,
    });
    ToolResult {
        call_id: call.id.clone(),
        name: call.name.clone(),
        text,
        is_error,
        images: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::{Decision, Journal, Prompter};

    /// The start of each block is passed on as an event before its chunks.
    #[test]
    fn block_starts_are_reported() {
        let seen = Mutex::new(Vec::new());
        let events = |event: AgentEvent| seen.lock().unwrap().push(event);
        let mut chunks = Chunks { events: &events };
        chunks.start(Part::Thinking);
        chunks.thinking("hm");
        chunks.start(Part::Text);
        chunks.text("hi");
        assert_eq!(
            *seen.lock().unwrap(),
            [
                AgentEvent::Started(Part::Thinking),
                AgentEvent::Thought("hm".into()),
                AgentEvent::Started(Part::Text),
                AgentEvent::Delta("hi".into())
            ]
        );
    }

    /// A model that plays back a fixed list of replies.
    struct Scripted {
        /// Replies still to give, in order; the last one repeats.
        replies: Mutex<Vec<Reply>>,
        /// Number of messages seen at each request.
        seen: Mutex<Vec<usize>>,
    }

    impl Scripted {
        /// Creates a model that answers with `replies` in order.
        fn new(replies: Vec<Reply>) -> Self {
            Self {
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ChatModel for Scripted {
        /// Records the history length and returns the next scripted reply.
        async fn complete(
            &self,
            request: Request<'_>,
            sink: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            self.seen.lock().unwrap().push(request.messages.len());
            let mut replies = self.replies.lock().unwrap();
            let reply = if replies.len() > 1 {
                replies.remove(0)
            } else {
                replies[0].clone()
            };
            if !reply.text.is_empty() {
                sink.text(&reply.text);
            }
            Ok(reply)
        }
    }

    /// Builds a reply that calls `tool` with `arguments`.
    fn call_reply(id: &str, tool: &str, arguments: serde_json::Value) -> Reply {
        Reply {
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: tool.into(),
                arguments,
            }],
            ..Reply::default()
        }
    }

    /// A prompter that denies every request.
    struct DenyAll;

    #[async_trait]
    impl Prompter for DenyAll {
        /// Always answers [`Decision::Deny`].
        async fn ask(&self, _request: &ApprovalRequest) -> Decision {
            Decision::Deny
        }
    }

    /// Builds a policy that denies every tool needing approval.
    fn deny_all() -> Approvals {
        Approvals::new(Arc::new(DenyAll), false)
    }

    /// Creates an empty scratch directory under the workspace's `build/`.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-agent")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The model asks for `read`, gets the file, then answers.
    #[tokio::test]
    async fn runs_a_tool_then_answers() {
        let dir = scratch("loop-read");
        std::fs::write(dir.join("hello.txt"), "hello from disk\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let model = Scripted::new(vec![
            call_reply("t1", "read", json!({ "path": "hello.txt" })),
            Reply {
                text: "it says hello".into(),
                ..Reply::default()
            },
        ]);
        let log = Mutex::new(Vec::new());
        let events = |event: AgentEvent| log.lock().unwrap().push(event);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "read it".into(),
            &events,
        )
        .await
        .unwrap();
        assert_eq!(history.len(), 4);
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results, got {:?}", history[2]);
        };
        assert!(!results[0].is_error);
        assert!(results[0].text.contains("hello from disk"));
        assert_eq!(*model.seen.lock().unwrap(), vec![1, 3]);
        let log = log.lock().unwrap();
        assert!(matches!(log[0], AgentEvent::ToolStart { .. }));
        assert!(matches!(
            log[1],
            AgentEvent::ToolDone {
                is_error: false,
                ..
            }
        ));
        assert_eq!(log[2], AgentEvent::Delta("it says hello".into()));
    }

    /// Runs one turn of `model` on the built-in tools in `dir` with `history`.
    async fn turn_of(
        model: &Scripted,
        dir: &std::path::Path,
        history: &mut Vec<Message>,
    ) -> Result<(), AgentError> {
        let registry = Registry::builtin(dir).unwrap();
        run_turn(
            &Context {
                model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            history,
            "go".into(),
            &|_| {},
        )
        .await
    }

    /// A model without image input: it refuses every request whose history
    /// holds an image, and otherwise answers with the text of the last tool
    /// result it was sent.
    struct Blind {
        /// Whether each request held an image, in order.
        saw_images: Mutex<Vec<bool>>,
    }

    #[async_trait]
    impl ChatModel for Blind {
        /// Refuses requests with images; asks to read `dot.png` first, then
        /// repeats the last tool result.
        async fn complete(
            &self,
            request: Request<'_>,
            _: &mut dyn Sink,
        ) -> Result<Reply, ProviderError> {
            let results: Vec<&ToolResult> = request
                .messages
                .iter()
                .filter_map(|message| match message {
                    Message::ToolResults(results) => Some(results),
                    _ => None,
                })
                .flatten()
                .collect();
            let images = results.iter().any(|result| !result.images.is_empty());
            self.saw_images.lock().unwrap().push(images);
            if images {
                return Err(ProviderError::Status {
                    status: "404 Not Found".into(),
                    body: "No endpoints found that support image input".into(),
                });
            }
            Ok(match results.last() {
                None => call_reply("t1", "read", json!({ "path": "dot.png" })),
                Some(result) => Reply {
                    text: result.text.clone(),
                    ..Reply::default()
                },
            })
        }
    }

    /// When the provider refuses a request because of an image, the image is
    /// replaced by a note and the request is sent once more.
    #[tokio::test]
    async fn drops_images_a_model_refuses() {
        let dir = scratch("loop-blind");
        let png = [
            0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        std::fs::write(dir.join("dot.png"), png).unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let model = Blind {
            saw_images: Mutex::new(Vec::new()),
        };
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "look".into(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(*model.saw_images.lock().unwrap(), vec![false, true, false]);
        let Message::Assistant(answer) = &history[3] else {
            panic!("expected the answer, got {:?}", history[3]);
        };
        assert!(
            answer.text.contains("1 image(s) removed"),
            "{}",
            answer.text
        );
    }

    /// A first answer with neither text nor tool calls fails the turn and
    /// leaves the history as it was; an empty answer after tool calls ends the
    /// turn normally.
    #[tokio::test]
    async fn an_empty_first_answer_fails_the_turn() {
        let dir = scratch("loop-empty");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let mut history = Vec::new();
        let blank = Reply {
            text: "  \n".into(),
            ..Reply::default()
        };
        let error = turn_of(&Scripted::new(vec![blank]), &dir, &mut history)
            .await
            .unwrap_err();
        assert!(matches!(error, AgentError::Empty), "{error}");
        assert!(history.is_empty());
        let model = Scripted::new(vec![
            call_reply("t1", "read", json!({ "path": "a.txt" })),
            Reply::default(),
        ]);
        turn_of(&model, &dir, &mut history).await.unwrap();
        assert_eq!(history.len(), 4);
    }

    /// Runs one turn in which the model calls `read` with `arguments` in `dir`,
    /// denying every approval, and returns the result of that call.
    async fn read_once(dir: &std::path::Path, arguments: serde_json::Value) -> ToolResult {
        let registry = Registry::builtin(dir).unwrap();
        let model = Scripted::new(vec![
            call_reply("t1", "read", arguments),
            Reply {
                text: "done".into(),
                ..Reply::default()
            },
        ]);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "read it".into(),
            &|_| {},
        )
        .await
        .unwrap();
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results, got {:?}", history[2]);
        };
        results[0].clone()
    }

    /// An image a tool returns reaches the result sent to the model.
    #[tokio::test]
    async fn passes_tool_images_to_the_model() {
        let dir = scratch("loop-image");
        std::fs::write(dir.join("dot.png"), b"\x89PNG\r\n\x1a\n").unwrap();
        let result = read_once(&dir, json!({ "path": "dot.png" })).await;
        assert!(!result.is_error, "{}", result.text);
        assert_eq!(result.images.len(), 1);
        assert_eq!(result.images[0].media_type, "image/png");
    }

    /// Reading a URL asks first, while reading a file does not.
    #[tokio::test]
    async fn asks_before_reading_urls() {
        let dir = scratch("loop-url");
        let result = read_once(&dir, json!({ "path": "http://127.0.0.1:9/x" })).await;
        assert!(result.is_error);
        assert!(result.text.contains("denied"), "{}", result.text);
    }

    /// Several calls in one reply all run and all get a result, tool errors included.
    #[tokio::test]
    async fn runs_parallel_calls_and_reports_failures() {
        let dir = scratch("loop-parallel");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let first = Reply {
            tool_calls: vec![
                ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: json!({ "path": "a.txt" }),
                },
                ToolCall {
                    id: "2".into(),
                    name: "read".into(),
                    arguments: json!({ "path": "missing.txt" }),
                },
                ToolCall {
                    id: "3".into(),
                    name: "nope".into(),
                    arguments: json!({}),
                },
            ],
            ..Reply::default()
        };
        let model = Scripted::new(vec![first, Reply::default()]);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "go".into(),
            &|_| {},
        )
        .await
        .unwrap();
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results");
        };
        let flags: Vec<bool> = results.iter().map(|r| r.is_error).collect();
        assert_eq!(flags, vec![false, true, true]);
        assert_eq!(results[2].call_id, "3");
    }

    /// A turn that needs many tool rounds keeps going until the model answers.
    #[tokio::test]
    async fn long_turns_are_not_cut_off() {
        let dir = scratch("long-turn");
        std::fs::write(dir.join("a.txt"), "a\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let mut replies: Vec<Reply> = (0..60)
            .map(|_| call_reply("t", "read", json!({ "path": "a.txt" })))
            .collect();
        replies.push(Reply {
            text: "done".into(),
            ..Reply::default()
        });
        let model = Scripted::new(replies);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "go".into(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(model.seen.lock().unwrap().len(), 61);
    }

    /// A denied edit does not touch the file and the model is told why.
    #[tokio::test]
    async fn denied_tools_do_not_run() {
        let dir = scratch("loop-denied");
        std::fs::write(dir.join("f.txt"), "before\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let edit = json!({ "files": [{ "path": "f.txt", "tag": omc_tools::snapshot_tag(b"before\n"), "ops": [{ "op": "replace", "line": 1, "text": "after" }] }] });
        let model = Scripted::new(vec![call_reply("e", "edit", edit), Reply::default()]);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "edit".into(),
            &|_| {},
        )
        .await
        .unwrap();
        let Message::ToolResults(results) = &history[2] else {
            panic!("expected tool results");
        };
        assert!(results[0].is_error);
        assert!(results[0].text.contains("denied"));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "before\n"
        );
    }

    /// In yolo mode the same edit runs without asking.
    #[tokio::test]
    async fn yolo_runs_tools_without_asking() {
        let dir = scratch("loop-yolo");
        std::fs::write(dir.join("f.txt"), "before\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let edit = json!({ "files": [{ "path": "f.txt", "tag": omc_tools::snapshot_tag(b"before\n"), "ops": [{ "op": "replace", "line": 1, "text": "after" }] }] });
        let model = Scripted::new(vec![call_reply("e", "edit", edit), Reply::default()]);
        let approvals = Approvals::new(Arc::new(DenyAll), true);
        let mut history = Vec::new();
        run_turn(
            &Context {
                model: &model,
                registry: &registry,
                approvals: &approvals,
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            &mut history,
            "edit".into(),
            &|_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "after\n"
        );
    }

    /// A model whose requests all fail.
    struct Failing;

    #[async_trait]
    impl ChatModel for Failing {
        /// Always fails.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            Err(ProviderError::NoModels)
        }
    }

    /// Runs one recorded turn for `prompt` against `model` with an empty registry.
    async fn recorded_turn(
        model: &dyn ChatModel,
        history: &mut Vec<Message>,
        journal: &Arc<Journal>,
        prompt: &str,
    ) -> Result<(), AgentError> {
        let registry = Registry::builtin(scratch("recorded")).unwrap();
        run_turn_recorded(
            &Context {
                model,
                registry: &registry,
                approvals: &deny_all(),
                system: "",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
            },
            history,
            prompt.into(),
            &|_| {},
            Some(&journal.scribe()),
        )
        .await
    }

    /// A failed turn is cut back in the file, and a later open continues from the history before it with the earlier turn in the request.
    #[tokio::test]
    async fn a_failed_turn_is_cut_back_in_the_file() {
        let path = scratch("journal-failed").join("c.jsonl");
        let (journal, mut history) = Journal::open(&path, None).unwrap();
        let answer = Scripted::new(vec![Reply {
            text: "fine".into(),
            ..Reply::default()
        }]);
        recorded_turn(&answer, &mut history, &journal, "one")
            .await
            .unwrap();
        assert!(
            recorded_turn(&Failing, &mut history, &journal, "two")
                .await
                .is_err()
        );
        assert_eq!(history.len(), 2);
        drop(journal);
        let (journal, mut reopened) = Journal::open(&path, None).unwrap();
        assert_eq!(reopened, history);
        recorded_turn(&answer, &mut reopened, &journal, "three")
            .await
            .unwrap();
        assert_eq!(*answer.seen.lock().unwrap(), vec![1, 3]);
    }
}
