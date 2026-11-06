//! Keeping the model's todo list current: reminders when it works on for long without updating a list that still has open items, and the open items restated after a compaction hides the list.
//!
//! The list is read from the history, from the latest `todo` call that succeeded, so it survives a reopened conversation.

use std::collections::HashMap;

use omc_providers::{Message, Reply, ToolCall, ToolResult};
use omc_tools::FileChange;
use serde_json::Value;

/// The tool the model keeps its list with.
const TODO: &str = "todo";

/// The tools that change a file whenever they succeed but report no record of the change; every other call counts as changing files only when its records show a file whose text changed.
const UNRECORDED: [&str; 1] = ["write"];

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
    /// Whether a `todo` call succeeded in this turn.
    listed: bool,
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

    /// Takes in a batch: `calls` made in a reply whose text is `text`, the `results` they gave, and the files each call changed, by call id, as `changed` records them. Returns the reminder to add to the results when the list still has open items after [`CHANGES`] file-changing calls since it changed, at most [`MAX_REMINDERS`] times between changes, and never when `text` asks the user a question.
    ///
    /// A call counts as changing files when it succeeded and its records show a file whose text changed, or when it is a call of one of [`UNRECORDED`]; a shell command that only looks, such as `ls` or a test run, does not count.
    pub(crate) fn observe(
        &mut self,
        calls: &[ToolCall],
        results: &[ToolResult],
        changed: &[(String, Vec<FileChange>)],
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
                self.listed = true;
            } else if changes_files(call, changed) {
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

    /// The notice to send along when the turn is about to end with `reply`: once a turn, when the list still has open items, it was written or files were changed in this turn, and `reply` does not ask the user a question. A list left over from an earlier turn does not hold up a turn that only answered a question.
    pub(crate) fn finishing(&mut self, reply: &Reply) -> Option<String> {
        if self.nudged
            || !(self.worked || self.listed)
            || self.open.is_empty()
            || asks_question(&reply.text)
        {
            return None;
        }
        self.nudged = true;
        Some(fill(FINISH, &self.open))
    }
}

/// Whether `call` changed files: it is a call of one of [`UNRECORDED`], or `changed` holds a record for it of a file whose text changed.
fn changes_files(call: &ToolCall, changed: &[(String, Vec<FileChange>)]) -> bool {
    UNRECORDED.contains(&call.name.as_str())
        || changed
            .iter()
            .filter(|(id, _)| *id == call.id)
            .flat_map(|(_, files)| files)
            .any(|file| file.old != file.new)
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

/// The text that restates the open items to the model after a compaction of `history` whose first kept message is at `first_kept`: only when the latest list in the whole history, after the compaction included, was written before `first_kept`, where the model no longer sees it, and has open items. A list written later is in the part the model sees, and stands in for the hidden one.
pub(crate) fn restated(history: &[Message], first_kept: usize) -> Option<String> {
    let (position, items) = latest(history, history.len())?;
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

    /// The records of the call `id` changing `a.rs` from `old` to `new`.
    fn changed(id: &str, old: &str, new: &str) -> Vec<(String, Vec<FileChange>)> {
        vec![(
            id.to_owned(),
            vec![FileChange {
                path: "a.rs".into(),
                old: old.into(),
                new: new.into(),
            }],
        )]
    }

    /// Writes the list `list()` through `tracker` in a batch of its own.
    fn listed(tracker: &mut TodoTracker) -> Option<String> {
        tracker.observe(
            &[call("t", TODO, list())],
            &[result("t", TODO, false)],
            &[],
            "",
        )
    }

    /// Runs `count` batches of one successful edit that changed a file through `tracker`, with replies saying `text`, and returns the reminders given.
    fn edits(tracker: &mut TodoTracker, count: u32, text: &str) -> Vec<String> {
        (0..count)
            .filter_map(|round| {
                let id = format!("e{round}");
                tracker.observe(
                    &[call(&id, "edit", json!({}))],
                    &[result(&id, "edit", false)],
                    &changed(&id, "a", "b"),
                    text,
                )
            })
            .collect()
    }

    /// Runs `count` batches of one successful call of `name` that changed no file through `tracker`, and returns the reminders given.
    fn looks(tracker: &mut TodoTracker, name: &str, count: u32) -> Vec<String> {
        (0..count)
            .filter_map(|round| {
                let id = format!("l{round}");
                tracker.observe(
                    &[call(&id, name, json!({"command": "ls"}))],
                    &[result(&id, name, false)],
                    &[],
                    "",
                )
            })
            .collect()
    }

    /// Open items earn a reminder after enough edits, at most twice, and a new list starts the count again.
    #[test]
    fn reminds_after_many_edits() {
        let mut tracker = TodoTracker::default();
        assert!(edits(&mut tracker, CHANGES * 2, "").is_empty(), "no list");
        assert_eq!(listed(&mut tracker), None);
        assert!(edits(&mut tracker, CHANGES - 1, "").is_empty());
        let first = edits(&mut tracker, 1, "");
        assert_eq!(first.len(), 1);
        assert!(first[0].contains("2 open item(s)"), "{}", first[0]);
        assert!(first[0].contains("- [in progress] fix\n- [pending] test"));
        assert!(!first[0].contains("read"));
        assert_eq!(edits(&mut tracker, CHANGES * 3, "").len(), 1, "two at most");
        listed(&mut tracker);
        assert_eq!(edits(&mut tracker, CHANGES, "").len(), 1);
    }

    /// Only calls whose records show a file changed count: shell commands that only look, and edits that left the file as it was, do not; a write does.
    #[test]
    fn only_real_changes_count() {
        let mut tracker = TodoTracker::default();
        listed(&mut tracker);
        assert!(looks(&mut tracker, "bash", CHANGES * 2).is_empty());
        for round in 0..CHANGES {
            let id = format!("n{round}");
            let reminder = tracker.observe(
                &[call(&id, "edit", json!({}))],
                &[result(&id, "edit", false)],
                &changed(&id, "same", "same"),
                "",
            );
            assert_eq!(reminder, None);
        }
        assert_eq!(looks(&mut tracker, "write", CHANGES).len(), 1);
        let mut idle = TodoTracker::default();
        listed(&mut idle);
        let shelled = idle.observe(
            &[call("s", "bash", json!({"command": "sed -i s/a/b/ a.rs"}))],
            &[result("s", "bash", false)],
            &changed("s", "a", "b"),
            "",
        );
        assert_eq!(shelled, None);
        assert!(idle.worked, "a shell command with change records counts");
    }

    /// A reply that asks the user something is not answered with a reminder, and failed calls do not count.
    #[test]
    fn questions_and_failures_are_left_alone() {
        let mut tracker = TodoTracker::default();
        listed(&mut tracker);
        assert!(edits(&mut tracker, CHANGES, "Shall I go on?").is_empty());
        let mut failing = TodoTracker::default();
        listed(&mut failing);
        for round in 0..CHANGES {
            let id = format!("e{round}");
            assert!(
                failing
                    .observe(
                        &[call(&id, "edit", json!({}))],
                        &[result(&id, "edit", true)],
                        &changed(&id, "a", "b"),
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
        let restated = restated(&history, 3).unwrap();
        assert!(restated.contains("- [pending] test"), "{restated}");
        assert_eq!(super::restated(&history, 1), None);
    }

    /// A list written after the compaction, where the model sees it, stands in for the hidden one, which is not restated.
    #[test]
    fn a_newer_list_is_not_restated_over() {
        let history = vec![
            Message::User("go".into()),
            Message::Assistant(Reply {
                tool_calls: vec![call("a", TODO, list())],
                ..Reply::default()
            }),
            Message::ToolResults(vec![result("a", TODO, false)]),
            Message::User("next".into()),
            Message::Assistant(Reply {
                tool_calls: vec![call(
                    "b",
                    TODO,
                    json!({"todos": [{"content": "fix", "status": "completed"}]}),
                )],
                ..Reply::default()
            }),
            Message::ToolResults(vec![result("b", TODO, false)]),
        ];
        assert_eq!(restated(&history, 3), None);
        assert!(restated(&history[..3], 3).is_some());
    }

    /// The turn may end once with open items before being nudged when it wrote the list or changed files, never after a question.
    #[test]
    fn nudges_once_before_the_turn_ends() {
        let done = Reply {
            text: "All done.".into(),
            ..Reply::default()
        };
        let mut written = TodoTracker::default();
        listed(&mut written);
        assert!(written.finishing(&done).is_some(), "the list was written");
        let history = vec![
            Message::Assistant(Reply {
                tool_calls: vec![call("t", TODO, list())],
                ..Reply::default()
            }),
            Message::ToolResults(vec![result("t", TODO, false)]),
        ];
        let mut tracker = TodoTracker::new(&history);
        assert_eq!(tracker.finishing(&done), None, "nothing done this turn");
        looks(&mut tracker, "bash", 3);
        assert_eq!(tracker.finishing(&done), None, "only looked around");
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
