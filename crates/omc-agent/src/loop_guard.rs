//! Noticing when the model goes round in circles within a turn, and telling it to change course.

use std::collections::HashMap;

use omc_providers::{ToolCall, ToolResult};
use omc_tools::FileChange;
use serde_json::Value;

/// How many times in a row the same calls, with the same arguments, are made before the model is told to stop repeating them.
pub(crate) const REPEATS: u32 = 5;

/// How many edits of one file that change nothing are made before the model is told to look again; at twice this, it is told to stop and explain.
pub(crate) const NO_OPS: u32 = 3;

/// Tools whose calls are expected to repeat, such as waiting on work running in the background, and do not count.
const EXEMPT: [&str; 1] = ["wait"];

/// The longest a call's arguments are quoted in a notice, in characters.
const ARGUMENTS_SHOWN: usize = 400;

/// The longest a result is quoted in a notice, in characters.
const RESULT_SHOWN: usize = 200;

/// Told when the same calls keep being made.
const REPEATED_CALL: &str = include_str!("prompts/repeated_call.md");

/// Told when edits of a file keep changing nothing.
const NO_OP_EDIT: &str = include_str!("prompts/noop_edit.md");

/// Told when edits of a file still change nothing after the model was told so.
const NO_OP_EDIT_STOP: &str = include_str!("prompts/noop_edit_stop.md");

/// What has been seen of the batches of tool calls of one turn, so that circles can be noticed.
#[derive(Debug, Default)]
pub(crate) struct LoopGuard {
    /// The calls of the latest batch that counted, in a form that is the same for the same calls.
    last: Option<String>,
    /// How many batches in a row have made those calls.
    repeats: u32,
    /// How many edits in a row of each file, by the path it is shown under, have changed nothing.
    no_ops: HashMap<String, u32>,
}

impl LoopGuard {
    /// Takes in a batch: `calls` made, `results` they gave and `changes` they made to files, by call id. Returns what the model is to be told about circles the batch shows, when it shows any.
    pub(crate) fn observe(
        &mut self,
        calls: &[ToolCall],
        results: &[ToolResult],
        changes: &[(String, Vec<FileChange>)],
    ) -> Option<String> {
        let mut notices = Vec::new();
        notices.extend(self.repeated(calls, results));
        notices.extend(self.no_op_edits(results, changes));
        (!notices.is_empty()).then(|| notices.join("\n\n"))
    }

    /// Counts how many batches in a row made `calls`, and returns the notice for repeating them when that is [`REPEATS`] or more; a batch only of exempt calls breaks the run.
    fn repeated(&mut self, calls: &[ToolCall], results: &[ToolResult]) -> Option<String> {
        let counted: Vec<&ToolCall> = calls
            .iter()
            .filter(|call| !EXEMPT.contains(&call.name.as_str()))
            .collect();
        let Some(first) = counted.first() else {
            self.last = None;
            self.repeats = 0;
            return None;
        };
        let mut keys: Vec<String> = counted
            .iter()
            .map(|call| format!("{}\u{0}{}", call.name, canonical(&call.arguments)))
            .collect();
        keys.sort();
        let key = keys.join("\u{1}");
        if self.last.as_deref() == Some(key.as_str()) {
            self.repeats += 1;
        } else {
            self.last = Some(key);
            self.repeats = 1;
        }
        if self.repeats < REPEATS {
            return None;
        }
        let result = results
            .iter()
            .find(|result| result.call_id == first.id)
            .map(|result| shortened(&result.text, RESULT_SHOWN))
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "(no text)".to_owned());
        Some(
            REPEATED_CALL
                .trim()
                .replace("{tool}", &first.name)
                .replace("{count}", &self.repeats.to_string())
                .replace(
                    "{arguments}",
                    &shortened(&canonical(&first.arguments).to_string(), ARGUMENTS_SHOWN),
                )
                .replace("{result}", &result),
        )
    }

    /// Counts, for each file a successful call in `results` reported in `changes`, how many edits in a row changed nothing, and returns the notices for the files where that reached [`NO_OPS`]: to look again, and from twice that on, to stop and explain.
    fn no_op_edits(
        &mut self,
        results: &[ToolResult],
        changes: &[(String, Vec<FileChange>)],
    ) -> Vec<String> {
        let mut notices = Vec::new();
        for (call, files) in changes {
            let succeeded = results
                .iter()
                .any(|result| result.call_id == *call && !result.is_error);
            if !succeeded {
                continue;
            }
            for file in files {
                if file.old != file.new {
                    self.no_ops.remove(&file.path);
                    continue;
                }
                let count = self.no_ops.entry(file.path.clone()).or_default();
                *count += 1;
                let template = match *count {
                    count if count >= NO_OPS * 2 => NO_OP_EDIT_STOP,
                    count if count >= NO_OPS => NO_OP_EDIT,
                    _ => continue,
                };
                notices.push(
                    template
                        .trim()
                        .replace("{path}", &file.path)
                        .replace("{count}", &count.to_string()),
                );
            }
        }
        notices
    }
}

/// `value` with the keys of every object in sorted order, so that the same arguments written in another order compare equal.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            Value::Object(
                keys.into_iter()
                    .map(|key| (key.clone(), canonical(&map[key])))
                    .collect(),
            )
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

/// `text` on one line, with runs of white space made single spaces, cut to at most `limit` characters with an ellipsis when it is longer.
fn shortened(text: &str, limit: usize) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit).collect();
    cut.push('…');
    cut
}

#[cfg(test)]
/// Tests for noticing circles.
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

    /// A result for the call `id` saying `text`.
    fn result(id: &str, text: &str, is_error: bool) -> ToolResult {
        ToolResult {
            call_id: id.into(),
            name: "x".into(),
            text: text.into(),
            is_error,
            images: Vec::new(),
        }
    }

    /// The same call made [`REPEATS`] times in a row is noticed, with its arguments in any key order, and another call starts the count again.
    #[test]
    fn notices_a_call_repeated_in_a_row() {
        let mut guard = LoopGuard::default();
        let results = [result("c", "no matches found", false)];
        for round in 1..REPEATS {
            let arguments = if round % 2 == 0 {
                json!({ "pattern": "x", "path": "src" })
            } else {
                json!({ "path": "src", "pattern": "x" })
            };
            assert_eq!(
                guard.observe(&[call("c", "grep", arguments)], &results, &[]),
                None
            );
        }
        let notice = guard
            .observe(
                &[call("c", "grep", json!({ "path": "src", "pattern": "x" }))],
                &results,
                &[],
            )
            .unwrap();
        assert!(notice.contains("`grep`"), "{notice}");
        assert!(notice.contains(&format!("{REPEATS} times")), "{notice}");
        assert!(notice.contains("no matches found"), "{notice}");
        assert!(
            guard
                .observe(
                    &[call("c", "grep", json!({ "path": "lib" }))],
                    &results,
                    &[]
                )
                .is_none()
        );
    }

    /// Waiting calls do not count and break a run of repeats.
    #[test]
    fn waiting_breaks_a_run() {
        let mut guard = LoopGuard::default();
        let read = || [call("c", "read", json!({ "path": "a" }))];
        for _ in 1..REPEATS {
            assert!(guard.observe(&read(), &[], &[]).is_none());
        }
        assert!(
            guard
                .observe(&[call("w", "wait", json!({}))], &[], &[])
                .is_none()
        );
        assert!(guard.observe(&read(), &[], &[]).is_none());
    }

    /// Edits of a file that change nothing are noticed from [`NO_OPS`] on, more firmly from twice that, and a real change starts the count again.
    #[test]
    fn notices_edits_that_change_nothing() {
        let mut guard = LoopGuard::default();
        let same = |id: &str| {
            vec![(
                id.to_owned(),
                vec![FileChange {
                    path: "a.rs".into(),
                    old: "x\n".into(),
                    new: "x\n".into(),
                }],
            )]
        };
        let mut notices = Vec::new();
        for round in 0..NO_OPS * 2 {
            let id = format!("e{round}");
            let calls = [call(&id, "edit", json!({ "round": round }))];
            notices.push(guard.observe(&calls, &[result(&id, "ok", false)], &same(&id)));
        }
        assert!(notices[..NO_OPS as usize - 1].iter().all(Option::is_none));
        let first = notices[NO_OPS as usize - 1].as_deref().unwrap();
        assert!(
            first.contains("`a.rs`") && first.contains("Read the file again"),
            "{first}"
        );
        let last = notices.last().unwrap().as_deref().unwrap();
        assert!(last.contains("Stop editing this file"), "{last}");
        let changed = vec![(
            "real".to_owned(),
            vec![FileChange {
                path: "a.rs".into(),
                old: "x\n".into(),
                new: "y\n".into(),
            }],
        )];
        guard.observe(&[], &[result("real", "ok", false)], &changed);
        assert!(
            guard
                .observe(&[], &[result("e", "ok", false)], &same("e"))
                .is_none()
        );
    }

    /// A failed edit is not counted as one that changed nothing.
    #[test]
    fn failed_edits_do_not_count() {
        let mut guard = LoopGuard::default();
        let same = vec![(
            "e".to_owned(),
            vec![FileChange {
                path: "a.rs".into(),
                old: "x".into(),
                new: "x".into(),
            }],
        )];
        for _ in 0..NO_OPS * 2 {
            assert!(
                guard
                    .observe(&[], &[result("e", "failed", true)], &same)
                    .is_none()
            );
        }
    }
}
