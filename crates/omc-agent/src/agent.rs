//! The tool-calling loop.

use std::borrow::Cow;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::future::join_all;
use omc_providers::{
    ChatModel, Effort, Message, Part, ProviderError, Pruning, Reply, Request, Sink, Stop, Tokens,
    ToolCall, ToolResult, ToolSpec,
};
use omc_tools::{
    Caller, Checkpoint, Concurrency, FileChange, NoCaller, Notebook, Registry, Tool, ToolOutput,
    sanitize,
};
use serde_json::Value;
use thiserror::Error;
use tokio::sync::{RwLockReadGuard, RwLockWriteGuard};

use crate::compaction;
use crate::hooks::{CompactTrigger, Compacting, Gate, Hooks, Permit, StopBound, with_note};
use crate::loop_guard::LoopGuard;
use crate::pruning;
use crate::recall::{self, RECALL};
use crate::recovery::{self, MAX_CUT_CALLS, MAX_RECOVERIES, Retry};
use crate::shake;
use crate::steering::{self, Steering};
use crate::text_calls;
use crate::todos::TodoTracker;
use crate::{ApprovalRequest, Approvals, Scribe, Standing};

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
        /// What the model it asks is called, when that is known.
        model: Option<String>,
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
    /// The subagent started by the `task` call `id`, or by the step `id` of one, works in a git worktree of its own at `path`; sent again with what became of its changes once it ended.
    Worktree {
        /// Id of the `task` call, or of the step within it.
        id: String,
        /// The folder the subagent works in.
        path: std::path::PathBuf,
        /// What became of its changes, once it ended.
        merge: Option<crate::Merge>,
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
    /// The history could not be compacted, for this reason.
    #[error("compacting the conversation failed: {0}")]
    Compaction(String),
    /// The model's replies kept running into the output limit before their tool calls were complete, so none of them could be run.
    #[error(
        "the model's replies kept running into the output limit before their tool calls were complete"
    )]
    OutputLimit,
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
    /// The size of the model's context window in tokens, when it is known; the agent is reminded to save its notes when a request fills most of it, and the older part of the history is summarized before a request that would fill nearly all of it.
    pub window: Option<u64>,
    /// The checks the user set up around tool calls and the end of the turn, when there are any.
    pub hooks: Option<&'a dyn Hooks>,
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
/// A request that fails in a way that is likely to pass, such as an overloaded
/// provider or a dropped connection, is sent again after a wait, a few times;
/// each wait is shown through `events` as a call of the `retry` tool. A reply
/// that stops short, empty or cut off by the output limit, is answered with a
/// notice that only the model sees, a few times a turn, and the model carries
/// on. The calls of a reply the output limit cut off are not run, and nor are
/// calls whose arguments cannot be read; the model is told why in their
/// results. When the model keeps making the same calls, or keeps making edits
/// that change nothing, a notice telling it to change course is added to the
/// results; when it changes many files without updating a todo list with open
/// items, a reminder is. Nothing in the text of a reply is ever run: a call the
/// model wrote out as text only earns a notice telling it so.
///
/// A finished answer is kept before the model is sent a notice after it, to
/// make a call it wrote as text or to finish its todo list; when the request
/// that follows fails, the turn still ends with that answer.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when a model request fails for good,
/// [`AgentError::Empty`] when the model answers the prompt with neither text
/// nor tool calls even after being nudged, and [`AgentError::OutputLimit`]
/// when its replies keep running into the output limit before their tool calls
/// are complete.
///
/// A turn has no request limit: it ends when the model stops calling tools or
/// when the task running it is aborted, which also ends any wait before a
/// request is sent again.
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
    run_turn_steered(context, history, prompt, Vec::new(), events, scribe, None).await
}

/// Runs a turn like [`run_turn_recorded`], showing the model `images`, which the reader attached to `prompt`, with it, and puts the messages waiting in `steering`, when there is one, into the conversation at each safe point: after a batch of tool calls and any compaction, before the next model request. Each is added to `history` and recorded as the reader's.
///
/// A message that is still waiting when the model answers without calling a tool is left in `steering` for the caller. When the turn fails, the messages put into it are cut back with it.
///
/// # Errors
///
/// Same as [`run_turn`].
pub async fn run_turn_steered(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    prompt: String,
    images: Vec<omc_providers::Image>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
    steering: Option<&dyn Steering>,
) -> Result<(), AgentError> {
    let start = history.len();
    let resets = context.notebook.map_or(0, Notebook::resets);
    history.push(Message::User(prompt));
    if !images.is_empty() {
        history.push(Message::Images(images));
    }
    for message in &history[start..] {
        record(scribe, message);
    }
    let mut settled = None;
    let result = drive(context, history, events, scribe, steering, &mut settled).await;
    if let Some(checkpoint) = context.checkpoint {
        checkpoint.clear();
    }
    let result = match (result, settled) {
        (Err(error), Some(length)) => {
            keep_answer(history, length, &error, scribe);
            Ok(())
        }
        (result, _) => result,
    };
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

/// Ends a turn that failed with `error` after its answer was kept: cuts `history` and the journal of `scribe` back to `length` messages, which end with that answer, and reports on standard error that what followed it failed.
fn keep_answer(
    history: &mut Vec<Message>,
    length: usize,
    error: &AgentError,
    scribe: Option<&Scribe>,
) {
    history.truncate(length);
    let reason = error.to_string();
    eprintln!("the turn ended with its answer; the request after it failed: {reason}");
    if let Some(Err(error)) = scribe.map(|scribe| scribe.cut_losing(length, &reason)) {
        eprintln!("history not saved: {error}");
    }
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

/// The notice to send along with the next request when `reply` stopped short of anything to act on: empty, or cut off by the output limit before any tool call; `None` when it can be taken as it is.
fn stopped_short(reply: &Reply) -> Option<&'static str> {
    let cut = reply.stop == Stop::Length;
    match (is_empty(reply), cut) {
        (true, true) => Some(recovery::LENGTH_THINKING),
        (true, false) => Some(recovery::EMPTY_REPLY),
        (false, true) if reply.tool_calls.is_empty() => Some(recovery::LENGTH_TEXT),
        _ => None,
    }
}

/// The messages a request is made with: what the model is given of `history`, then `cut`, a reply the output limit cut off that the model is to carry on, joined to a finished answer it follows, and `notice`, when there are, at the end.
fn request_messages<'h>(
    history: &'h [Message],
    cut: Option<&Reply>,
    notice: Option<&str>,
) -> Cow<'h, [Message]> {
    let mut messages = compaction::context(history);
    if let Some(cut) = cut {
        compaction::push_reply(messages.to_mut(), cut.clone());
    }
    if let Some(notice) = notice {
        recovery::add_notice(messages.to_mut(), notice);
    }
    messages
}

/// Answers each of `calls` with the error `text` without running it, and reports each through `events` as started and failed.
fn refuse(
    calls: &[ToolCall],
    text: &str,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Vec<ToolResult> {
    calls
        .iter()
        .map(|call| {
            events(AgentEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
                summary: "cut off by the output limit".to_owned(),
            });
            finish(call, text.to_owned(), true, Vec::new(), events)
        })
        .collect()
}

/// Adds `notice` to the end of the last of `results`, where the model reads it with them.
fn append_notice(results: &mut [ToolResult], notice: &str) {
    if let Some(last) = results.last_mut() {
        last.text.push_str("\n\n");
        last.text.push_str(notice.trim());
    }
}

/// Repeats model request and tool execution until no tool is requested, recording each step with `scribe` and putting the messages waiting in `steering` into the history before each request that does not carry on a cut-off reply or follow up on a finished answer.
///
/// `settled` is set to the length of the history once it ends with a finished answer that a notice follows up on, and cleared when a rewind or a fresh context window replaces that part of the history; a turn that fails after that is to be ended with the answer rather than undone.
///
/// # Errors
///
/// Same as [`run_turn`], and [`AgentError::Empty`] when the first reply of
/// the turn is empty and stays so after the model was nudged.
///
/// A request that fails in a way likely to pass is sent again after a wait that doubles each time, or that the provider asked for, up to [`recovery::MAX_RETRIES`] times; the wait is shown as a `retry` call and ends early when the turn is aborted. Nothing of a failed request reaches the history, so nothing is repeated in it.
///
/// A reply that is empty, or that the output limit cut off before any tool call, is not taken: the request is made again with a notice to the model added to its end, which is not kept in the history, up to [`MAX_RECOVERIES`] times a turn. A reply cut off part of the way through its text is carried on by the next one, the two kept as one. The calls of a reply cut off in its tool calls are answered with an error without being run, the last result telling the model to take a smaller step, up to [`MAX_CUT_CALLS`] times a turn, counted apart; past that the turn fails.
///
/// The batches of calls are watched for circles: the same calls made again and again, or edits that change nothing, earn a notice to the model at the end of the batch's results.
///
/// The model's todo list is watched too: when it still has open items after [`crate::todos::CHANGES`] calls that changed files since it last changed, a reminder listing them is added to the batch's results, at most [`crate::todos::MAX_REMINDERS`] times between changes; and a turn that wrote the list or changed files and is about to end with open items is followed up once with a notice to finish them or update the list. Neither happens when the model's reply ends by asking the user a question.
///
/// A reply without tool calls whose text holds a call written out as text is never run; it is followed up once a turn with a notice telling the model to make the call if it meant to.
///
/// A follow-up keeps the finished answer in the history and sends its notice with the next request only; the model's next reply is kept as a reply of its own, and an empty one ends the turn.
///
/// When the provider refuses a request while the history holds images, the
/// images are taken out, with a note left in their place, and the request is
/// sent once more, since the usual cause is a model without image input.
///
/// When the window is known and the context has grown past the compaction threshold, old tool outputs are left out and the heavy content of all but the recent messages is shaken out, and when that is not enough the older part of the history is summarized before the next request; a summary that cannot be made is shown and not tried again this turn. When the provider says a request holds more than the window takes, the history is compacted and the request sent once more, once a turn.
async fn drive(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
    steering: Option<&dyn Steering>,
    settled: &mut Option<usize>,
) -> Result<(), AgentError> {
    let mut saved = context.notebook.map_or(0, Notebook::revision);
    let mut reminded = false;
    let mut first = true;
    let mut stripped = false;
    let mut overflowed = false;
    let mut compaction_failed = false;
    let mut retries = 0;
    let mut recoveries = 0;
    let mut cut_calls = 0;
    let mut written_noticed = false;
    let mut following_up = false;
    let mut notice: Option<Cow<'static, str>> = None;
    let mut cut: Option<Reply> = None;
    let mut guard = LoopGuard::default();
    let mut todos = TodoTracker::new(history);
    let mut stops = StopBound::default();
    loop {
        let live = context.refresh.map(|refresh| refresh());
        let registry = live
            .as_ref()
            .map_or(context.registry, |live| &*live.registry);
        let system = live.as_ref().map_or(context.system, |live| &*live.system);
        if let Some(pruning) = pruning::stale(history) {
            prune_history(history, pruning, system, events, scribe);
        }
        if !compaction_failed
            && over_threshold(context, history, system)
            && make_room(context, history, system, false, events, scribe)
                .await
                .is_err()
        {
            compaction_failed = true;
        }
        if cut.is_none() && !following_up {
            steering::inject(steering, history, scribe);
        }
        let mut specs = tool_specs(registry);
        if recall::offered(history) && registry.get(RECALL).is_none() {
            specs.push(recall::spec());
        }
        let mut chunks = Chunks { events };
        let answered = {
            let messages = request_messages(history, cut.as_ref(), notice.as_deref());
            let request = Request {
                system,
                messages: &messages,
                tools: &specs,
                effort: context.effort,
            };
            context.model.complete(request, &mut chunks).await
        };
        let reply = match answered {
            Ok(reply) => reply,
            Err(error) if !overflowed && error.is_context_overflow() => {
                overflowed = true;
                match make_room(context, history, system, true, events, scribe).await {
                    Ok(true) => continue,
                    Ok(false) => return Err(error.into()),
                    Err(reason) => {
                        return Err(AgentError::Compaction(format!(
                            "{reason}, after the provider refused the request as too long: {error}"
                        )));
                    }
                }
            }
            Err(error) if !stripped && refuses_request(&error) && strip_images(history) => {
                stripped = true;
                continue;
            }
            Err(error) if let Some(wait) = recovery::retry_wait(&error, retries + 1) => {
                retries += 1;
                let retry = Retry::new(retries, wait);
                events(retry.started(&error));
                tokio::time::sleep(wait).await;
                events(retry.finished(&error));
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        retries = 0;
        if let Some(tokens) = reply.tokens {
            events(AgentEvent::Used(tokens));
        }
        let reply = match cut.take() {
            Some(earlier) => recovery::joined(earlier, reply),
            None => reply,
        };
        notice = None;
        if std::mem::take(&mut following_up) && is_empty(&reply) && reply.stop != Stop::Length {
            return Ok(());
        }
        if recoveries < MAX_RECOVERIES
            && let Some(next) = stopped_short(&reply)
        {
            recoveries += 1;
            notice = Some(Cow::Borrowed(next));
            if !is_empty(&reply) {
                cut = Some(reply);
            }
            continue;
        }
        if first && is_empty(&reply) {
            return Err(AgentError::Empty);
        }
        first = false;
        let nearly_full = reply
            .tokens
            .zip(context.window)
            .is_some_and(|(tokens, window)| tokens.context() >= window / 100 * REMINDER_PERCENT);
        let calls = reply.tool_calls.clone();
        let said = reply.text.clone();
        let truncated = reply.stop == Stop::Length;
        if truncated && !calls.is_empty() && cut_calls >= MAX_CUT_CALLS {
            return Err(AgentError::OutputLimit);
        }
        let follow_up = if calls.is_empty() {
            match follow_up(&reply, registry, &mut written_noticed, &mut todos) {
                Some(notice) => Some(notice),
                None if reply.stop != Stop::Length => stops.next(context.hooks, &said).await,
                None => None,
            }
        } else {
            stops.tools_called();
            None
        };
        if let Some(hooks) = context.hooks {
            hooks.replied(&reply).await;
        }
        history.push(Message::Assistant(reply));
        record(scribe, &history[history.len() - 1]);
        if calls.is_empty() {
            let Some(follow_up) = follow_up else {
                return Ok(());
            };
            *settled = Some(history.len());
            following_up = true;
            notice = Some(Cow::Owned(follow_up));
            continue;
        }
        if truncated {
            cut_calls += 1;
            let mut results = refuse(&calls, recovery::CUT_CALL, events);
            append_notice(&mut results, recovery::LENGTH_CALLS);
            history.push(Message::ToolResults(results));
            record(scribe, &history[history.len() - 1]);
            continue;
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
        let (mut results, mut halt) = run_calls(
            registry,
            context.approvals,
            context.hooks,
            history,
            &calls,
            &capture,
        )
        .await;
        if let Some(hooks) = context.hooks {
            let batched = hooks.after_batch(&calls, &results).await;
            if let Some(note) = batched.note {
                append_notice(&mut results, &with_note(String::new(), &note));
            }
            halt = halt.or(batched.halt);
        }
        if let Some(reason) = halt {
            let reason = reason.trim();
            eprintln!("a hook stopped the turn: {reason}");
            append_notice(
                &mut results,
                &with_note(String::new(), &format!("A hook stopped the turn: {reason}")),
            );
            history.push(Message::ToolResults(results));
            record(scribe, &history[history.len() - 1]);
            return Ok(());
        }
        let changed = changed
            .into_inner()
            .unwrap_or_else(|held| held.into_inner());
        for (call, files) in &changed {
            if let Some(Err(error)) = scribe.map(|scribe| scribe.changes(call, files)) {
                eprintln!("history not saved: {error}");
            }
        }
        if let Some(redirect) = guard.observe(&calls, &results, &changed) {
            append_notice(&mut results, &redirect);
        }
        if let Some(reminder) = todos.observe(&calls, &results, &changed, &said) {
            append_notice(&mut results, &reminder);
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
                *settled = None;
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
            *settled = None;
        }
    }
}

/// The notice to follow up `answer`, a reply without tool calls that would end the turn, with: that it wrote a call of a tool in `registry` out as text, when it did and the model was not yet told so this turn, which `written_noticed` keeps; otherwise, for a finished answer, that the list `todos` tracks still has open items, once a turn. Nothing when the turn is to end.
fn follow_up(
    answer: &Reply,
    registry: &Registry,
    written_noticed: &mut bool,
    todos: &mut TodoTracker,
) -> Option<String> {
    if !*written_noticed
        && let Some(written) = text_calls::notice(answer, &|name| registry.get(name).is_some())
    {
        *written_noticed = true;
        return Some(written);
    }
    (answer.stop == Stop::Finished)
        .then(|| todos.finishing(answer))
        .flatten()
}

/// Compacts `history` now, as the reader asked: summarizes all but the most recent messages, giving particular attention to `focus` when there is one, and records the compaction with `scribe`, which is synced when it is done.
///
/// When the compaction fails, the journal records the turn that asked for it as lost for that reason, as it does for a failed turn, so a reopened conversation still shows it.
///
/// # Errors
///
/// Returns [`AgentError::Compaction`] when the history holds nothing old enough to summarize, or when the summary could not be made; the history is then left as it was.
pub async fn compact(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    focus: Option<&str>,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) -> Result<(), AgentError> {
    let live = context.refresh.map(|refresh| refresh());
    let system = live.as_ref().map_or(context.system, |live| &*live.system);
    let result = match compact_history(
        context,
        history,
        system,
        (CompactTrigger::Manual, focus),
        events,
        scribe,
    )
    .await
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(AgentError::Compaction(
            "there is nothing to compact yet: the conversation is no longer than the recent messages a compaction keeps".to_owned(),
        )),
        Err(reason) => Err(AgentError::Compaction(reason)),
    };
    if let (Err(error), Some(scribe)) = (&result, scribe)
        && let Err(error) = scribe.cut_losing(history.len(), &error.to_string())
    {
        eprintln!("history not saved: {error}");
    }
    if let Some(Err(error)) = scribe.map(Scribe::sync) {
        eprintln!("history not saved: {error}");
    }
    result
}

/// Whether the window is known and the context of `history`, sent with the system prompt `system`, has grown past the compaction threshold.
fn over_threshold(context: &Context<'_>, history: &[Message], system: &str) -> bool {
    context.window.is_some_and(|window| {
        compaction::context_tokens(history, system) > compaction::threshold(window)
    })
}

/// Makes room in the context of `history`, sent with the system prompt `system`: leaves old tool outputs out first; then, when that is not enough to bring the context under the compaction threshold, shakes the heavy outputs and fenced blocks out of all but the recent messages; and summarizes the older part of the history when that is still not enough, or always when `overflowed`, since the provider has refused a request as too long and the estimate cannot be trusted.
///
/// Returns whether anything was left out or summarized.
///
/// # Errors
///
/// Returns why, for the reader, when a summary was needed and could not be made; what was left out stays left out.
async fn make_room(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    system: &str,
    overflowed: bool,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) -> Result<bool, String> {
    let pruned = match pruning::prune(history) {
        Some(pruning) => {
            prune_history(history, pruning, system, events, scribe);
            true
        }
        None => false,
    };
    if pruned && !overflowed && !over_threshold(context, history, system) {
        return Ok(true);
    }
    let shaken = match shake::shake(history, compaction::keep_recent(context.window)) {
        Some(shaking) => {
            prune_history(history, shaking, system, events, scribe);
            true
        }
        None => false,
    };
    if shaken && !overflowed && !over_threshold(context, history, system) {
        return Ok(true);
    }
    let compacted = compact_history(
        context,
        history,
        system,
        (CompactTrigger::Auto, None),
        events,
        scribe,
    )
    .await?;
    Ok(compacted || pruned || shaken)
}

/// Appends `pruning` to `history` and to `scribe`'s journal, with how full the context of `history`, sent with the system prompt `system`, is after it, and shows that through `events`.
fn prune_history(
    history: &mut Vec<Message>,
    pruning: Pruning,
    system: &str,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) {
    history.push(Message::Pruning(pruning));
    let tokens_after = compaction::estimated(history, system);
    let at = history.len() - 1;
    if let Message::Pruning(pruning) = &mut history[at] {
        pruning.tokens_after = tokens_after;
    }
    record(scribe, &history[at]);
    events(AgentEvent::Used(Tokens {
        input: tokens_after,
        ..Tokens::default()
    }));
}

/// Summarizes the older part of `history`, whose context is sent with the system prompt `system`, and appends the compaction to it and to `scribe`'s journal; `asked` says what started it and what the summary should give particular attention to.
///
/// The hooks of `context` are asked first, once there is something to summarize: they may block the compaction or add instructions for the summary. The compaction is shown through `events` as a call of the `compact` tool, followed by how full the context is after it. Returns whether the history was compacted: it is not when nothing is old enough to summarize.
///
/// # Errors
///
/// Returns why, for the reader, when a hook blocked the compaction or the summary could not be made; the history is then left as it was.
async fn compact_history(
    context: &Context<'_>,
    history: &mut Vec<Message>,
    system: &str,
    asked: (CompactTrigger, Option<&str>),
    events: &(dyn Fn(AgentEvent) + Send + Sync),
    scribe: Option<&Scribe>,
) -> Result<bool, String> {
    let (trigger, focus) = asked;
    let tokens = compaction::context_tokens(history, system);
    let Some(plan) = compaction::plan(history, compaction::keep_recent(context.window), tokens)
    else {
        return Ok(false);
    };
    let focus = match context.hooks {
        Some(hooks) => match hooks.before_compact(trigger, focus).await {
            Compacting::Go(added) => joined_focus(focus, added),
            Compacting::Blocked(reason) => {
                return Err(format!(
                    "a PreCompact hook blocked the compaction: {}",
                    reason.trim()
                ));
            }
        },
        None => focus.map(str::to_owned),
    };
    let at = history.len();
    events(compaction::started(at, tokens));
    match compaction::summarize(
        context.model,
        context.effort,
        history,
        &plan,
        focus.as_deref(),
    )
    .await
    {
        Ok(done) => {
            let summary = done.summary.clone();
            history.push(Message::Compaction(done));
            let tokens_after = compaction::estimated(history, system);
            if let Message::Compaction(done) = &mut history[at] {
                done.tokens_after = tokens_after;
            }
            record(scribe, &history[at]);
            if let Some(hooks) = context.hooks {
                hooks.after_compact(trigger, &summary).await;
            }
            events(compaction::finished(at, summary, false));
            events(AgentEvent::Used(Tokens {
                input: tokens_after,
                ..Tokens::default()
            }));
            Ok(true)
        }
        Err(reason) => {
            events(compaction::finished(at, reason.clone(), true));
            Err(reason)
        }
    }
}

/// What the summary of a compaction should give particular attention to: the reader's `focus` and the instructions hooks `added`, one after the other, or nothing when there is neither.
fn joined_focus(focus: Option<&str>, added: Option<String>) -> Option<String> {
    match (
        focus.map(str::trim).filter(|focus| !focus.is_empty()),
        added,
    ) {
        (Some(focus), Some(added)) => Some(format!("{focus}\n\n{}", added.trim())),
        (Some(focus), None) => Some(focus.to_owned()),
        (None, added) => added.map(|added| added.trim().to_owned()),
    }
}

/// Describes every registered tool on the tool list in the provider-neutral form; the tools left out until the model looks them up with `load_tools` are not described, though they run like the rest when called.
fn tool_specs(registry: &Registry) -> Vec<ToolSpec> {
    registry
        .offered()
        .map(|tool| ToolSpec {
            name: tool.name().to_string(),
            description: tool.description().to_string(),
            schema: tool.schema(),
        })
        .collect()
}

/// Runs the calls of one model reply and returns their results in call order, and why the turn stops after them when a hook asked for that.
///
/// Every call is announced first. The arguments of a call of a registered tool
/// are read and put right against its schema once, then shown to `hooks`, which
/// may deny the call, let it run without asking, have the reader asked or rewrite its arguments, which
/// are put right against the schema again; only then is the call described,
/// asked about or run, so the user approves exactly what runs. A call whose
/// arguments cannot be read or put right fails without being asked about.
/// Every call is put to the permission rules: one a deny rule matches fails
/// without running, in yolo mode too. Calls that need approval, and calls an
/// ask rule matches, are asked about one at a time, in order, unless a hook
/// allowed them or allow rules cover them; before the reader is asked, the hooks
/// may decide in the reader's place, and every question is answered before
/// any call runs. The calls that may run then run in the model's order, each
/// run of [`Concurrency::Shared`] calls next to each other at once and each
/// [`Concurrency::Exclusive`] call alone, so that what the calls change happens
/// in the order the model asked for while reads overlap. The hooks see each
/// call once more, after it ran, and may add a note to its result or replace it.
///
/// A tool that runs other tools, such as `script`, runs them through a [`Nested`] caller, so that each of its calls goes through the same steps and is reported under it.
async fn run_calls(
    registry: &Registry,
    approvals: &Approvals,
    hooks: Option<&dyn Hooks>,
    history: &[Message],
    calls: &[ToolCall],
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> (Vec<ToolResult>, Option<String>) {
    let gate = tokio::sync::RwLock::new(());
    let halt = std::sync::Mutex::new(None);
    let batch = Batch {
        registry,
        approvals,
        hooks,
        events,
        gate: &gate,
        halt: &halt,
    };
    let mut slots: Vec<Option<ToolResult>> = vec![None; calls.len()];
    let mut admitted = Vec::new();
    for (index, call) in calls.iter().enumerate() {
        if call.name == RECALL && recall::offered(history) {
            events(AgentEvent::ToolStart {
                id: call.id.clone(),
                name: call.name.clone(),
                summary: sanitize(&recall::describe(&call.arguments)),
            });
            slots[index] = Some(match recall::run(history, call.arguments.clone()) {
                Ok((text, images)) => {
                    let mut result = finish(call, text, false, Vec::new(), events);
                    result.images = images;
                    result
                }
                Err(message) => finish(call, message, true, Vec::new(), events),
            });
            continue;
        }
        let mut ready = match admit(&batch, call, events).await {
            Admitted::Ended(result) => {
                slots[index] = Some(result);
                continue;
            }
            Admitted::Ready(ready) => ready,
        };
        match batch.approve(call, &mut ready, None).await {
            Ok(()) => admitted.push((index, ready)),
            Err(message) => {
                slots[index] = Some(finish(call, message, true, Vec::new(), events));
            }
        }
    }
    for wave in waves(admitted) {
        let indices: Vec<usize> = wave.iter().map(|(index, _)| *index).collect();
        let done = join_all(
            wave.into_iter()
                .map(|(index, ready)| batch.run_call(&calls[index], ready)),
        )
        .await;
        for (index, result) in indices.into_iter().zip(done) {
            slots[index] = Some(result);
        }
    }
    let halt = halt.into_inner().unwrap_or_else(|held| held.into_inner());
    (slots.into_iter().flatten().collect(), halt)
}

/// Splits `admitted`, the calls that may run with their places in the reply, in the model's order, into the groups that run one after another: each run of shared calls next to each other is one group, run at once, and each exclusive call is a group of its own.
fn waves(admitted: Vec<(usize, Ready<'_>)>) -> Vec<Vec<(usize, Ready<'_>)>> {
    let mut waves: Vec<Vec<(usize, Ready<'_>)>> = Vec::new();
    for (index, ready) in admitted {
        let shared = |ready: &Ready<'_>| ready.concurrency == Concurrency::Shared;
        match waves.last_mut() {
            Some(last) if shared(&ready) && last.iter().all(|(_, earlier)| shared(earlier)) => {
                last.push((index, ready));
            }
            _ => waves.push(vec![(index, ready)]),
        }
    }
    waves
}

/// What the hooks said about one call before it ran.
#[derive(Default)]
struct Screening {
    /// Why the call is not run, when it is not.
    denied: Option<String>,
    /// Whether it runs without the reader being asked.
    allowed: bool,
    /// Whether the reader is asked about it whatever it needs.
    asked: bool,
    /// A note for the model, added to the result.
    note: Option<String>,
}

/// Where one call stands once it was read, shown to the hooks and announced.
enum Admitted<'r> {
    /// It ended without running, with this result.
    Ended(ToolResult),
    /// It may run, once it is approved when it needs to be.
    Ready(Ready<'r>),
}

/// A call that may run, once it is approved when it needs to be.
struct Ready<'r> {
    /// The tool it calls.
    tool: &'r Arc<dyn Tool>,
    /// Its arguments, put right against the tool's schema and as the hooks left them.
    arguments: Value,
    /// A note from the hooks for the model, added to the result.
    note: Option<String>,
    /// What the reader is asked to approve, and what permission rules look at.
    request: ApprovalRequest,
    /// Whether the call needs the reader's approval before it runs.
    needed: bool,
    /// Whether a hook allowed it without asking the reader.
    allowed: bool,
    /// Whether a hook wants the reader asked about it.
    asked: bool,
    /// Whether it may overlap other calls of the reply.
    concurrency: Concurrency,
}

impl Ready<'_> {
    /// Takes `arguments`, already put right against the tool's schema, in place of the call's, with what is approved, what rules look at and whether the call may overlap others made from them.
    fn rewrite(&mut self, arguments: Value) {
        let scope = self.tool.approval_scope(&arguments);
        self.needed = scope.is_some();
        self.request.scope = scope.unwrap_or_else(|| self.request.tool.clone());
        self.request.subject = self.tool.subject(&arguments);
        self.request.summary = sanitize(&self.tool.describe(&arguments));
        self.concurrency = self.tool.concurrency(&arguments);
        self.arguments = arguments;
    }
}

/// Reads the arguments of `call`, a call of a tool in the registry of `batch`, puts them right against the tool's schema and shows them to the hooks of `batch`, which may deny the call, allow it without asking, have the reader asked or rewrite its arguments; then announces the call through `events` and says whether it may run.
///
/// A call of an unknown tool, with arguments that cannot be read or put right, or that a hook denied, ends here with its result reported.
async fn admit<'r>(
    batch: &Batch<'r>,
    call: &ToolCall,
    events: &(dyn Fn(AgentEvent) + Send + Sync),
) -> Admitted<'r> {
    let registry = batch.registry;
    let mut prepared = registry
        .get(&call.name)
        .map(|tool| (tool, prepare(registry, call)));
    let mut screened = Screening::default();
    if let (Some(hooks), Some((_, Ok(arguments)))) = (batch.hooks, &mut prepared) {
        let verdict = hooks.before_tool(call, arguments).await;
        if let Some(rewritten) = verdict.arguments {
            match registry.prepared(&call.name, rewritten) {
                Ok((_, rewritten)) => *arguments = rewritten,
                Err(error) => {
                    screened.denied =
                        Some(format!("a hook rewrote the arguments wrongly: {error}"));
                }
            }
        }
        match verdict.gate {
            Gate::Pass => {}
            Gate::Allow => screened.allowed = true,
            Gate::Ask => screened.asked = true,
            Gate::Deny(reason) => screened.denied = Some(reason),
        }
        screened.note = verdict.note;
        if let Some(reason) = verdict.halt {
            batch.stop(reason);
        }
    }
    let summary = sanitize(&match &prepared {
        Some((tool, Ok(arguments))) => tool.describe(arguments),
        Some((tool, Err(_))) => tool.describe(&call.arguments),
        None => call.arguments.to_string(),
    });
    events(AgentEvent::ToolStart {
        id: call.id.clone(),
        name: call.name.clone(),
        summary: summary.clone(),
    });
    if let Some(reason) = screened.denied.take() {
        let message = format!("a hook blocked this tool call: {}", reason.trim());
        return Admitted::Ended(finish(call, message, true, Vec::new(), events));
    }
    match prepared {
        None => {
            let message = format!("unknown tool: {}", call.name);
            Admitted::Ended(finish(call, message, true, Vec::new(), events))
        }
        Some((_, Err(message))) => Admitted::Ended(finish(call, message, true, Vec::new(), events)),
        Some((tool, Ok(arguments))) => Admitted::Ready(Ready {
            needed: tool.approval_scope(&arguments).is_some(),
            concurrency: tool.concurrency(&arguments),
            request: ApprovalRequest {
                tool: call.name.clone(),
                scope: tool
                    .approval_scope(&arguments)
                    .unwrap_or_else(|| call.name.clone()),
                summary,
                subject: tool.subject(&arguments),
                ..ApprovalRequest::default()
            },
            tool,
            arguments,
            note: screened.note,
            allowed: screened.allowed,
            asked: screened.asked && !screened.allowed,
        }),
    }
}

/// The arguments of `call`, a call of a tool in `registry`, read as a JSON object and put right against the tool's schema, as they are described, approved and run.
///
/// # Errors
///
/// Returns the result text, for the model, when the arguments are not a JSON object or a string holding one, or break the tool's schema in a way that cannot be put right.
fn prepare(registry: &Registry, call: &ToolCall) -> Result<Value, String> {
    let arguments = recovery::readable(call)?;
    registry
        .prepared(&call.name, arguments)
        .map(|(_, arguments)| arguments)
        .map_err(|error| error.to_string())
}

/// What one batch of calls runs with, shared with the [`Nested`] callers of the tools in it that run other tools.
#[derive(Clone, Copy)]
struct Batch<'a> {
    /// The tools.
    registry: &'a Registry,
    /// What is allowed without asking.
    approvals: &'a Approvals,
    /// The checks around each call.
    hooks: Option<&'a dyn Hooks>,
    /// Where the calls are reported.
    events: &'a (dyn Fn(AgentEvent) + Send + Sync),
    /// Taken by each call made by another tool while it is asked about and run: alone when it needs approval, a hook wants the reader asked or it is exclusive, so that such calls run one at a time, and shared by the shared calls that need no approval, which overlap; it is handed out in the order the calls were made.
    gate: &'a tokio::sync::RwLock<()>,
    /// Why the turn stops once the batch has ended, when a hook asked for that; the first reason given is kept.
    halt: &'a std::sync::Mutex<Option<String>>,
}

impl Batch<'_> {
    /// Runs `call`, admitted as `ready`, and reports its outcome, with the note from the hooks that screened it and what the hooks say after it added to its text; the tools it runs, when it runs any, are run through a [`Nested`] caller under it.
    ///
    /// A failing tool yields a result with `is_error` set.
    async fn run_call(&self, call: &ToolCall, ready: Ready<'_>) -> ToolResult {
        let caller = Nested::new(*self, call);
        let done = perform(call, ready, self.hooks, &caller).await;
        if let Some(reason) = done.halt {
            self.stop(reason);
        }
        let mut result = finish(call, done.text, done.is_error, done.changes, self.events);
        result.images = done.images.into_iter().map(image).collect();
        result
    }

    /// Keeps `reason` as why the turn stops once the batch has ended, unless an earlier reason was kept.
    fn stop(&self, reason: String) {
        let mut halt = self.halt.lock().unwrap_or_else(|held| held.into_inner());
        if halt.is_none() {
            *halt = Some(reason);
        }
    }

    /// Settles whether `call`, admitted as `ready`, may run: the permission rules decide when they can; otherwise the hooks may decide in the reader's place, allowing it, with arguments of their own, or refusing it, and when they do not, the reader is asked through `asking`, or through the approvals of the batch when there is none.
    ///
    /// Arguments a hook gives are put right against the tool's schema and put to the deny rules again.
    ///
    /// # Errors
    ///
    /// Returns the reason for the model when a rule, a hook or the reader refuses the call.
    async fn approve(
        &self,
        call: &ToolCall,
        ready: &mut Ready<'_>,
        asking: Option<&Nested<'_>>,
    ) -> Result<(), String> {
        let open =
            match self
                .approvals
                .standing(&ready.request, ready.needed, ready.allowed, ready.asked)
            {
                Standing::Settled(settled) => return settled,
                Standing::Open(open) => open,
            };
        if let Some(hooks) = self.hooks {
            match hooks.permission_request(call, &ready.arguments).await {
                Permit::Pass => {}
                Permit::Allow { arguments, rules } => {
                    if let Some(rewritten) = arguments {
                        let (_, rewritten) = self
                            .registry
                            .prepared(&call.name, rewritten)
                            .map_err(|error| {
                                format!("a hook rewrote the arguments wrongly: {error}")
                            })?;
                        ready.rewrite(rewritten);
                        if let Standing::Settled(Err(denied)) =
                            self.approvals
                                .standing(&ready.request, ready.needed, true, false)
                        {
                            return Err(denied);
                        }
                    }
                    self.approvals.allow_for_session(&rules);
                    return Ok(());
                }
                Permit::Deny { reason, halt } => {
                    let reason = format!("a hook denied this tool call: {}", reason.trim());
                    if halt {
                        self.stop(reason.clone());
                    }
                    return Err(reason);
                }
            }
        }
        match asking {
            Some(nested) => nested.ask(open).await,
            None => self.approvals.ask(open).await,
        }
    }
}

/// How one call that ran ended.
struct Performed {
    /// The result text, with the hooks' notes, or the error.
    text: String,
    /// Whether it failed.
    is_error: bool,
    /// The files it changed.
    changes: Vec<FileChange>,
    /// The images it returned.
    images: Vec<omc_tools::Image>,
    /// What its text says as a value, when the tool gives one and no hook added a note to the text.
    data: Option<Value>,
    /// Why the turn stops once the batch has ended, when a hook said so after the call.
    halt: Option<String>,
}

/// Runs `call`, admitted as `ready`, with `caller` for the tools it runs, and adds the note from the hooks that screened it and what `hooks` say after the call, whether it succeeded or failed, to its text, or replaces the text with what they give in its place.
async fn perform(
    call: &ToolCall,
    ready: Ready<'_>,
    hooks: Option<&dyn Hooks>,
    caller: &dyn Caller,
) -> Performed {
    let Ready {
        tool,
        arguments,
        note,
        ..
    } = ready;
    let shown = hooks.map(|_| arguments.clone());
    let began = Instant::now();
    let mut done = match tool.call_through(&call.id, arguments, caller).await {
        Ok(output) => Performed {
            text: output.text,
            is_error: false,
            changes: output.changes,
            images: output.images,
            data: output.data,
            halt: None,
        },
        Err(error) => Performed {
            text: error.to_string(),
            is_error: true,
            changes: Vec::new(),
            images: Vec::new(),
            data: None,
            halt: None,
        },
    };
    if let (Some(hooks), Some(arguments)) = (hooks, &shown) {
        let followed = if done.is_error {
            hooks
                .after_tool_failed(call, arguments, &done.text, began.elapsed())
                .await
        } else {
            hooks.after_tool(call, arguments, &done.text).await
        };
        if let Some(output) = followed.output {
            done.text = output;
            done.data = None;
        }
        if let Some(note) = note {
            done.text = with_note(done.text, &note);
            done.data = None;
        }
        if let Some(note) = followed.note {
            done.text = with_note(done.text, &note);
            done.data = None;
        }
        done.halt = followed.halt;
    } else if let Some(note) = note {
        done.text = with_note(done.text, &note);
        done.data = None;
    }
    done
}

/// What the calls a tool makes of other tools wait for and report under.
struct Nested<'a> {
    /// What the batch of the calling tool runs with.
    batch: Batch<'a>,
    /// The id of the call that runs the tools.
    parent: &'a str,
    /// The name of the tool that runs them, which the reports are filed under.
    agent: &'a str,
    /// How many calls it made.
    made: AtomicUsize,
    /// How long the calls waited for the reader, and since when the one being asked about now has.
    waiting: std::sync::Mutex<(Duration, Option<Instant>)>,
}

impl<'a> Nested<'a> {
    /// The caller for the tools `call` runs, as the batch `batch` runs it.
    fn new(batch: Batch<'a>, call: &'a ToolCall) -> Self {
        Self {
            batch,
            parent: &call.id,
            agent: &call.name,
            made: AtomicUsize::new(0),
            waiting: std::sync::Mutex::new((Duration::ZERO, None)),
        }
    }

    /// `event` filed under the calling tool's call.
    fn file(&self, event: AgentEvent) -> AgentEvent {
        AgentEvent::Nested {
            parent: self.parent.to_owned(),
            agent: self.agent.to_owned(),
            event: Box::new(event),
        }
    }

    /// Asks the reader about `request`, a call the permission rules left open, timing how long the answer takes.
    ///
    /// # Errors
    ///
    /// Returns the reason for the model when the reader refuses the call.
    async fn ask(&self, request: ApprovalRequest) -> Result<(), String> {
        self.lock_waiting().1 = Some(Instant::now());
        let allowed = self.batch.approvals.ask(request).await;
        let mut waiting = self.lock_waiting();
        if let Some(since) = waiting.1.take() {
            waiting.0 += since.elapsed();
        }
        allowed
    }

    /// The time waited for the reader, held.
    fn lock_waiting(&self) -> std::sync::MutexGuard<'_, (Duration, Option<Instant>)> {
        self.waiting.lock().unwrap_or_else(|held| held.into_inner())
    }
}

#[async_trait]
impl Caller for Nested<'_> {
    /// Whether the agent has a tool called `name`.
    fn has(&self, name: &str) -> bool {
        self.batch.registry.get(name).is_some()
    }

    /// Runs the tool `name` with `args` the way a call of the model runs: its arguments put right against the schema and shown to the hooks, the reader asked when it needs approval, the hooks shown its result; reported under the calling tool's call, numbered from one in the order they were made.
    ///
    /// Calls that need approval or are exclusive are asked about and run one at a time, while shared calls that need no approval overlap; the reader is told which tool made them.
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, String> {
        let number = self.made.fetch_add(1, AtomicOrdering::Relaxed) + 1;
        let call = ToolCall {
            id: number.to_string(),
            name: name.to_owned(),
            arguments: args,
        };
        let filed = |event: AgentEvent| (self.batch.events)(self.file(event));
        let mut ready = match admit(&self.batch, &call, &filed).await {
            Admitted::Ended(result) => return Err(result.text),
            Admitted::Ready(ready) => ready,
        };
        let alone = ready.needed || ready.asked || ready.concurrency == Concurrency::Exclusive;
        let _turn: (
            Option<RwLockWriteGuard<'_, ()>>,
            Option<RwLockReadGuard<'_, ()>>,
        ) = match alone {
            true => (Some(self.batch.gate.write().await), None),
            false => (None, Some(self.batch.gate.read().await)),
        };
        ready.request.summary = format!("{} (called by {})", ready.request.summary, self.agent);
        if let Err(message) = self.batch.approve(&call, &mut ready, Some(self)).await {
            finish(&call, message.clone(), true, Vec::new(), &filed);
            return Err(message);
        }
        let done = perform(&call, ready, self.batch.hooks, &NoCaller).await;
        finish(
            &call,
            done.text.clone(),
            done.is_error,
            done.changes,
            &filed,
        );
        if done.is_error {
            return Err(done.text);
        }
        let output = ToolOutput::new(done.text);
        Ok(match done.data {
            Some(data) => output.with_data(data),
            None => output,
        })
    }

    /// How long the calls made so far waited for the reader's answer, the open question included.
    fn waited(&self) -> Duration {
        let waiting = self.lock_waiting();
        waiting.0 + waiting.1.map_or(Duration::ZERO, |since| since.elapsed())
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
                hooks: None,
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
                hooks: None,
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
                    retry_after: None,
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
                hooks: None,
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
                hooks: None,
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
                hooks: None,
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
                hooks: None,
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
                hooks: None,
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
                hooks: None,
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
                hooks: None,
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
