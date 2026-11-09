//! Compaction: summarizing the older part of a long history so the conversation keeps fitting the model's context window.
//!
//! A compaction never deletes messages. It appends a [`Compaction`] to the history that holds a summary of the messages before a cut point; from then on the model is given that summary, the messages from the cut point on, and what follows. The whole history stays in the journal, so the conversation can still be shown and read in full.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use omc_providers::{
    ChatModel, Compaction, Effort, Elision, Message, Part, Reply, Request, Sink, ToolResult,
};

use crate::AgentEvent;
use crate::recall::RECALL;
use crate::{plans, recovery, todos};

/// The fewest tokens kept free below the context window for the next request and its reply, on a window large enough to spare them.
const RESERVE_TOKENS: u64 = 16_384;

/// The share of the context window, in percent, kept free below it when that is more than [`RESERVE_TOKENS`], or on a window too small to spare those.
const RESERVE_PERCENT: u64 = 15;

/// About how many tokens of the most recent messages a compaction keeps as they are.
const KEEP_RECENT_TOKENS: u64 = 20_000;

/// The characters an image is counted as when tokens are estimated.
const IMAGE_CHARS: usize = 4_800;

/// The most characters of a tool result, or of a tool call's arguments, the summarizer is shown.
const CLIP_CHARS: usize = 2_000;

/// The name compactions are shown under, as if they were a tool call.
pub const COMPACT: &str = "compact";

/// The system prompt of a summary request.
const SYSTEM: &str = include_str!("prompts/compaction_system.md");

/// What the summarizer is asked for the first time a history is compacted.
const SUMMARY: &str = include_str!("prompts/compaction_summary.md");

/// What the summarizer is asked when an earlier summary is brought up to date.
const UPDATE: &str = include_str!("prompts/compaction_update.md");

/// What the summarizer is asked for the beginning of a turn the cut point splits.
const TURN: &str = include_str!("prompts/compaction_turn.md");

/// The user message that gives the model a summary, with `{summary}` standing for it.
const COMPACTED: &str = include_str!("prompts/compacted.md");

/// What a compaction is to summarize, worked out from the history before a model is asked.
#[derive(Debug, PartialEq)]
pub(crate) struct Plan {
    /// The positions of the messages summarized with the earlier summary, oldest first.
    earlier: Vec<usize>,
    /// The positions of the beginning of the turn the cut point splits, summarized on their own; empty when the cut point starts a turn.
    prefix: Vec<usize>,
    /// The position of the first message kept as it is.
    first_kept: usize,
    /// About how many tokens the context held.
    tokens_before: u64,
}

/// The messages the model is given for `history`: the history itself when it was never compacted or pruned; otherwise the latest summary as a user message, when there is one, followed by the messages kept with it and the messages after it, with every elided tool output replaced by its note.
///
/// A reply that follows a finished answer directly, as the reply to a notice sent after that answer does, is given joined to it as one reply, since the notice is not kept and every provider wants a reader's message between two replies.
pub fn context(history: &[Message]) -> Cow<'_, [Message]> {
    let hidden = hidden(history);
    let compacted = latest(history);
    if compacted.is_none() && hidden.is_empty() {
        return joined_replies(Cow::Borrowed(history));
    }
    let mut messages: Vec<Message> = compacted
        .map(|(_, compaction)| {
            let mut text = summary_message(compaction);
            if let Some(plan) = plans::restated(history, compaction.first_kept) {
                text.push_str("\n\n");
                text.push_str(&plan);
            }
            if let Some(todos) = todos::restated(history, compaction.first_kept) {
                text.push_str("\n\n");
                text.push_str(&todos);
            }
            Message::User(text)
        })
        .into_iter()
        .collect();
    messages.extend(
        live(history)
            .into_iter()
            .map(|at| shown(history, at, &hidden).into_owned()),
    );
    joined_replies(Cow::Owned(messages))
}

/// Whether `later` is a reply that directly follows `earlier`, a reply that made no tool calls.
fn follows_answer(earlier: &Message, later: &Message) -> bool {
    matches!(
        (earlier, later),
        (Message::Assistant(answer), Message::Assistant(_)) if answer.tool_calls.is_empty()
    )
}

/// `messages` with each reply that directly follows a reply without tool calls joined to it, the text of the two kept a paragraph apart; `messages` as they are when there is none.
fn joined_replies(messages: Cow<'_, [Message]>) -> Cow<'_, [Message]> {
    if !messages
        .windows(2)
        .any(|pair| follows_answer(&pair[0], &pair[1]))
    {
        return messages;
    }
    let mut joined: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages.iter() {
        match message {
            Message::Assistant(reply) => push_reply(&mut joined, reply.clone()),
            other => joined.push(other.clone()),
        }
    }
    Cow::Owned(joined)
}

/// Adds `reply` to the end of `messages`, the messages of a request: joined to the last of them, a paragraph after its text, when that is a reply without tool calls, and as a message of its own otherwise.
pub(crate) fn push_reply(messages: &mut Vec<Message>, reply: Reply) {
    match messages.last_mut() {
        Some(Message::Assistant(earlier)) if earlier.tool_calls.is_empty() => {
            *earlier = recovery::joined(recovery::carried_on(std::mem::take(earlier)), reply);
        }
        _ => messages.push(Message::Assistant(reply)),
    }
}

/// The latest compaction in `history` and its position, when there is one.
fn latest(history: &[Message]) -> Option<(usize, &Compaction)> {
    history
        .iter()
        .enumerate()
        .rev()
        .find_map(|(at, message)| match message {
            Message::Compaction(compaction) => Some((at, compaction)),
            _ => None,
        })
}

/// Whether `message` is a record of how the history is given to the model, a compaction or a pruning, rather than part of the conversation.
fn is_record(message: &Message) -> bool {
    matches!(message, Message::Compaction(_) | Message::Pruning(_))
}

/// The positions of the messages the model is given, oldest first: every message when `history` was never compacted, otherwise those from the latest compaction's first kept message on, with the compactions and prunings themselves left out.
pub(crate) fn live(history: &[Message]) -> Vec<usize> {
    let start = latest(history).map_or(0, |(at, compaction)| compaction.first_kept.min(at));
    (start..history.len())
        .filter(|&at| !is_record(&history[at]))
        .collect()
}

/// What the prunings of a history keep from the model: tool outputs left out and fenced blocks cut out of prompts and replies.
#[derive(Debug, Default)]
pub(crate) struct Hidden<'h> {
    /// Every tool output left out, keyed by the position of its tool results and the id of its call.
    pub(crate) elided: HashMap<(usize, &'h str), Elision>,
    /// The byte ranges cut out of the text of each prompt or reply, keyed by its position, in no particular order.
    pub(crate) excised: HashMap<usize, Vec<(usize, usize)>>,
}

impl Hidden<'_> {
    /// Whether nothing is kept from the model.
    pub(crate) fn is_empty(&self) -> bool {
        self.elided.is_empty() && self.excised.is_empty()
    }
}

/// What the prunings in `history` keep from the model.
pub(crate) fn hidden(history: &[Message]) -> Hidden<'_> {
    let mut hidden = Hidden::default();
    for message in history {
        let Message::Pruning(pruning) = message else {
            continue;
        };
        for elided in &pruning.elided {
            hidden
                .elided
                .insert((elided.at, elided.call_id.as_str()), elided.reason);
        }
        for block in &pruning.excised {
            hidden
                .excised
                .entry(block.at)
                .or_default()
                .push((block.start, block.end));
        }
    }
    hidden
}

/// Every tool output the prunings in `history` left out, keyed by the position of its tool results and the id of its call.
pub(crate) fn elisions(history: &[Message]) -> HashMap<(usize, &str), Elision> {
    hidden(history).elided
}

/// The message at `at` in `history` as the model is given it: the message itself, or a copy with each tool output `hidden` lists replaced by its note and its images dropped, and each block `hidden` cuts out of its text replaced by its note.
pub(crate) fn shown<'h>(history: &'h [Message], at: usize, hidden: &Hidden) -> Cow<'h, Message> {
    let message = &history[at];
    match message {
        Message::ToolResults(results)
            if results
                .iter()
                .any(|result| hidden.elided.contains_key(&(at, result.call_id.as_str()))) =>
        {
            let results = results
                .iter()
                .map(
                    |result| match hidden.elided.get(&(at, result.call_id.as_str())) {
                        Some(&reason) => ToolResult {
                            text: elision_note(result, reason),
                            images: Vec::new(),
                            ..result.clone()
                        },
                        None => result.clone(),
                    },
                )
                .collect();
            Cow::Owned(Message::ToolResults(results))
        }
        Message::User(text) if let Some(ranges) = hidden.excised.get(&at) => {
            Cow::Owned(Message::User(excise(text, at, ranges)))
        }
        Message::Assistant(reply) if let Some(ranges) = hidden.excised.get(&at) => {
            Cow::Owned(Message::Assistant(Reply {
                text: excise(&reply.text, at, ranges),
                ..reply.clone()
            }))
        }
        _ => Cow::Borrowed(message),
    }
}

/// `text`, the text of the message at `at`, with each of `ranges` that is a whole fenced block of it replaced by its note; a range that no longer fits the text is left alone.
fn excise(text: &str, at: usize, ranges: &[(usize, usize)]) -> String {
    let mut ranges: Vec<(usize, usize)> = ranges
        .iter()
        .copied()
        .filter(|&(start, end)| {
            start < end
                && end <= text.len()
                && text.is_char_boundary(start)
                && text.is_char_boundary(end)
        })
        .collect();
    ranges.sort_unstable();
    ranges.dedup();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (start, end) in ranges {
        if start < cursor {
            continue;
        }
        out.push_str(&text[cursor..start]);
        out.push_str(&excision_note(&text[start..end], at));
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// The note that stands in for `block`, a fenced block cut out of the message at `at`, which tells the model how to read it again.
pub(crate) fn excision_note(block: &str, at: usize) -> String {
    let lines = block.lines().count().saturating_sub(2);
    let language = block
        .lines()
        .next()
        .map(|fence| fence.trim().trim_start_matches(['`', '~']).trim())
        .and_then(|info| info.split_whitespace().next())
        .filter(|language| !language.is_empty());
    let kind = match language {
        Some(language) => format!("{language} block"),
        None => "fenced block".to_owned(),
    };
    format!(
        "[{lines} lines of {kind} removed to save room. Call {RECALL} with messages \"{at}\" to read them again.]"
    )
}

/// The note that stands in for `result` when it is left out for `reason`, which tells the model how to read it again.
pub(crate) fn elision_note(result: &ToolResult, reason: Elision) -> String {
    let id = &result.call_id;
    match reason {
        Elision::Old => format!(
            "[Output elided to save room in the context: about {} tokens. Call {RECALL} with call_id \"{id}\" to read it again.]",
            result_tokens(result)
        ),
        Elision::Superseded => format!(
            "[Output elided: the same read was made again later in the conversation, so this copy is out of date. Call {RECALL} with call_id \"{id}\" to read it anyway.]"
        ),
        Elision::Shaken => format!(
            "[{} lines of {} output removed to save room. Call {RECALL} with call_id \"{id}\" to read it again.]",
            result.text.lines().count(),
            result.name
        ),
    }
}

/// The text of the user message that gives the model `compaction`'s summary, with the files read and changed before it listed after it.
fn summary_message(compaction: &Compaction) -> String {
    let mut text = COMPACTED
        .trim_end()
        .replace("{summary}", compaction.summary.trim());
    for (tag, files) in [
        ("read-files", &compaction.read_files),
        ("modified-files", &compaction.modified_files),
    ] {
        if !files.is_empty() {
            text.push_str(&format!("\n\n<{tag}>\n{}\n</{tag}>", files.join("\n")));
        }
    }
    text
}

/// About how many tokens `result` takes, counted as [`estimate`] counts them.
pub(crate) fn result_tokens(result: &ToolResult) -> u64 {
    ((result.text.len() + result.images.len() * IMAGE_CHARS) as u64).div_ceil(4)
}

/// About how many tokens `message` takes, counting four bytes of text to a token and an image as [`IMAGE_CHARS`] bytes; meant to err on the high side. A pruning takes none, since it is never sent.
pub(crate) fn estimate(message: &Message) -> u64 {
    let bytes = match message {
        Message::User(text) => text.len(),
        Message::Assistant(reply) => {
            reply.text.len()
                + reply.thinking.len()
                + reply
                    .tool_calls
                    .iter()
                    .map(|call| call.name.len() + call.arguments.to_string().len())
                    .sum::<usize>()
        }
        Message::ToolResults(results) => {
            return results.iter().map(result_tokens).sum();
        }
        Message::Compaction(compaction) => summary_message(compaction).len(),
        Message::Pruning(_) => 0,
    };
    (bytes as u64).div_ceil(4)
}

/// About how many tokens the context of `history` holds with the system prompt `system`.
///
/// When a reply since the latest compaction or pruning says what its request cost, that count and the reply's output are taken, with an estimate of the messages after it added; otherwise the whole context is estimated with [`estimated`].
pub(crate) fn context_tokens(history: &[Message], system: &str) -> u64 {
    let since = history.iter().rposition(is_record).map_or(0, |at| at + 1);
    let counted = history
        .iter()
        .enumerate()
        .skip(since)
        .rev()
        .find_map(|(at, message)| match message {
            Message::Assistant(reply) => reply
                .tokens
                .map(|tokens| (at, tokens.context() + tokens.output)),
            _ => None,
        });
    match counted {
        Some((at, tokens)) => tokens + history[at + 1..].iter().map(estimate).sum::<u64>(),
        None => estimated(history, system),
    }
}

/// About how many tokens the context of `history` holds with the system prompt `system`, estimated whole: the system prompt and every message the model is given.
pub(crate) fn estimated(history: &[Message], system: &str) -> u64 {
    (system.len() as u64).div_ceil(4) + context(history).iter().map(estimate).sum::<u64>()
}

/// How many tokens the context of a model whose window holds `window` tokens may reach before it is compacted: the window less what is kept free for the next request and its reply.
pub(crate) fn threshold(window: u64) -> u64 {
    let share = window / 100 * RESERVE_PERCENT;
    let reserve = if window > 4 * RESERVE_TOKENS {
        share.max(RESERVE_TOKENS)
    } else {
        share
    };
    window.saturating_sub(reserve)
}

/// About how many tokens of the most recent messages a compaction keeps as they are, for a model whose window holds `window` tokens when that is known: [`KEEP_RECENT_TOKENS`], or a quarter of the threshold on a window too small for that.
pub(crate) fn keep_recent(window: Option<u64>) -> u64 {
    window.map_or(KEEP_RECENT_TOKENS, |window| {
        KEEP_RECENT_TOKENS.min(threshold(window) / 4)
    })
}

/// Works out what to summarize in `history` so that about `keep` tokens of the most recent messages stay as they are; nothing when nothing older than that is left to summarize. `tokens_before` is how full the context is now.
///
/// The cut point is a user message or a model reply, never tool results, so every kept result keeps its call. When it falls on a reply in the middle of a turn, the beginning of that turn is summarized on its own, so the rest of the turn still makes sense after the summary.
pub(crate) fn plan(history: &[Message], keep: u64, tokens_before: u64) -> Option<Plan> {
    let live = live(history);
    let hidden = hidden(history);
    let starts_here = |index: usize| matches!(history[live[index]], Message::User(_));
    let cuts: Vec<usize> = (0..live.len())
        .filter(|&index| {
            matches!(
                history[live[index]],
                Message::User(_) | Message::Assistant(_)
            )
        })
        .collect();
    let last = *cuts.last()?;
    let mut cut = cuts[0];
    let mut held = 0;
    for index in (0..live.len()).rev() {
        held += estimate(&shown(history, live[index], &hidden));
        if held >= keep {
            cut = cuts
                .iter()
                .copied()
                .find(|&candidate| candidate >= index)
                .unwrap_or(last);
            break;
        }
    }
    let turn = if starts_here(cut) {
        None
    } else {
        (0..cut).rev().find(|&index| starts_here(index))
    };
    let (earlier, prefix) = match turn {
        Some(start) => (live[..start].to_vec(), live[start..cut].to_vec()),
        None => (live[..cut].to_vec(), Vec::new()),
    };
    if earlier.is_empty() && prefix.is_empty() {
        return None;
    }
    Some(Plan {
        earlier,
        prefix,
        first_kept: live[cut],
        tokens_before,
    })
}

/// Summarizes what `plan` covers in `history` with `model`, building on the latest summary when there is one, and returns the compaction to record, whose `tokens_after` is left for the caller to fill in with [`estimated`] once it is recorded; `focus` is what the reader asked the summary to dwell on.
///
/// # Errors
///
/// Returns why, for the reader, when the model request fails or the model answers with something other than a summary.
pub(crate) async fn summarize(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    history: &[Message],
    plan: &Plan,
    focus: Option<&str>,
) -> Result<Compaction, String> {
    let previous = latest(history).map(|(_, compaction)| compaction);
    let focus = focus
        .map(str::trim)
        .filter(|focus| !focus.is_empty())
        .map(|focus| format!("\n\nGive particular attention to: {focus}"))
        .unwrap_or_default();
    let mut summary = if plan.earlier.is_empty() {
        previous.map_or_else(
            || "No earlier history.".to_owned(),
            |previous| previous.summary.clone(),
        )
    } else {
        let mut prompt = format!(
            "<conversation>\n{}\n</conversation>\n\n",
            transcript(history, &plan.earlier)
        );
        match previous {
            Some(previous) => prompt.push_str(&format!(
                "<previous-summary>\n{}\n</previous-summary>\n\n{UPDATE}",
                previous.summary
            )),
            None => prompt.push_str(SUMMARY),
        }
        prompt.push_str(&focus);
        ask(model, effort, prompt).await?
    };
    if !plan.prefix.is_empty() {
        let prompt = format!(
            "<conversation>\n{}\n</conversation>\n\n{TURN}{focus}",
            transcript(history, &plan.prefix)
        );
        let turn = ask(model, effort, prompt).await?;
        summary.push_str(&format!("\n\n---\n\n## The turn under way\n\n{turn}"));
    }
    let mut read: BTreeSet<String> =
        previous.map_or_else(BTreeSet::new, |c| c.read_files.iter().cloned().collect());
    let mut modified: BTreeSet<String> = previous.map_or_else(BTreeSet::new, |c| {
        c.modified_files.iter().cloned().collect()
    });
    for positions in [&plan.earlier, &plan.prefix] {
        file_activity(history, positions, &mut read, &mut modified);
    }
    Ok(Compaction {
        summary,
        first_kept: plan.first_kept,
        tokens_before: plan.tokens_before,
        tokens_after: 0,
        read_files: read.difference(&modified).cloned().collect(),
        modified_files: modified.into_iter().collect(),
    })
}

/// Asks `model` for the summary `prompt` asks for, offering it no tools and writing nothing to the provider's prompt cache, since the request is never sent again.
///
/// # Errors
///
/// Returns why when the request fails, the model calls a tool, or it answers with nothing.
async fn ask(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    prompt: String,
) -> Result<String, String> {
    let messages = [Message::User(prompt)];
    let request = Request {
        system: SYSTEM,
        messages: &messages,
        tools: &[],
        effort,
    };
    let reply = model
        .complete_uncached(request, &mut Quiet)
        .await
        .map_err(|error| error.to_string())?;
    if !reply.tool_calls.is_empty() {
        return Err("the model called a tool instead of writing the summary".to_owned());
    }
    let text = reply.text.trim();
    if text.is_empty() {
        return Err("the model returned an empty summary".to_owned());
    }
    Ok(text.to_owned())
}

/// A sink that drops the chunks of a summary as they stream in, since the summary is shown once it is whole.
struct Quiet;

impl Sink for Quiet {
    /// Ignores the start of a block.
    fn start(&mut self, _part: Part) {}

    /// Ignores a chunk of text.
    fn text(&mut self, _chunk: &str) {}

    /// Ignores a chunk of thinking.
    fn thinking(&mut self, _chunk: &str) {}
}

/// The messages at `positions` in `history` as plain text for the summarizer, so that it reads them as a record rather than as a conversation to carry on.
///
/// Thinking is left out, since what providers return of it is a digest that can be longer than what it stands for; tool results and call arguments are cut to [`CLIP_CHARS`].
fn transcript(history: &[Message], positions: &[usize]) -> String {
    let mut parts = Vec::new();
    for message in positions.iter().map(|&at| &history[at]) {
        match message {
            Message::User(text) => parts.push(format!("[User]: {text}")),
            Message::Assistant(reply) => {
                if !reply.text.trim().is_empty() {
                    parts.push(format!("[Assistant]: {}", reply.text));
                }
                if !reply.tool_calls.is_empty() {
                    let calls: Vec<String> = reply
                        .tool_calls
                        .iter()
                        .map(|call| {
                            format!(
                                "{}({})",
                                call.name,
                                clip(&call.arguments.to_string(), CLIP_CHARS)
                            )
                        })
                        .collect();
                    parts.push(format!("[Assistant tool calls]: {}", calls.join("; ")));
                }
            }
            Message::ToolResults(results) => {
                for result in results {
                    let label = if result.is_error {
                        "Tool error"
                    } else {
                        "Tool result"
                    };
                    parts.push(format!(
                        "[{label}, {}]: {}",
                        result.name,
                        clip(&result.text, CLIP_CHARS)
                    ));
                }
            }
            Message::Compaction(_) | Message::Pruning(_) => {}
        }
    }
    parts.join("\n\n")
}

/// `text` cut to at most `limit` bytes at a character boundary, with a note of how much was cut; `text` itself when it is short enough.
fn clip(text: &str, limit: usize) -> Cow<'_, str> {
    if text.len() <= limit {
        return Cow::Borrowed(text);
    }
    let mut end = limit;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!(
        "{}\n[... {} more bytes cut]",
        &text[..end],
        text.len() - end
    ))
}

/// Adds the files that the tool calls among the messages at `positions` in `history` read to `read`, and those they wrote or edited to `modified`.
fn file_activity(
    history: &[Message],
    positions: &[usize],
    read: &mut BTreeSet<String>,
    modified: &mut BTreeSet<String>,
) {
    for message in positions.iter().map(|&at| &history[at]) {
        let Message::Assistant(reply) = message else {
            continue;
        };
        for call in &reply.tool_calls {
            let path = call.arguments["path"].as_str().map(str::to_owned);
            match call.name.as_str() {
                "read" => read.extend(path),
                "write" | "ast_edit" => modified.extend(path),
                "edit" => modified.extend(
                    call.arguments["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|file| file["path"].as_str())
                        .map(str::to_owned),
                ),
                _ => {}
            }
        }
    }
}

/// The event that shows a compaction of a context of `tokens_before` tokens starting, as a call of [`COMPACT`] whose id comes from `at`, the position the compaction takes in the history.
pub(crate) fn started(at: usize, tokens_before: u64) -> AgentEvent {
    AgentEvent::ToolStart {
        id: call_id(at),
        name: COMPACT.to_owned(),
        summary: format!("summarize older messages, {tokens_before} tokens in context"),
    }
}

/// The event that shows the compaction at `at` ending with `text`, its summary or why it failed.
pub(crate) fn finished(at: usize, text: String, is_error: bool) -> AgentEvent {
    AgentEvent::ToolDone {
        id: call_id(at),
        name: COMPACT.to_owned(),
        text,
        is_error,
        changes: Vec::new(),
    }
}

/// The events that show `compaction`, recorded at position `at` of a saved history, as they were shown when it was made: its start, its summary, and how full the context was after it when that was recorded.
pub fn replayed(at: usize, compaction: &Compaction) -> Vec<AgentEvent> {
    let mut events = vec![
        started(at, compaction.tokens_before),
        finished(at, compaction.summary.clone(), false),
    ];
    if compaction.tokens_after > 0 {
        events.push(AgentEvent::Used(omc_providers::Tokens {
            input: compaction.tokens_after,
            ..omc_providers::Tokens::default()
        }));
    }
    events
}

/// The id a compaction at position `at` of the history is shown under.
fn call_id(at: usize) -> String {
    format!("{COMPACT}-{at}")
}

#[cfg(test)]
/// Tests for planning and summarizing a compaction.
mod tests {
    use omc_providers::{Reply, ToolCall, ToolResult};
    use serde_json::json;

    use super::*;

    /// A user message of about `tokens` tokens.
    fn user(tokens: usize) -> Message {
        Message::User("u".repeat(tokens * 4))
    }

    /// A reply of about `tokens` tokens that calls `read` on `path`.
    fn reading(path: &str, tokens: usize) -> Message {
        Message::Assistant(Reply {
            text: "r".repeat(tokens * 4),
            tool_calls: vec![ToolCall {
                id: path.to_owned(),
                name: "read".to_owned(),
                arguments: json!({ "path": path }),
            }],
            ..Reply::default()
        })
    }

    /// The results of the call `id`, of about `tokens` tokens.
    fn results(id: &str, tokens: usize) -> Message {
        Message::ToolResults(vec![ToolResult {
            call_id: id.to_owned(),
            name: "read".to_owned(),
            text: "t".repeat(tokens * 4),
            is_error: false,
            images: Vec::new(),
        }])
    }

    /// A reply of about `tokens` tokens that calls nothing.
    fn answer(tokens: usize) -> Message {
        Message::Assistant(Reply {
            text: "a".repeat(tokens * 4),
            ..Reply::default()
        })
    }

    /// The cut keeps about the budget of recent messages, starts at a user message when the budget lands there, and leaves everything older to summarize.
    #[test]
    fn plans_a_cut_at_a_turn() {
        let history = vec![user(100), answer(100), user(100), answer(100)];
        let plan = plan(&history, 150, 0).unwrap();
        assert_eq!(plan.first_kept, 2);
        assert_eq!(plan.earlier, vec![0, 1]);
        assert!(plan.prefix.is_empty());
    }

    /// A cut inside a turn falls on a reply, never on results, and the beginning of the turn is summarized on its own.
    #[test]
    fn splits_a_turn_at_a_reply() {
        let history = vec![
            user(100),
            answer(100),
            user(10),
            reading("a", 10),
            results("a", 100),
            reading("b", 10),
            results("b", 100),
        ];
        let plan = plan(&history, 150, 0).unwrap();
        assert_eq!(plan.first_kept, 5);
        assert_eq!(plan.earlier, vec![0, 1]);
        assert_eq!(plan.prefix, vec![2, 3, 4]);
    }

    /// A history that fits the budget has nothing to compact.
    #[test]
    fn nothing_to_plan_when_it_fits() {
        let history = vec![user(10), answer(10)];
        assert_eq!(plan(&history, 1_000, 0), None);
        assert_eq!(plan(&[], 1_000, 0), None);
    }

    /// After a compaction the model is given the summary, the kept messages and what came after, and the compaction's own position is skipped.
    #[test]
    fn the_context_starts_with_the_summary() {
        let mut history = vec![user(1), answer(1), user(1), answer(1)];
        history.push(Message::Compaction(Compaction {
            summary: "it went well".into(),
            first_kept: 2,
            read_files: vec!["a.rs".into()],
            ..Compaction::default()
        }));
        history.push(user(1));
        let context = context(&history);
        assert_eq!(context.len(), 4);
        let Message::User(first) = &context[0] else {
            panic!("no summary first");
        };
        assert!(first.contains("<summary>\nit went well\n</summary>"));
        assert!(first.contains("<read-files>\na.rs\n</read-files>"));
        assert_eq!(context[1], history[2]);
        assert_eq!(context[3], history[5]);
    }

    /// A compaction that hides the latest todo list restates its open items after the summary.
    #[test]
    fn the_summary_restates_open_todos() {
        let mut history = vec![
            user(1),
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "t".into(),
                    name: "todo".into(),
                    arguments: json!({"todos": [
                        {"content": "done", "status": "completed"},
                        {"content": "left", "status": "pending"}
                    ]}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "t".into(),
                name: "todo".into(),
                text: "ok".into(),
                is_error: false,
                images: Vec::new(),
            }]),
            answer(1),
            user(1),
            answer(1),
        ];
        history.push(Message::Compaction(Compaction {
            summary: "s".into(),
            first_kept: 4,
            ..Compaction::default()
        }));
        let given = context(&history);
        let Message::User(first) = &given[0] else {
            panic!("no summary first");
        };
        assert!(
            first.contains("<open-todos>") && first.contains("- [pending] left"),
            "{first}"
        );
        assert!(!first.contains("done"), "{first}");
        history.extend([
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "u".into(),
                    name: "todo".into(),
                    arguments: json!({"todos": [{"content": "left", "status": "completed"}]}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "u".into(),
                name: "todo".into(),
                text: "ok".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        ]);
        let given = context(&history);
        let Message::User(first) = &given[0] else {
            panic!("no summary first");
        };
        assert!(!first.contains("<open-todos>"), "{first}");
    }

    /// A reply that follows a finished answer directly is given joined to it, so no request holds two replies in a row, while the history keeps them apart.
    #[test]
    fn replies_in_a_row_are_given_as_one() {
        let history = vec![
            user(1),
            Message::Assistant(Reply {
                text: "First.".into(),
                ..Reply::default()
            }),
            reading("a", 1),
            results("a", 1),
        ];
        let given = context(&history);
        assert_eq!(given.len(), 3);
        let Message::Assistant(joined) = &given[1] else {
            panic!("expected the joined reply, got {:?}", given[1]);
        };
        assert_eq!(joined.text, format!("First.\n\n{}", "r".repeat(4)));
        assert_eq!(joined.tool_calls.len(), 1);
        assert!(matches!(context(&history[..2]), Cow::Borrowed(_)));
    }

    /// A second compaction only considers what the model is given now, and can keep messages from before the first one.
    #[test]
    fn a_second_plan_starts_after_the_first_summary() {
        let mut history = vec![user(100), answer(100), user(100), answer(100)];
        history.push(Message::Compaction(Compaction {
            summary: "s".into(),
            first_kept: 2,
            ..Compaction::default()
        }));
        history.extend([user(100), answer(100)]);
        let plan = plan(&history, 150, 0).unwrap();
        assert_eq!(plan.earlier, vec![2, 3]);
        assert_eq!(plan.first_kept, 5);
    }

    /// The count of the latest reply stands for the context, plus an estimate of what follows; a compaction makes counts from before it stale.
    #[test]
    fn counts_the_context() {
        let mut history = vec![
            user(1),
            Message::Assistant(Reply {
                tokens: Some(omc_providers::Tokens {
                    input: 500,
                    output: 20,
                    ..Default::default()
                }),
                ..Reply::default()
            }),
            user(10),
        ];
        assert_eq!(context_tokens(&history, ""), 530);
        history.push(Message::Compaction(Compaction {
            summary: "s".into(),
            first_kept: 2,
            ..Compaction::default()
        }));
        assert!(context_tokens(&history, "") < 200);
    }

    /// The threshold leaves a reserve that grows with the window, and a small window keeps a proportional one.
    #[test]
    fn the_threshold_leaves_room() {
        assert_eq!(threshold(200_000), 170_000);
        assert_eq!(threshold(80_000), 63_616);
        assert_eq!(threshold(1_000), 850);
        assert_eq!(keep_recent(Some(200_000)), KEEP_RECENT_TOKENS);
        assert_eq!(keep_recent(Some(1_000)), 212);
    }

    /// Long results are cut at a character boundary with a note of how much was left out.
    #[test]
    fn clips_long_text() {
        assert_eq!(clip("short", 10), "short");
        let clipped = clip("ééééé", 3);
        assert!(clipped.starts_with("é\n[... 8 more bytes cut]"));
    }

    /// A model that answers each summary request with a fixed text and keeps the prompts it was given.
    struct Summarizer {
        /// The prompts, in order.
        prompts: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl ChatModel for Summarizer {
        /// Records the prompt and returns a summary named after how many came before.
        async fn complete(
            &self,
            request: Request<'_>,
            _sink: &mut dyn Sink,
        ) -> Result<Reply, omc_providers::ProviderError> {
            let Message::User(prompt) = &request.messages[0] else {
                panic!("no prompt");
            };
            assert!(request.tools.is_empty());
            let mut prompts = self.prompts.lock().unwrap();
            prompts.push(prompt.clone());
            Ok(Reply {
                text: format!("summary {}", prompts.len()),
                ..Reply::default()
            })
        }
    }

    /// A split turn is summarized twice and merged, the earlier summary is built on, and the files read and changed carry over.
    #[tokio::test]
    async fn summarizes_with_the_previous_summary_and_the_files() {
        let mut history = vec![user(100), answer(100)];
        history.push(Message::Compaction(Compaction {
            summary: "before".into(),
            first_kept: 0,
            read_files: vec!["old.rs".into(), "a".into()],
            modified_files: vec!["changed.rs".into()],
            ..Compaction::default()
        }));
        history.extend([
            user(10),
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "e".into(),
                    name: "edit".into(),
                    arguments: json!({ "files": [{ "path": "a" }] }),
                }],
                ..Reply::default()
            }),
            results("e", 100),
            reading("b", 10),
            results("b", 100),
        ]);
        let plan = plan(&history, 150, 900).unwrap();
        let model = Summarizer {
            prompts: std::sync::Mutex::new(Vec::new()),
        };
        let done = summarize(&model, None, &history, &plan, Some("the tests"))
            .await
            .unwrap();
        let prompts = model.prompts.lock().unwrap();
        assert_eq!(prompts.len(), 2);
        assert!(prompts[0].contains("<previous-summary>\nbefore\n</previous-summary>"));
        assert!(prompts[0].contains("Give particular attention to: the tests"));
        assert!(prompts[1].contains("[Assistant tool calls]: edit("));
        assert!(done.summary.starts_with("summary 1"));
        assert!(done.summary.contains("## The turn under way\n\nsummary 2"));
        assert_eq!(done.first_kept, 6);
        assert_eq!(done.tokens_before, 900);
        assert_eq!(done.read_files, vec!["old.rs".to_owned()]);
        assert_eq!(
            done.modified_files,
            vec!["a".to_owned(), "changed.rs".to_owned()]
        );
    }
}
