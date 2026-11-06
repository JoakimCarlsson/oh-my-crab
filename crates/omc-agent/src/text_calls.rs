//! Tool calls the model wrote out as text instead of making them.
//!
//! A reply with no tool calls whose text holds what is unmistakably a call is either turned into that call or answered with a notice telling the model to make it. Only two shapes count: a block marked as a tool call, such as `<tool_call>{"name": "read", "arguments": {...}}</tool_call>`, and a fenced JSON block that is a call of a known tool or the arguments of an `edit`. Any other code block, JSON included, is left alone.

use omc_providers::{Reply, Stop, ToolCall};
use serde_json::{Map, Value};

use crate::shake::fenced;

/// Told when the reply holds a call written as text that is not taken as a call.
const NOTICE: &str = include_str!("prompts/text_call.md");

/// The tags a block marked as a tool call is wrapped in.
const TAGS: [&str; 3] = ["tool_call", "function_call", "tool_use"];

/// The keys a call written as JSON gives its arguments under.
const ARGUMENT_KEYS: [&str; 4] = ["arguments", "parameters", "input", "args"];

/// The most bytes of text a reply may hold besides fenced calls for them to be made.
const MAX_PROSE: usize = 300;

/// What to do about a reply that wrote calls out as text.
#[derive(Debug, PartialEq)]
pub(crate) enum TextCalls {
    /// Make these calls: the reply with them added and their text taken out.
    Made(Reply),
    /// Send this notice along, since it is not clear enough that the text was meant to be run.
    Unclear(String),
}

/// One call found in the text of a reply.
struct Found {
    /// The byte range of the text that holds it.
    range: (usize, usize),
    /// The tool it calls.
    name: String,
    /// Whether it was marked as a tool call rather than only fenced.
    marked: bool,
    /// Its arguments.
    arguments: Value,
}

/// What to do about `reply` when it made no tool calls but its text holds calls of tools `known` names: the reply with the calls made, their ids built from `serial`, when every call is marked as one, or when the reply is fenced calls and little else; otherwise a notice for the model. Nothing when the reply made calls, was cut off, or holds none written as text.
pub(crate) fn recover(
    reply: &Reply,
    known: &dyn Fn(&str) -> bool,
    serial: usize,
) -> Option<TextCalls> {
    if !reply.tool_calls.is_empty() || reply.stop != Stop::Finished {
        return None;
    }
    let text = &reply.text;
    let mut found = marked(text, known);
    found.extend(
        fenced(text)
            .into_iter()
            .filter(|range| !found.iter().any(|call| overlaps(call.range, *range)))
            .filter_map(|range| fenced_call(text, range, known))
            .collect::<Vec<_>>(),
    );
    if found.is_empty() {
        return None;
    }
    found.sort_by_key(|call| call.range.0);
    let mut rest = String::new();
    let mut cursor = 0;
    for call in &found {
        rest.push_str(&text[cursor..call.range.0]);
        cursor = call.range.1;
    }
    rest.push_str(&text[cursor..]);
    let rest = rest.trim();
    let clear = found
        .iter()
        .all(|call| call.marked || rest.len() <= MAX_PROSE);
    if !clear {
        let mut names: Vec<String> = found
            .iter()
            .map(|call| format!("`{}`", call.name))
            .collect();
        names.dedup();
        return Some(TextCalls::Unclear(
            NOTICE.trim().replace("{tools}", &names.join(", ")),
        ));
    }
    let tool_calls = found
        .into_iter()
        .enumerate()
        .map(|(index, call)| ToolCall {
            id: format!("text_call_{serial}_{index}"),
            name: call.name,
            arguments: call.arguments,
        })
        .collect();
    Some(TextCalls::Made(Reply {
        text: rest.to_owned(),
        tool_calls,
        ..reply.clone()
    }))
}

/// Whether the byte ranges `a` and `b` share any byte.
fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// The calls of tools `known` names in the blocks of `text` marked with one of [`TAGS`], whose JSON may sit in a fence of its own; a marked block inside a fenced block is quoted, not made, and is left out.
fn marked(text: &str, known: &dyn Fn(&str) -> bool) -> Vec<Found> {
    let fences = fenced(text);
    let quoted =
        |start: usize, end: usize| fences.iter().any(|&(from, to)| from <= start && end <= to);
    let mut found = Vec::new();
    for tag in TAGS {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let mut from = 0;
        while let Some(start) = text[from..].find(&open).map(|at| at + from) {
            let body_start = start + open.len();
            let Some(body_end) = text[body_start..].find(&close).map(|at| at + body_start) else {
                break;
            };
            let end = body_end + close.len();
            from = end;
            if quoted(start, end) {
                continue;
            }
            let body = unfenced(text[body_start..body_end].trim());
            if let Ok(Value::Object(object)) = serde_json::from_str::<Value>(body)
                && let Some((name, arguments)) = call_shape(&object)
                && known(&name)
            {
                found.push(Found {
                    range: (start, end),
                    name,
                    marked: true,
                    arguments,
                });
            }
        }
    }
    found
}

/// `body` without a fence around it, when it is one fenced block.
fn unfenced(body: &str) -> &str {
    match fenced(body).as_slice() {
        [(0, end)] if *end == body.len() => {
            let inner = body.split_once('\n').map_or("", |(_, rest)| rest);
            inner.rsplit_once('\n').map_or("", |(inner, _)| inner)
        }
        _ => body,
    }
}

/// The call in the fenced block at `range` of `text`, when it is a JSON block, untagged or tagged as JSON, that is a call of a tool `known` names or the arguments of an `edit`.
fn fenced_call(text: &str, range: (usize, usize), known: &dyn Fn(&str) -> bool) -> Option<Found> {
    let block = &text[range.0..range.1];
    let (fence, rest) = block.split_once('\n')?;
    let info = fence.trim().trim_start_matches(['`', '~']).trim();
    if !matches!(info, "" | "json" | "jsonc" | "json5") {
        return None;
    }
    let body = rest.rsplit_once('\n').map_or("", |(body, _)| body);
    let Ok(Value::Object(object)) = serde_json::from_str::<Value>(body.trim()) else {
        return None;
    };
    let (name, arguments) = match call_shape(&object) {
        Some(call) => call,
        None if is_edit(&object) => ("edit".to_owned(), Value::Object(object)),
        None => return None,
    };
    known(&name).then_some(Found {
        range,
        name,
        marked: false,
        arguments,
    })
}

/// The tool and arguments of `object` when it is shaped as a call: a `name` or `tool` naming the tool, and its arguments as an object, or as a string holding one, under one of [`ARGUMENT_KEYS`]; nothing else.
fn call_shape(object: &Map<String, Value>) -> Option<(String, Value)> {
    let name_key = ["name", "tool", "tool_name"]
        .into_iter()
        .find(|key| object.contains_key(*key))?;
    let name = object[name_key].as_str()?.trim().to_owned();
    let arguments_key = ARGUMENT_KEYS
        .into_iter()
        .find(|key| object.contains_key(*key))?;
    if object.len() != 2 || name.is_empty() {
        return None;
    }
    let arguments = match &object[arguments_key] {
        Value::Object(arguments) => Value::Object(arguments.clone()),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(Value::Object(arguments)) => Value::Object(arguments),
            _ => return None,
        },
        _ => return None,
    };
    Some((name, arguments))
}

/// Whether `object` is the arguments of an `edit`: a non-empty list of files, each with a path and a list of operations.
fn is_edit(object: &Map<String, Value>) -> bool {
    object.len() == 1
        && object
            .get("files")
            .and_then(Value::as_array)
            .is_some_and(|files| {
                !files.is_empty()
                    && files
                        .iter()
                        .all(|file| file["path"].is_string() && file["ops"].is_array())
            })
}

#[cfg(test)]
/// Tests for finding calls written as text.
mod tests {
    use serde_json::json;

    use super::*;

    /// Whether `name` is a tool of the built-in set the tests pretend to have.
    fn known(name: &str) -> bool {
        matches!(name, "read" | "edit" | "bash")
    }

    /// A finished reply saying `text`.
    fn said(text: &str) -> Reply {
        Reply {
            text: text.into(),
            ..Reply::default()
        }
    }

    /// The arguments of an edit of `a.rs`.
    fn edit() -> Value {
        json!({"files": [{"path": "a.rs", "tag": "T", "ops": [{"op": "replace", "line": 1, "text": "x"}]}]})
    }

    /// A marked call is made, wherever it stands, and taken out of the text.
    #[test]
    fn makes_a_marked_call() {
        let reply = said(
            "I will read it now.\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>",
        );
        let Some(TextCalls::Made(made)) = recover(&reply, &known, 4) else {
            panic!("not made");
        };
        assert_eq!(made.text, "I will read it now.");
        assert_eq!(made.tool_calls.len(), 1);
        assert_eq!(made.tool_calls[0].id, "text_call_4_0");
        assert_eq!(made.tool_calls[0].name, "read");
        assert_eq!(made.tool_calls[0].arguments, json!({"path": "a.rs"}));
    }

    /// The arguments of an edit fenced as JSON, with little else said, are made into an edit.
    #[test]
    fn makes_a_fenced_edit() {
        let reply = said(&format!("Applying the fix:\n```json\n{}\n```", edit()));
        let Some(TextCalls::Made(made)) = recover(&reply, &known, 0) else {
            panic!("not made");
        };
        assert_eq!(made.text, "Applying the fix:");
        assert_eq!(made.tool_calls[0].name, "edit");
        assert_eq!(made.tool_calls[0].arguments, edit());
    }

    /// A fenced call amid a long answer is not run; the model is asked instead.
    #[test]
    fn a_call_amid_prose_is_asked_about() {
        let reply = said(&format!(
            "{}\n```json\n{}\n```\n{}",
            "Here is how the tool is used. ".repeat(10),
            json!({"name": "bash", "arguments": {"command": "make"}}),
            "That is all there is to it. ".repeat(10)
        ));
        let Some(TextCalls::Unclear(notice)) = recover(&reply, &known, 0) else {
            panic!("not asked about");
        };
        assert!(notice.contains("`bash`"), "{notice}");
    }

    /// Ordinary code, other JSON, calls of unknown tools, and replies that made calls or were cut off are left alone.
    #[test]
    fn leaves_ordinary_code_alone() {
        for text in [
            "```rust\nfn main() {}\n```",
            "```json\n{\"name\": \"serde\", \"version\": \"1\"}\n```",
            "```json\n{\"name\": \"deploy\", \"arguments\": {}}\n```",
            "```python\n{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n```",
            "<tool_call>not json</tool_call>",
            "The format is:\n```\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>\n```",
            "Plain answer.",
        ] {
            assert_eq!(recover(&said(text), &known, 0), None, "{text}");
        }
        let marked =
            "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>";
        let mut called = said(marked);
        called.tool_calls.push(ToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: json!({}),
        });
        assert_eq!(recover(&called, &known, 0), None);
        let cut = Reply {
            stop: Stop::Length,
            ..said(marked)
        };
        assert_eq!(recover(&cut, &known, 0), None);
    }
}
