//! Pruning: leaving tool outputs the model no longer needs out of its context, without asking a model for anything.
//!
//! A pruning never deletes messages. It appends a [`Pruning`] to the history that names the outputs to leave out; from then on the model is given a short note in place of each, which says how to read it again with `recall`. Two kinds of output are left out:
//!
//! - **Superseded reads:** a read whose exact arguments were read again later is out of date. These are left out as soon as leaving them out costs little of the provider's prompt cache, and whenever the history is pruned.
//! - **Old outputs:** when the context nears the compaction threshold, large tool outputs older than the most recent ones are left out, before any summary is asked for.

use std::collections::{HashMap, HashSet};

use omc_providers::{Elided, Elision, Message, Pruning, ToolResult};

use crate::compaction::{elision_note, elisions, estimate, hidden, live, result_tokens, shown};

/// About how many tokens of the most recent tool outputs are never left out as old.
const PROTECT_TOKENS: u64 = 40_000;

/// The fewest tokens a pruning of old outputs must save to be worth breaking the provider's prompt cache for.
const MIN_SAVINGS: u64 = 20_000;

/// The fewest tokens an old output must take to be left out.
const MIN_OUTPUT_TOKENS: u64 = 50;

/// The most tokens the messages after a superseded read may take for it to be left out between prunings, since the provider reads everything after it again without its prompt cache.
const STALE_SUFFIX_TOKENS: u64 = 8_000;

/// The tools whose outputs are never left out as old: instructions and notes the model works from.
pub(crate) const KEPT_TOOLS: [&str; 3] = ["skill", "context_notes", "todo"];

/// One tool output in the history that may be left out.
struct Candidate<'h> {
    /// The position of the tool results it is one of.
    at: usize,
    /// The output.
    result: &'h ToolResult,
}

impl Candidate<'_> {
    /// About how many tokens leaving the output out for `reason` saves; none when its note is no shorter than it.
    fn savings(&self, reason: Elision) -> u64 {
        let note = (elision_note(self.result, reason).len() as u64).div_ceil(4);
        result_tokens(self.result).saturating_sub(note)
    }

    /// The record that leaves the output out for `reason`.
    fn elide(&self, reason: Elision) -> Elided {
        Elided {
            at: self.at,
            call_id: self.result.call_id.clone(),
            reason,
        }
    }
}

/// The tool outputs among the messages the model is given in `history` that have not been left out yet, oldest first, each with the call it answers.
fn outputs(history: &[Message]) -> Vec<Candidate<'_>> {
    let elided = elisions(history);
    live(history)
        .into_iter()
        .filter_map(|at| match &history[at] {
            Message::ToolResults(results) => Some((at, results)),
            _ => None,
        })
        .flat_map(|(at, results)| results.iter().map(move |result| Candidate { at, result }))
        .filter(|candidate| {
            !elided.contains_key(&(candidate.at, candidate.result.call_id.as_str()))
        })
        .collect()
}

/// Whether the model has seen the message at `at` in `history`: a reply came after it.
pub(crate) fn seen(history: &[Message], at: usize) -> bool {
    history[at + 1..]
        .iter()
        .any(|message| matches!(message, Message::Assistant(_)))
}

/// The arguments of every `read` call in `history` the model is given, by call id, in a form two calls compare equal in when they read the same thing.
fn reads(history: &[Message]) -> HashMap<&str, String> {
    live(history)
        .into_iter()
        .filter_map(|at| match &history[at] {
            Message::Assistant(reply) => Some(&reply.tool_calls),
            _ => None,
        })
        .flatten()
        .filter(|call| call.name == "read")
        .map(|call| {
            let key = ["path", "offset", "limit"]
                .map(|field| call.arguments[field].to_string())
                .join("\u{0}");
            (call.id.as_str(), key)
        })
        .collect()
}

/// The successful reads in `candidates` that a later successful read of the same thing makes out of date, and that the model has seen.
fn superseded<'c, 'h>(
    history: &'h [Message],
    candidates: &'c [Candidate<'h>],
) -> Vec<&'c Candidate<'h>> {
    let reads = reads(history);
    let key = |candidate: &Candidate| {
        (candidate.result.name == "read" && !candidate.result.is_error)
            .then(|| reads.get(candidate.result.call_id.as_str()))
            .flatten()
    };
    let mut later = HashSet::new();
    let mut out_of_date = Vec::new();
    for candidate in candidates.iter().rev() {
        let Some(key) = key(candidate) else {
            continue;
        };
        if !later.insert(key) && seen(history, candidate.at) {
            out_of_date.push(candidate);
        }
    }
    out_of_date.reverse();
    out_of_date
}

/// The pruning that leaves out the superseded reads of `history` that cost little of the prompt cache to leave out now: those the messages after which take at most [`STALE_SUFFIX_TOKENS`], and that are longer than their note. Nothing when there are none.
pub(crate) fn stale(history: &[Message]) -> Option<Pruning> {
    let candidates = outputs(history);
    let hidden = hidden(history);
    let after = |at: usize| -> u64 {
        (at + 1..history.len())
            .map(|position| estimate(&shown(history, position, &hidden)))
            .sum()
    };
    let elided: Vec<Elided> = superseded(history, &candidates)
        .into_iter()
        .filter(|candidate| candidate.savings(Elision::Superseded) > 0)
        .filter(|candidate| after(candidate.at) <= STALE_SUFFIX_TOKENS)
        .map(|candidate| candidate.elide(Elision::Superseded))
        .collect();
    (!elided.is_empty()).then_some(Pruning {
        elided,
        ..Pruning::default()
    })
}

/// The pruning that makes room in a nearly full context of `history`: every superseded read, and every old output of at least [`MIN_OUTPUT_TOKENS`] tokens that the model has seen, outside the most recent [`PROTECT_TOKENS`] of tool output and not from one of [`KEPT_TOOLS`]. Nothing when that saves less than [`MIN_SAVINGS`] tokens.
pub(crate) fn prune(history: &[Message]) -> Option<Pruning> {
    let candidates = outputs(history);
    let out_of_date: HashSet<(usize, &str)> = superseded(history, &candidates)
        .into_iter()
        .map(|candidate| (candidate.at, candidate.result.call_id.as_str()))
        .collect();
    let mut protected = 0;
    let mut elided = Vec::new();
    let mut saved = 0;
    for candidate in candidates.iter().rev() {
        let id = (candidate.at, candidate.result.call_id.as_str());
        let tokens = result_tokens(candidate.result);
        let reason = if out_of_date.contains(&id) {
            Elision::Superseded
        } else if protected < PROTECT_TOKENS
            || !seen(history, candidate.at)
            || KEPT_TOOLS.contains(&candidate.result.name.as_str())
        {
            protected += tokens;
            continue;
        } else if tokens < MIN_OUTPUT_TOKENS {
            continue;
        } else {
            Elision::Old
        };
        let savings = candidate.savings(reason);
        if savings > 0 {
            saved += savings;
            elided.push(candidate.elide(reason));
        }
    }
    elided.reverse();
    (saved >= MIN_SAVINGS).then_some(Pruning {
        elided,
        ..Pruning::default()
    })
}

#[cfg(test)]
/// Tests for choosing the tool outputs to leave out.
mod tests {
    use omc_providers::{Reply, ToolCall};
    use serde_json::{Value, json};

    use super::*;
    use crate::compaction::context;

    /// A reply that calls `name` with `arguments` under the id `id`.
    fn call(id: &str, name: &str, arguments: Value) -> Message {
        Message::Assistant(Reply {
            tool_calls: vec![ToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments,
            }],
            ..Reply::default()
        })
    }

    /// The result of the call `id` to `name`, about `tokens` tokens long.
    fn output(id: &str, name: &str, tokens: usize) -> Message {
        Message::ToolResults(vec![ToolResult {
            call_id: id.to_owned(),
            name: name.to_owned(),
            text: "x".repeat(tokens * 4),
            is_error: false,
            images: Vec::new(),
        }])
    }

    /// A reply with text only.
    fn answer() -> Message {
        Message::Assistant(Reply {
            text: "ok".into(),
            ..Reply::default()
        })
    }

    /// A read of the same path made again leaves the first one out before the next request, while the suffix after it is small.
    #[test]
    fn a_read_made_again_is_left_out() {
        let mut history = vec![
            Message::User("go".into()),
            call("a", "read", json!({"path": "src/lib.rs"})),
            output("a", "read", 500),
            call("b", "read", json!({"path": "src/lib.rs"})),
            output("b", "read", 500),
        ];
        let pruning = stale(&history).expect("the first read is out of date");
        assert_eq!(
            pruning.elided,
            [Elided {
                at: 2,
                call_id: "a".into(),
                reason: Elision::Superseded
            }]
        );
        history.push(Message::Pruning(pruning));
        let Message::ToolResults(results) = &context(&history)[2] else {
            panic!("not tool results");
        };
        assert!(
            results[0].text.contains("call_id \"a\""),
            "{}",
            results[0].text
        );
        assert_eq!(stale(&history), None, "it is left out once");
    }

    /// Reads of other ranges of a file, and reads that failed, do not make a read out of date.
    #[test]
    fn other_ranges_and_failures_do_not_supersede() {
        let mut failed = output("c", "read", 500);
        if let Message::ToolResults(results) = &mut failed {
            results[0].is_error = true;
        }
        let history = vec![
            Message::User("go".into()),
            call("a", "read", json!({"path": "src/lib.rs"})),
            output("a", "read", 500),
            call("b", "read", json!({"path": "src/lib.rs:1-20"})),
            output("b", "read", 500),
            call("c", "read", json!({"path": "src/lib.rs"})),
            failed,
            answer(),
        ];
        assert_eq!(stale(&history), None);
    }

    /// A bare read of a code file, which may be an outline with bodies folded,
    /// and reads of ranges or of the whole text of the same file never make one
    /// another out of date, in either order; only the same read made again does.
    #[test]
    fn outlines_and_ranges_do_not_supersede_each_other() {
        let history = vec![
            Message::User("go".into()),
            call("a", "read", json!({"path": "src/lib.rs:120-180"})),
            output("a", "read", 500),
            call("b", "read", json!({"path": "src/lib.rs"})),
            output("b", "read", 500),
            call("c", "read", json!({"path": "src/lib.rs:1-"})),
            output("c", "read", 500),
            call("d", "read", json!({"path": "src/lib.rs:120-180"})),
            output("d", "read", 500),
            answer(),
        ];
        let ids: Vec<String> = stale(&history)
            .expect("the repeated range read is out of date")
            .elided
            .into_iter()
            .map(|elided| elided.call_id)
            .collect();
        assert_eq!(ids, ["a"]);
    }

    /// A superseded read far back costs too much of the cache to leave out between prunings.
    #[test]
    fn a_read_far_back_waits_for_the_next_pruning() {
        let history = vec![
            Message::User("go".into()),
            call("a", "read", json!({"path": "a"})),
            output("a", "read", 500),
            call("b", "read", json!({"path": "a"})),
            output("b", "read", 500),
            call("c", "bash", json!({"command": "make"})),
            output("c", "bash", 9_000),
            answer(),
        ];
        assert_eq!(stale(&history), None);
        assert_eq!(prune(&history), None, "too little to save");
    }

    /// Old outputs past the protected recent ones are left out, the recent ones, small ones and skills are kept, and nothing is left out when it saves too little.
    #[test]
    fn prunes_old_outputs_outside_the_recent_ones() {
        let mut history = vec![Message::User("go".into())];
        for (id, name, tokens) in [
            ("s", "skill", 5_000),
            ("tiny", "bash", 40),
            ("old1", "bash", 15_000),
            ("old2", "grep", 15_000),
            ("new1", "bash", 30_000),
            ("new2", "bash", 15_000),
        ] {
            history.push(call(id, name, json!({})));
            history.push(output(id, name, tokens));
        }
        history.push(answer());
        let pruning = prune(&history).expect("enough to save");
        let ids: Vec<&str> = pruning
            .elided
            .iter()
            .map(|elided| elided.call_id.as_str())
            .collect();
        assert_eq!(ids, ["old1", "old2"]);
        assert!(pruning.elided.iter().all(|e| e.reason == Elision::Old));
        history.push(Message::Pruning(pruning));
        assert_eq!(prune(&history), None, "nothing left worth pruning");
    }

    /// The outputs of the latest batch are never left out before the model has seen them, however large.
    #[test]
    fn unseen_outputs_are_kept() {
        let batch = Message::Assistant(Reply {
            tool_calls: ["b", "c"]
                .map(|id| ToolCall {
                    id: id.to_owned(),
                    name: "bash".to_owned(),
                    arguments: json!({}),
                })
                .to_vec(),
            ..Reply::default()
        });
        let Message::ToolResults(mut results) = output("b", "bash", 45_000) else {
            unreachable!("output builds tool results");
        };
        results.push(ToolResult {
            call_id: "c".into(),
            ..results[0].clone()
        });
        let history = vec![
            Message::User("go".into()),
            call("a", "bash", json!({})),
            output("a", "bash", 30_000),
            batch,
            Message::ToolResults(results),
        ];
        let ids: Vec<String> = prune(&history)
            .expect("enough to save")
            .elided
            .into_iter()
            .map(|elided| elided.call_id)
            .collect();
        assert_eq!(ids, ["a"]);
    }
}
