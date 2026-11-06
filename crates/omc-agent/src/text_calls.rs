//! Tool calls the model wrote out as text instead of making them.
//!
//! Nothing in the text of a reply is ever run: text the model quotes, from a file it read or from JSON the user pasted, must not turn into a call. A reply with no tool calls whose text holds what is unmistakably a call only earns a notice telling the model that nothing ran and to make a real call if it meant to. Only two shapes count: a block marked as a tool call, such as `<tool_call>{"name": "read", "arguments": {...}}</tool_call>`, and a fenced JSON block that is a call of a known tool or the arguments of an `edit`. Any other code block, JSON included, is left alone.

use omc_providers::{Reply, Stop};
use serde_json::{Map, Value};

use crate::shake::fenced;

/// Told when the reply holds a call written as text.
const NOTICE: &str = include_str!("prompts/text_call.md");

/// The tags a block marked as a tool call is wrapped in.
const TAGS: [&str; 3] = ["tool_call", "function_call", "tool_use"];

/// The keys a call written as JSON gives its arguments under.
const ARGUMENT_KEYS: [&str; 4] = ["arguments", "parameters", "input", "args"];

/// One call found in the text of a reply.
struct Found {
    /// The byte range of the text that holds it.
    range: (usize, usize),
    /// The tool it calls.
    name: String,
}

/// The notice to send the model when `reply` made no tool calls, finished, and its text holds calls of tools `known` names written out as text, naming those tools; nothing otherwise.
pub(crate) fn notice(reply: &Reply, known: &dyn Fn(&str) -> bool) -> Option<String> {
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
    let mut names: Vec<String> = found
        .iter()
        .map(|call| format!("`{}`", call.name))
        .collect();
    names.dedup();
    Some(NOTICE.trim().replace("{tools}", &names.join(", ")))
}

/// Whether the byte ranges `a` and `b` share any byte.
fn overlaps(a: (usize, usize), b: (usize, usize)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

/// The calls of tools `known` names in the blocks of `text` marked with one of [`TAGS`], whose JSON may sit in a fence of its own; a marked block inside a fenced block is quoted as an example and is left out.
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
                && let Some(name) = call_shape(&object)
                && known(&name)
            {
                found.push(Found {
                    range: (start, end),
                    name,
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
    let name = match call_shape(&object) {
        Some(name) => name,
        None if is_edit(&object) => "edit".to_owned(),
        None => return None,
    };
    known(&name).then_some(Found { range, name })
}

/// The tool `object` calls when it is shaped as a call: a `name` or `tool` naming the tool, and its arguments as an object, or as a string holding one, under one of [`ARGUMENT_KEYS`]; nothing else.
fn call_shape(object: &Map<String, Value>) -> Option<String> {
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
    let readable = match &object[arguments_key] {
        Value::Object(_) => true,
        Value::String(raw) => matches!(serde_json::from_str::<Value>(raw), Ok(Value::Object(_))),
        _ => false,
    };
    readable.then_some(name)
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
    use omc_providers::ToolCall;
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

    /// A marked call, wherever it stands, earns a notice naming its tool.
    #[test]
    fn a_marked_call_earns_a_notice() {
        let reply = said(
            "I will read it now.\n<tool_call>\n{\"name\": \"read\", \"arguments\": {\"path\": \"a.rs\"}}\n</tool_call>",
        );
        let notice = notice(&reply, &known).unwrap();
        assert!(notice.contains("`read`"), "{notice}");
        assert!(notice.contains("never run"), "{notice}");
    }

    /// Fenced calls and the arguments of an edit fenced as JSON earn a notice, however little or much else is said, and each tool is named once.
    #[test]
    fn fenced_calls_earn_a_notice() {
        let edit = json!({"files": [{"path": "a.rs", "tag": "T", "ops": [{"op": "replace", "line": 1, "text": "x"}]}]});
        let short = notice(&said(&format!("Fixing:\n```json\n{edit}\n```")), &known).unwrap();
        assert!(short.contains("`edit`"), "{short}");
        let bash = json!({"name": "bash", "arguments": {"command": "make"}});
        let long = said(&format!(
            "{}\n```json\n{bash}\n```\n```json\n{bash}\n```\n{}",
            "Here is how the tool is used. ".repeat(10),
            "That is all there is to it. ".repeat(10)
        ));
        let asked = notice(&long, &known).unwrap();
        assert_eq!(asked.matches("`bash`").count(), 1, "{asked}");
    }

    /// Ordinary code, other JSON, calls of unknown tools, and replies that made calls or were cut off are left alone.
    #[test]
    fn leaves_ordinary_code_alone() {
        for text in [
            "```rust\nfn main() {}\n```",
            "```json\n{\"name\": \"serde\", \"version\": \"1\"}\n```",
            "```json\n{\"name\": \"deploy\", \"arguments\": {}}\n```",
            "```json\n{\"name\": \"read\", \"arguments\": [1]}\n```",
            "```python\n{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n```",
            "<tool_call>not json</tool_call>",
            "The format is:\n```\n<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>\n```",
            "Plain answer.",
        ] {
            assert_eq!(notice(&said(text), &known), None, "{text}");
        }
        let marked =
            "<tool_call>{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}</tool_call>";
        let mut called = said(marked);
        called.tool_calls.push(ToolCall {
            id: "c".into(),
            name: "read".into(),
            arguments: json!({}),
        });
        assert_eq!(notice(&called, &known), None);
        let cut = Reply {
            stop: Stop::Length,
            ..said(marked)
        };
        assert_eq!(notice(&cut, &known), None);
    }
}
