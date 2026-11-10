//! The `recall` tool: reading the whole history again, including what a compaction summarized and the tool outputs a pruning left out.
//!
//! The agent answers it from the history it holds, so it is offered only once the history holds something the model is no longer given in full.

use std::fmt::Write as _;

use omc_providers::{Image, Message, ToolSpec};
use serde::Deserialize;
use serde_json::{Value, json};

/// The name the model calls the tool by.
pub const RECALL: &str = "recall";

/// The most bytes of history one call returns.
const MAX_BYTES: usize = 50_000;

/// The most matching lines a search returns.
const MAX_MATCHES: usize = 100;

/// The most bytes of a matching line a search shows.
const MAX_LINE: usize = 300;

/// What the tool is, for the model.
const DESCRIPTION: &str = "Read the earlier conversation again, including the messages a compaction summarized and the tool outputs elided to save room. Each message has a number, shown as #N.\n\
- call_id: the full output of one tool call, with its arguments, such as one an elision note names.\n\
- search: the lines of the whole history that contain this text, ignoring case, with the numbers of their messages.\n\
- messages: the messages numbered in a range, such as 120-140, in full.\n\
Pass exactly one of them.";

/// Arguments of a `recall` call.
#[derive(Deserialize)]
struct Args {
    /// The id of the call whose output to return.
    call_id: Option<String>,
    /// Text to look for.
    search: Option<String>,
    /// A range of message numbers, such as `120-140`.
    messages: Option<String>,
}

/// Whether the tool is offered with `history`: when the model is no longer given all of it as it is, because it was compacted or pruned.
pub(crate) fn offered(history: &[Message]) -> bool {
    history
        .iter()
        .any(|message| matches!(message, Message::Compaction(_) | Message::Pruning(_)))
}

/// The `recall` tool as it is offered to the model once the history was pruned or compacted, for a client that lists it from the start so the tool list does not change then.
pub fn spec() -> ToolSpec {
    ToolSpec {
        name: RECALL.to_owned(),
        description: DESCRIPTION.to_owned(),
        schema: json!({
            "type": "object",
            "properties": {
                "call_id": {"type": "string", "description": "Id of the tool call whose full output to return."},
                "search": {"type": "string", "description": "Text to find in the whole history, ignoring case."},
                "messages": {"type": "string", "description": "Range of message numbers to return in full, such as 120-140."}
            }
        }),
    }
}

/// A one-line description of a call with `arguments`, shown to the user.
pub(crate) fn describe(arguments: &Value) -> String {
    ["call_id", "search", "messages"]
        .into_iter()
        .find_map(|field| {
            arguments[field]
                .as_str()
                .map(|value| format!("{field} {value}"))
        })
        .unwrap_or_else(|| arguments.to_string())
}

/// Answers a call with `arguments` from `history`: the text of the result and the images of a recalled output.
///
/// # Errors
///
/// Returns why, for the model, when the arguments do not name exactly one of the three ways to recall, or name a call or a range that is not in the history.
pub(crate) fn run(history: &[Message], arguments: Value) -> Result<(String, Vec<Image>), String> {
    let args: Args = serde_json::from_value(arguments).map_err(|error| error.to_string())?;
    match (args.call_id, args.search, args.messages) {
        (Some(id), None, None) => output(history, &id),
        (None, Some(text), None) => search(history, &text).map(|found| (found, Vec::new())),
        (None, None, Some(range)) => messages(history, &range).map(|text| (text, Vec::new())),
        _ => Err("pass exactly one of call_id, search and messages".to_owned()),
    }
}

/// The full output of the call `id` in `history`, after its arguments, with its images.
///
/// # Errors
///
/// Returns a message for the model when no output answers a call with that id.
fn output(history: &[Message], id: &str) -> Result<(String, Vec<Image>), String> {
    let arguments = history
        .iter()
        .filter_map(|message| match message {
            Message::Assistant(reply) => Some(&reply.tool_calls),
            _ => None,
        })
        .flatten()
        .find(|call| call.id == id)
        .map(|call| call.arguments.to_string());
    let (at, result) = history
        .iter()
        .enumerate()
        .filter_map(|(at, message)| match message {
            Message::ToolResults(results) => Some((at, results)),
            _ => None,
        })
        .find_map(|(at, results)| {
            results
                .iter()
                .find(|result| result.call_id == id)
                .map(|result| (at, result))
        })
        .ok_or_else(|| format!("no tool output answers a call with id {id}"))?;
    let mut text = format!(
        "#{at} {}{} call {id}, arguments {}:\n{}",
        result.name,
        if result.is_error { " error of" } else { "" },
        arguments.unwrap_or_else(|| "unknown".to_owned()),
        result.text
    );
    clip(&mut text, "search for the part you need");
    Ok((text, result.images.clone()))
}

/// The lines of the whole of `history` that contain `text`, ignoring case, each after the number of its message.
///
/// # Errors
///
/// Returns a message for the model when `text` is empty.
fn search(history: &[Message], text: &str) -> Result<String, String> {
    let needle = text.to_lowercase();
    if needle.trim().is_empty() {
        return Err("search needs some text to look for".to_owned());
    }
    let mut found = Vec::new();
    let mut more = 0;
    for (at, message) in history.iter().enumerate() {
        for line in render(at, message).lines().skip(1) {
            if !line.to_lowercase().contains(&needle) {
                continue;
            }
            if found.len() == MAX_MATCHES {
                more += 1;
                continue;
            }
            let mut line = line.trim().to_owned();
            clip_to(&mut line, MAX_LINE);
            found.push(format!("#{at}: {line}"));
        }
    }
    if found.is_empty() {
        return Ok(format!("nothing in the history contains {text:?}"));
    }
    let mut out = found.join("\n");
    if more > 0 {
        let _ = write!(
            out,
            "\n[{more} more matching lines; search for something narrower]"
        );
    }
    Ok(out)
}

/// The messages of `history` numbered in `range`, such as `120-140` or `7`, in full, up to [`MAX_BYTES`].
///
/// # Errors
///
/// Returns a message for the model when `range` is not a range of numbers of messages in the history.
fn messages(history: &[Message], range: &str) -> Result<String, String> {
    let bad = || format!("{range:?} is not a range of message numbers, such as 120-140");
    let (first, last) = match range.trim().split_once('-') {
        Some((first, last)) => (first.trim(), last.trim()),
        None => (range.trim(), range.trim()),
    };
    let first: usize = first.trim_start_matches('#').parse().map_err(|_| bad())?;
    let last: usize = last.trim_start_matches('#').parse().map_err(|_| bad())?;
    if first > last || first >= history.len() {
        return Err(format!(
            "{range:?} holds no messages: they are numbered 0 to {}",
            history.len().saturating_sub(1)
        ));
    }
    let last = last.min(history.len() - 1);
    let mut out = String::new();
    for (at, message) in history.iter().enumerate().take(last + 1).skip(first) {
        let text = render(at, message);
        if text.is_empty() {
            continue;
        }
        if !out.is_empty() && out.len() + text.len() > MAX_BYTES {
            let _ = write!(
                out,
                "\n\n[stopped before #{at} to stay short; ask for messages {at}-{last} to go on]"
            );
            return Ok(out);
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&text);
    }
    clip(&mut out, "ask for fewer messages or search");
    Ok(out)
}

/// The message at `at` as text, under a header line naming its number and kind; empty for a pruning, which holds nothing of the conversation.
fn render(at: usize, message: &Message) -> String {
    match message {
        Message::User(text) => format!("#{at} user:\n{text}"),
        Message::Images(images) => format!("#{at} user attached {} image(s)", images.len()),
        Message::Assistant(reply) => {
            let mut out = format!("#{at} assistant:");
            if !reply.text.is_empty() {
                let _ = write!(out, "\n{}", reply.text);
            }
            for call in &reply.tool_calls {
                let _ = write!(
                    out,
                    "\ncalls {} (call {}) with {}",
                    call.name, call.id, call.arguments
                );
            }
            out
        }
        Message::ToolResults(results) => {
            let mut out = format!("#{at} tool results:");
            for result in results {
                let label = if result.is_error { "error" } else { "output" };
                let _ = write!(
                    out,
                    "\n{label} of {} (call {}):\n{}",
                    result.name, result.call_id, result.text
                );
            }
            out
        }
        Message::Compaction(compaction) => {
            format!("#{at} compaction summary:\n{}", compaction.summary)
        }
        Message::Pruning(_) => String::new(),
    }
}

/// Cuts `text` to [`MAX_BYTES`], ending it with a note that says how much was cut and to `instead`.
fn clip(text: &mut String, instead: &str) {
    let total = text.len();
    if total > MAX_BYTES {
        clip_to(text, MAX_BYTES);
        let _ = write!(text, "\n[{} more bytes cut; {instead}]", total - text.len());
    }
}

/// Cuts `text` to at most `limit` bytes, at a character boundary.
fn clip_to(text: &mut String, limit: usize) {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
}

#[cfg(test)]
/// Tests for reading the history again.
mod tests {
    use omc_providers::{Compaction, Reply, ToolCall, ToolResult};

    use super::*;

    /// A history with one call, its output, and a compaction after them.
    fn history() -> Vec<Message> {
        vec![
            Message::User("find the bug".into()),
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: json!({"command": "make test"}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "bash".into(),
                text: "ok 1\nFAILED parser::nested\nok 3".into(),
                is_error: false,
                images: Vec::new(),
            }]),
            Message::Compaction(Compaction {
                summary: "Ran the tests.".into(),
                ..Compaction::default()
            }),
        ]
    }

    /// Only a compacted or pruned history offers the tool.
    #[test]
    fn offered_once_compacted() {
        let mut history = history();
        assert!(offered(&history));
        history.pop();
        assert!(!offered(&history));
    }

    /// An output comes back whole, with the call's arguments.
    #[test]
    fn recalls_an_output() {
        let (text, _) = run(&history(), json!({"call_id": "c1"})).unwrap();
        assert_eq!(
            text,
            "#2 bash call c1, arguments {\"command\":\"make test\"}:\nok 1\nFAILED parser::nested\nok 3"
        );
        assert!(run(&history(), json!({"call_id": "nope"})).is_err());
    }

    /// A search finds lines anywhere in the history, ignoring case.
    #[test]
    fn searches_the_whole_history() {
        let found = search(&history(), "failed").unwrap();
        assert_eq!(found, "#2: FAILED parser::nested");
        assert!(search(&history(), "zzz").unwrap().starts_with("nothing"));
    }

    /// A range of messages comes back in full, and a range outside the history is refused.
    #[test]
    fn reads_a_range() {
        let text = messages(&history(), "0-1").unwrap();
        assert_eq!(
            text,
            "#0 user:\nfind the bug\n\n#1 assistant:\ncalls bash (call c1) with {\"command\":\"make test\"}"
        );
        assert!(messages(&history(), "9-12").is_err());
        assert!(messages(&history(), "x").is_err());
    }

    /// Exactly one way to recall must be named.
    #[test]
    fn needs_exactly_one_argument() {
        assert!(run(&history(), json!({})).is_err());
        assert!(run(&history(), json!({"search": "a", "messages": "1"})).is_err());
    }
}
