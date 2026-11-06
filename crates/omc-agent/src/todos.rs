//! Keeping the model's todo list current: reminders when it works on for long without updating a list that still has open items, and the open items restated after a compaction hides the list.
//!
//! The list is read from the history, from the latest `todo` call that succeeded, so it survives a reopened conversation.

use std::collections::HashMap;

use omc_providers::{Message, Reply, ToolCall, ToolResult};
use serde_json::Value;

/// The tool the model keeps its list with.
const TODO: &str = "todo";

/// The tools whose calls count as changing files.
const MUTATING: [&str; 4] = ["edit", "write", "ast_edit", "bash"];

/// How many file-changing calls since the list last changed earn a reminder.
pub(crate) const CHANGES: u32 = 12;

/// How many reminders are given between two changes of the list.
pub(crate) const MAX_REMINDERS: u32 = 2;

/// Added to a batch of results when the list has gone stale.
const REMINDER: &str = include_str!("prompts/todo_reminder.md");

/// Sent along when the model ends its turn with open items.
const FINISH: &str = include_str!("prompts/todo_finish.md");

/// Added to the summary of a compaction that hid the list.
const RESTATED: &str = include_str!("prompts/todo_restated.md");

/// One open item of the list: what it is and how far along, as the model wrote them.
#[derive(Debug, Clone, PartialEq)]
struct Item {
    /// What the task is.
    content: String,
    /// Whether it is in progress rather than pending.
    doing: bool,
}

/// What a turn has seen of the todo list and of the work done since it last changed.
#[derive(Debug, Default)]
pub(crate) struct TodoTracker {
    /// The open items of the latest list.
    open: Vec<Item>,
    /// How many file-changing calls succeeded since the list last changed or the model was last reminded.
    changes: u32,
    /// How many reminders were given since the list last changed.
    reminders: u32,
    /// Whether a file-changing call succeeded in this turn.
    worked: bool,
    /// Whether the model was already told not to end the turn with open items.
    nudged: bool,
}

impl TodoTracker {
    /// A tracker that starts from the latest list in `history`.
    pub(crate) fn new(history: &[Message]) -> Self {
        Self {
            open: latest(history, history.len())
                .map(|(_, items)| items)
                .unwrap_or_default(),
            ..Self::default()
        }
    }

    /// Takes in a batch: `calls` made in a reply whose text is `text`, and the `results` they gave. Returns the reminder to add to the results when the list still has open items after [`CHANGES`] file-changing calls since it changed, at most [`MAX_REMINDERS`] times between changes, and never when `text` asks the user a question.
    pub(crate) fn observe(
        &mut self,
        calls: &[ToolCall],
        results: &[ToolResult],
        text: &str,
    ) -> Option<String> {
        for call in calls {
            let succeeded = results
                .iter()
                .any(|result| result.call_id == call.id && !result.is_error);
            if !succeeded {
                continue;
            }
            if call.name == TODO {
                self.open = open_items(&call.arguments);
                self.changes = 0;
                self.reminders = 0;
            } else if MUTATING.contains(&call.name.as_str()) {
                self.changes += 1;
                self.worked = true;
            }
        }
        if self.open.is_empty()
            || self.changes < CHANGES
            || self.reminders >= MAX_REMINDERS
            || asks_question(text)
        {
            return None;
        }
        let changes = self.changes;
        self.reminders += 1;
        self.changes = 0;
        Some(fill(REMINDER, &self.open).replace("{changes}", &changes.to_string()))
    }

    /// The notice to send along when the turn is about to end with `reply`: once a turn, when files were changed in it, the list still has open items, and `reply` does not ask the user a question.
    pub(crate) fn finishing(&mut self, reply: &Reply) -> Option<String> {
        if self.nudged || !self.worked || self.open.is_empty() || asks_question(&reply.text) {
            return None;
        }
        self.nudged = true;
        Some(fill(FINISH, &self.open))
    }
}

/// The open items of the latest list a successful `todo` call before position `at` of `history` wrote, with the position of that call, when there is one.
fn latest(history: &[Message], at: usize) -> Option<(usize, Vec<Item>)> {
    let before = &history[..at.min(history.len())];
    let failed: HashMap<&str, bool> = before
        .iter()
        .filter_map(|message| match message {
            Message::ToolResults(results) => Some(results),
            _ => None,
        })
        .flatten()
        .filter(|result| result.name == TODO)
        .map(|result| (result.call_id.as_str(), result.is_error))
        .collect();
    before
        .iter()
        .enumerate()
        .rev()
        .filter_map(|(position, message)| match message {
            Message::Assistant(reply) => Some((position, reply)),
            _ => None,
        })
        .find_map(|(position, reply)| {
            reply
                .tool_calls
                .iter()
                .rev()
                .find(|call| call.name == TODO && failed.get(call.id.as_str()) == Some(&false))
                .map(|call| (position, open_items(&call.arguments)))
        })
}

/// The text that restates the open items to the model after the compaction at position `at` of `history`, whose first kept message is at `first_kept`: only when the latest list before it was written before `first_kept`, where the model no longer sees it, and has open items.
pub(crate) fn restated(history: &[Message], at: usize, first_kept: usize) -> Option<String> {
    let (position, items) = latest(history, at)?;
    (position < first_kept && !items.is_empty()).then(|| fill(RESTATED, &items))
}

/// The items of the list `arguments` of a `todo` call lay out that are neither completed nor cancelled.
fn open_items(arguments: &Value) -> Vec<Item> {
    arguments["todos"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let content = item["content"].as_str()?.trim();
            let status = item["status"].as_str().unwrap_or_default();
            let closed = matches!(
                status,
                "completed" | "done" | "complete" | "cancelled" | "canceled"
            );
            (!content.is_empty() && !closed).then(|| Item {
                content: content.to_owned(),
                doing: matches!(status, "in_progress" | "doing" | "active" | "started"),
            })
        })
        .collect()
}

/// `template` with `{count}` replaced by how many of `items` there are and `{items}` by a list of them, trimmed.
fn fill(template: &str, items: &[Item]) -> String {
    let list: Vec<String> = items
        .iter()
        .map(|item| {
            let status = if item.doing { "in progress" } else { "pending" };
            format!("- [{status}] {}", item.content)
        })
        .collect();
    template
        .trim()
        .replace("{count}", &items.len().to_string())
        .replace("{items}", &list.join("\n"))
}

/// Whether `text` ends by asking the user something: its last line ends with a question mark, or opens with a phrase that waits for an answer.
pub(crate) fn asks_question(text: &str) -> bool {
    let Some(last) = text.lines().map(str::trim).rfind(|line| !line.is_empty()) else {
        return false;
    };
    let bare = last.trim_end_matches(['*', '_', '~', '`', '"', '\'', ')', ']', ' ']);
    if bare.ends_with('?') || bare.ends_with('？') {
        return true;
    }
    let lower = last
        .trim_start_matches(['>', '-', '*', '_', ' '])
        .to_lowercase();
    [
        "let me know",
        "please confirm",
        "please let me know",
        "tell me which",
        "tell me whether",
        "tell me if",
    ]
    .iter()
    .any(|cue| lower.starts_with(cue))
}

#[cfg(test)]
/// Tests for tracking the todo list.
mod tests {
    use serde_json::json;

    use super::*;

    /// A call of `name` with `arguments` under the id `id`.
    fn call(id: &str, name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        }
    }

    /// A result of the call `id` to `name`.
    fn result(id: &str, name: &str, is_error: bool) -> ToolResult {
        ToolResult {
            call_id: id.into(),
            name: name.into(),
            text: "ok".into(),
            is_error,
            images: Vec::new(),
        }
    }

    /// A list with one item done, one in progress and one pending.
    fn list() -> Value {
        json!({"todos": [
            {"content": "read", "status": "completed"},
            {"content": "fix", "status": "in_progress"},
            {"content": "test", "status": "pending"}
        ]})
    }

    /// Runs `count` batches of one successful edit through `tracker`, with replies saying `text`, and returns the reminders given.
    fn edits(tracker: &mut TodoTracker, count: u32, text: &str) -> Vec<String> {
        (0..count)
            .filter_map(|round| {
                let id = format!("e{round}");
                tracker.observe(
                    &[call(&id, "edit", json!({}))],
                    &[result(&id, "edit", false)],
                    text,
                )
            })
            .collect()
    }

    /// Open items earn a reminder after enough edits, at most twice, and a new list starts the count again.
    #[test]
    fn reminds_after_many_edits() {
        let mut tracker = TodoTracker::default();
        assert!(edits(&mut tracker, CHANGES * 2, "").is_empty(), "no list");
        let todo = [call("t", TODO, list())];
        assert_eq!(
            tracker.observe(&todo, &[result("t", TODO, false)], ""),
            None
        );
        assert!(edits(&mut tracker, CHANGES - 1, "").is_empty());
        let first = edits(&mut tracker, 1, "");
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("2 open item(s)"), "{}", first[0]);
        assert!(first[0].contains("- [in progress] fix\n- [pending] test"));
        assert!(!first[0].contains("read"));
        assert_eq!(edits(&mut tracker, CHANGES * 3, "").len(), 1, "two at most");
        tracker.observe(&todo, &[result("t", TODO, false)], "");
        assert_eq!(edits(&mut tracker, CHANGES, "").len(), 1);
    }

    /// A reply that asks the user something is not answered with a reminder, and failed calls do not count.
    #[test]
    fn questions_and_failures_are_left_alone() {
        let mut tracker = TodoTracker::default();
        tracker.observe(&[call("t", TODO, list())], &[result("t", TODO, false)], "");
        assert!(edits(&mut tracker, CHANGES, "Shall I go on?").is_empty());
        let mut failing = TodoTracker::default();
        failing.observe(&[call("t", TODO, list())], &[result("t", TODO, false)], "");
        for round in 0..CHANGES {
            let id = format!("e{round}");
            assert!(
                failing
                    .observe(
                        &[call(&id, "edit", json!({}))],
                        &[result(&id, "edit", true)],
                        ""
                    )
                    .is_none()
            );
        }
    }

    /// The list is read back from the history, skipping a call that failed, and restated after a compaction that hid it.
    #[test]
    fn reads_the_list_from_the_history() {
        let reply = |id: &str, arguments: Value| {
            Message::Assistant(Reply {
                tool_calls: vec![call(id, TODO, arguments)],
                ..Reply::default()
            })
        };
        let history = vec![
            Message::User("go".into()),
            reply("a", list()),
            Message::ToolResults(vec![result("a", TODO, false)]),
            reply(
                "b",
                json!({"todos": [{"content": "x", "status": "pending"}]}),
            ),
            Message::ToolResults(vec![result("b", TODO, true)]),
        ];
        let tracker = TodoTracker::new(&history);
        assert_eq!(tracker.open.len(), 2);
        assert_eq!(tracker.open[0].content, "fix");
        let restated = restated(&history, history.len(), 3).unwrap();
        assert!(restated.contains("- [pending] test"), "{restated}");
        assert_eq!(super::restated(&history, history.len(), 1), None);
    }

    /// The turn may end once with open items before being nudged, never after a question, and not when nothing was changed.
    #[test]
    fn nudges_once_before_the_turn_ends() {
        let mut tracker = TodoTracker::default();
        tracker.observe(&[call("t", TODO, list())], &[result("t", TODO, false)], "");
        let done = Reply {
            text: "All done.".into(),
            ..Reply::default()
        };
        assert_eq!(tracker.finishing(&done), None, "nothing changed yet");
        edits(&mut tracker, 1, "");
        let asking = Reply {
            text: "Which file next?".into(),
            ..Reply::default()
        };
        assert_eq!(tracker.finishing(&asking), None);
        assert!(tracker.finishing(&done).is_some());
        assert_eq!(tracker.finishing(&done), None);
    }

    /// Questions are told by their last line.
    #[test]
    fn tells_questions() {
        assert!(asks_question("Done.\n\n**Should I also update the docs?**"));
        assert!(asks_question("Let me know which one you prefer."));
        assert!(!asks_question("Is it fixed? Yes.\nAll tests pass."));
        assert!(!asks_question(""));
    }
}
