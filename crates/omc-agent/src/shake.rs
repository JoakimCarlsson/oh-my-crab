//! Shaking: making room in a full context without asking a model, by cutting heavy content out of the older messages.
//!
//! A shake is the step between pruning and a summary. Like a pruning, it never deletes messages: it is recorded as a [`Pruning`] that names the tool outputs to leave out and the fenced blocks to cut out of earlier prompts and replies, and from then on the model is given a short note in place of each, which says how to read it again with `recall`. Everything in the most recent messages, about as much as a summary would keep, stays as it is.

use omc_providers::{Elided, Elision, Excised, Message, Pruning};

use crate::compaction::{
    elision_note, estimate, excision_note, hidden, live, result_tokens, shown,
};
use crate::plans;
use crate::pruning::{KEPT_TOOLS, seen};

/// The fewest tokens a shake must save to be worth breaking the provider's prompt cache for.
const MIN_SAVINGS: u64 = 4_000;

/// The fewest tokens a tool output must take to be left out.
const MIN_OUTPUT_TOKENS: u64 = 100;

/// The fewest tokens a fenced block must take to be cut out.
const MIN_BLOCK_TOKENS: u64 = 400;

/// The shake that makes room in `history`, keeping about `keep` tokens of the most recent messages as they are: every tool output of at least [`MIN_OUTPUT_TOKENS`] tokens older than those that the model has seen and that is not from one of [`KEPT_TOOLS`], and every fenced block of at least [`MIN_BLOCK_TOKENS`] tokens in an older prompt or reply, except the first message of a fresh context window opened for an approved plan, which holds the plan. Nothing when that saves less than [`MIN_SAVINGS`] tokens.
pub(crate) fn shake(history: &[Message], keep: u64) -> Option<Pruning> {
    let live = live(history);
    let hidden = hidden(history);
    let mut held = 0;
    let mut boundary = 0;
    for (index, &at) in live.iter().enumerate().rev() {
        held += estimate(&shown(history, at, &hidden));
        if held >= keep {
            boundary = index;
            break;
        }
    }
    let mut saved = 0;
    let mut elided = Vec::new();
    let mut excised = Vec::new();
    for &at in &live[..boundary] {
        match &history[at] {
            Message::ToolResults(results) if seen(history, at) => {
                for result in results {
                    if hidden.elided.contains_key(&(at, result.call_id.as_str()))
                        || KEPT_TOOLS.contains(&result.name.as_str())
                    {
                        continue;
                    }
                    let tokens = result_tokens(result);
                    let note = (elision_note(result, Elision::Shaken).len() as u64).div_ceil(4);
                    if tokens < MIN_OUTPUT_TOKENS || tokens <= note {
                        continue;
                    }
                    saved += tokens - note;
                    elided.push(Elided {
                        at,
                        call_id: result.call_id.clone(),
                        reason: Elision::Shaken,
                    });
                }
            }
            Message::User(_) if plans::seeds_plan(&history[at]) => {}
            Message::User(text) => saved += blocks(text, at, &hidden, &mut excised),
            Message::Assistant(reply) => saved += blocks(&reply.text, at, &hidden, &mut excised),
            _ => {}
        }
    }
    (saved >= MIN_SAVINGS && (!elided.is_empty() || !excised.is_empty())).then_some(Pruning {
        elided,
        excised,
        tokens_after: 0,
    })
}

/// Adds to `excised` every fenced block of at least [`MIN_BLOCK_TOKENS`] tokens in `text`, the text of the message at `at`, that `hidden` does not cut out already, and returns about how many tokens cutting them out saves.
fn blocks(
    text: &str,
    at: usize,
    hidden: &crate::compaction::Hidden,
    excised: &mut Vec<Excised>,
) -> u64 {
    let done = hidden.excised.get(&at);
    let mut saved = 0;
    for (start, end) in fenced(text) {
        if done.is_some_and(|done| done.contains(&(start, end))) {
            continue;
        }
        let block = &text[start..end];
        let tokens = (block.len() as u64).div_ceil(4);
        let note = (excision_note(block, at).len() as u64).div_ceil(4);
        if tokens < MIN_BLOCK_TOKENS || tokens <= note {
            continue;
        }
        saved += tokens - note;
        excised.push(Excised { at, start, end });
    }
    saved
}

/// The byte ranges of the fenced blocks in `text`, in order, each from the start of its opening fence line to the end of its closing one.
///
/// A fence is a line that starts, after any indentation, with three or more backticks or tildes; it is closed by a line of at least as many of the same character and nothing else. A block that is never closed is not counted.
pub(crate) fn fenced(text: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut open: Option<(usize, char, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        let Some(marker) = trimmed.chars().next().filter(|c| *c == '`' || *c == '~') else {
            continue;
        };
        let run = trimmed.chars().take_while(|c| *c == marker).count();
        if run < 3 {
            continue;
        }
        match open {
            None => open = Some((start, marker, run)),
            Some((opened, fence, length))
                if fence == marker && run >= length && trimmed[run..].trim().is_empty() =>
            {
                ranges.push((opened, start + content.len()));
                open = None;
            }
            Some(_) => {}
        }
    }
    ranges
}

#[cfg(test)]
/// Tests for choosing what a shake cuts out.
mod tests {
    use omc_providers::{Reply, ToolCall, ToolResult};
    use serde_json::json;

    use super::*;
    use crate::compaction::context;

    /// A reply that calls `name` under the id `id`.
    fn call(id: &str, name: &str) -> Message {
        Message::Assistant(Reply {
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments: json!({}),
            }],
            ..Reply::default()
        })
    }

    /// The output of the call `id` to `name`: `lines` lines of about 40 bytes each.
    fn output(id: &str, name: &str, lines: usize) -> Message {
        Message::ToolResults(vec![ToolResult {
            call_id: id.to_owned(),
            name: name.to_owned(),
            text: "0123456789012345678901234567890123456789\n".repeat(lines),
            is_error: false,
            images: Vec::new(),
        }])
    }

    /// A text with a short lead-in and a fenced `rust` block of `lines` lines of about 40 bytes each.
    fn with_block(lines: usize) -> String {
        format!(
            "Here is the code:\n```rust\n{}```\nThat is all.",
            "let x = 0123456789012345678901234567890;\n".repeat(lines)
        )
    }

    /// Fenced blocks are found whole, with longer and tilde fences, and an unclosed one is not.
    #[test]
    fn finds_fenced_blocks() {
        let text = "a\n```rust\nfn x() {}\n```\nb\n~~~~\n```\ninner\n~~~~\nc\n```\nopen";
        let ranges = fenced(text);
        assert_eq!(ranges.len(), 2);
        assert_eq!(&text[ranges[0].0..ranges[0].1], "```rust\nfn x() {}\n```");
        assert_eq!(&text[ranges[1].0..ranges[1].1], "~~~~\n```\ninner\n~~~~");
    }

    /// Old heavy outputs and large blocks in old replies are cut out, while the recent messages, small outputs and skills stay as they are; the notes say how to read them again.
    #[test]
    fn shakes_old_heavy_content_and_keeps_the_recent() {
        let history = vec![
            Message::User("go".into()),
            call("a", "bash"),
            output("a", "bash", 400),
            call("s", "skill"),
            output("s", "skill", 400),
            call("tiny", "grep"),
            output("tiny", "grep", 2),
            Message::Assistant(Reply {
                text: with_block(200),
                ..Reply::default()
            }),
            Message::User("more".into()),
            call("b", "bash"),
            output("b", "bash", 600),
            Message::Assistant(Reply {
                text: "ok".into(),
                ..Reply::default()
            }),
        ];
        let pruning = shake(&history, 5_000).expect("enough to save");
        let ids: Vec<&str> = pruning
            .elided
            .iter()
            .map(|elided| elided.call_id.as_str())
            .collect();
        assert_eq!(ids, ["a"]);
        assert_eq!(pruning.excised.len(), 1);
        assert_eq!(pruning.excised[0].at, 7);
        let mut shaken = history.clone();
        shaken.push(Message::Pruning(pruning));
        let given = context(&shaken);
        let Message::ToolResults(results) = &given[2] else {
            panic!("not tool results");
        };
        assert_eq!(
            results[0].text,
            "[400 lines of bash output removed to save room. Call recall with call_id \"a\" to read it again.]"
        );
        let Message::Assistant(reply) = &given[7] else {
            panic!("not a reply");
        };
        assert_eq!(
            reply.text,
            "Here is the code:\n[200 lines of rust block removed to save room. Call recall with messages \"7\" to read them again.]\nThat is all."
        );
        assert_eq!(given[10], history[10]);
        assert_eq!(shake(&shaken, 5_000), None, "nothing left worth shaking");
    }

    /// The plan is never shaken out: not the output of a submission, nor the blocks of the first message of a fresh context window opened for an approved plan, while other old content is.
    #[test]
    fn the_plan_is_never_shaken() {
        let plan = format!("# Plan\n{}", with_block(200));
        let history = vec![
            Message::User(omc_context::plan_fresh(&plan, false, None)),
            call("p", "exit_plan_mode"),
            output("p", "exit_plan_mode", 400),
            call("a", "bash"),
            output("a", "bash", 400),
            Message::User("more".into()),
            call("b", "bash"),
            output("b", "bash", 600),
        ];
        let pruning = shake(&history, 5_000).expect("enough to save");
        let ids: Vec<&str> = pruning
            .elided
            .iter()
            .map(|elided| elided.call_id.as_str())
            .collect();
        assert_eq!(ids, ["a"]);
        assert!(pruning.excised.is_empty());
    }

    /// Too little to cut out is left alone.
    #[test]
    fn too_little_is_not_shaken() {
        let history = vec![
            Message::User(with_block(10)),
            call("a", "bash"),
            output("a", "bash", 50),
            Message::Assistant(Reply {
                text: "ok".into(),
                ..Reply::default()
            }),
        ];
        assert_eq!(shake(&history, 10), None);
    }
}
