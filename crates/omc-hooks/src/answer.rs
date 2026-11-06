//! What the hooks of one event answered, read the way Claude Code reads them, and what that comes to.
//!
//! Exit code 0 is an answer: standard output holding a JSON object is read for its fields, and other text is context for a prompt and ignored elsewhere. Exit code 2 blocks, with standard error as the reason. Any other exit code is a failure that lets things go on. A hook that timed out or could not start gave no answer: before a tool call that denies the call, since the check never happened, and everywhere else it lets things go on.

use omc_agent::{Gate, Screened};
use serde_json::Value;

use crate::config::Hook;
use crate::run::Outcome;

/// What one hook gave back.
pub(crate) struct Given<'a> {
    /// The hook.
    pub(crate) hook: &'a Hook,
    /// How it ended.
    pub(crate) outcome: Outcome,
}

/// How one hook that exited answered.
enum Said {
    /// It exited 0, with this JSON object on standard output.
    Json(Value),
    /// It exited 0, with this text on standard output, which is not a JSON object.
    Text(String),
    /// It exited 2, blocking, with this on standard error.
    Blocked(String),
    /// It gave no answer: it failed, timed out or could not start.
    Nothing,
}

impl Given<'_> {
    /// What the hook said, reporting a hook that failed or gave no answer on standard error.
    fn said(&self) -> Said {
        match &self.outcome {
            Outcome::Exited {
                code: 0, stdout, ..
            } => match stdout.trim() {
                text if text.starts_with('{') => match serde_json::from_str::<Value>(text) {
                    Ok(json) if json.is_object() => Said::Json(json),
                    _ => Said::Text(text.to_owned()),
                },
                text => Said::Text(text.to_owned()),
            },
            Outcome::Exited {
                code: 2, stderr, ..
            } => Said::Blocked(stderr.trim().to_owned()),
            Outcome::Exited { code, stderr, .. } => {
                eprintln!(
                    "hook failed with exit code {code} and was ignored: {} ({}): {}",
                    self.hook.label(),
                    self.hook.file.display(),
                    stderr.trim()
                );
                Said::Nothing
            }
            other => {
                eprintln!(
                    "hook gave no answer: {} ({}): {}",
                    self.hook.label(),
                    self.hook.file.display(),
                    other.silence().unwrap_or_default()
                );
                Said::Nothing
            }
        }
    }

    /// Why the hook gave no answer, when it timed out or could not start.
    fn silence(&self) -> Option<String> {
        self.outcome.silence()
    }
}

/// The text of `key` in `json`, when it is non-empty text.
fn text(json: &Value, key: &str) -> Option<String> {
    json.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The `additionalContext` of the `hookSpecificOutput` of `json`, when there is some.
fn context(json: &Value) -> Option<String> {
    json.get("hookSpecificOutput")
        .and_then(|specific| text(specific, "additionalContext"))
}

/// Whether `json` asks for everything to stop, with `continue` set to `false`.
fn halts(json: &Value) -> bool {
    json.get("continue") == Some(&Value::Bool(false))
}

/// What the hooks before a tool call came to: denied when any denied, blocked or gave no answer, with every reason; allowed when one allowed and none denied; the arguments the last hook that rewrote them gave; and the notes they added, in order.
pub(crate) fn before_tool(given: &[Given<'_>]) -> Screened {
    let mut denials = Vec::new();
    let mut allowed = false;
    let mut arguments = None;
    let mut notes = Vec::new();
    for one in given {
        if let Some(silence) = one.silence() {
            denials.push(format!(
                "the hook `{}` gave no answer, so the call was not run: {silence}",
                one.hook.command
            ));
            continue;
        }
        match one.said() {
            Said::Blocked(reason) => denials.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                let specific = json.get("hookSpecificOutput").cloned().unwrap_or_default();
                let decision = text(&specific, "permissionDecision").or_else(|| {
                    text(&json, "decision").map(|legacy| match legacy.as_str() {
                        "approve" => "allow".to_owned(),
                        "block" => "deny".to_owned(),
                        other => other.to_owned(),
                    })
                });
                let reason = text(&specific, "permissionDecisionReason")
                    .or_else(|| text(&json, "reason"))
                    .unwrap_or_default();
                if halts(&json) {
                    denials.push(or_default(
                        text(&json, "stopReason").unwrap_or(reason),
                        one.hook,
                    ));
                    continue;
                }
                match decision.as_deref() {
                    Some("deny") => denials.push(or_default(reason, one.hook)),
                    Some("allow") => allowed = true,
                    _ => {}
                }
                if let Some(updated) = specific.get("updatedInput").filter(|v| v.is_object()) {
                    arguments = Some(updated.clone());
                }
                notes.extend(context(&json));
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    let gate = if !denials.is_empty() {
        Gate::Deny(denials.join("\n"))
    } else if allowed {
        Gate::Allow
    } else {
        Gate::Pass
    };
    Screened {
        gate,
        arguments,
        note: joined(notes),
    }
}

/// `reason`, or a reason naming `hook` when it gave none.
fn or_default(reason: String, hook: &Hook) -> String {
    if reason.trim().is_empty() {
        format!("the hook `{}` blocked it", hook.command)
    } else {
        reason
    }
}

/// `parts` joined by blank lines, or nothing when there are none.
fn joined(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The note the hooks after a tool call add to its result: the reasons of those that blocked, by exit code 2 or `decision: "block"`, and the context they added, in order; nothing when there is none.
pub(crate) fn after_tool(given: &[Given<'_>]) -> Option<String> {
    let mut notes = Vec::new();
    for one in given {
        match one.said() {
            Said::Blocked(reason) => notes.extend(Some(reason).filter(|r| !r.is_empty())),
            Said::Json(json) => {
                if json.get("decision").and_then(Value::as_str) == Some("block") {
                    notes.extend(text(&json, "reason"));
                }
                notes.extend(context(&json));
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    joined(notes)
}

/// What the hooks of a prompt came to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Submitted {
    /// The prompt goes to the model as it is.
    #[default]
    Go,
    /// The prompt goes to the model with this context added before it.
    WithContext(String),
    /// The prompt is not sent, for this reason.
    Blocked(String),
}

/// What the hooks of a prompt came to: blocked when any blocked, by exit code 2, `decision: "block"` or `continue: false`; otherwise the context they added, as JSON or as plain text, in order.
pub(crate) fn prompt(given: &[Given<'_>]) -> Submitted {
    let mut blocks = Vec::new();
    let mut added = Vec::new();
    for one in given {
        match one.said() {
            Said::Blocked(reason) => blocks.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                if halts(&json) {
                    blocks.push(or_default(
                        text(&json, "stopReason").unwrap_or_default(),
                        one.hook,
                    ));
                } else if json.get("decision").and_then(Value::as_str) == Some("block") {
                    blocks.push(or_default(
                        text(&json, "reason").unwrap_or_default(),
                        one.hook,
                    ));
                }
                added.extend(context(&json));
            }
            Said::Text(text) if !text.is_empty() => added.push(text),
            Said::Text(_) | Said::Nothing => {}
        }
    }
    if !blocks.is_empty() {
        return Submitted::Blocked(blocks.join("\n"));
    }
    joined(added).map_or(Submitted::Go, Submitted::WithContext)
}

/// Why the agent should go on instead of ending its turn, from the hooks at its end: the reasons of those that blocked, by exit code 2 or `decision: "block"`, and the context they added; nothing when none asked, or when one said `continue: false`, which always lets the turn end.
pub(crate) fn stop(given: &[Given<'_>]) -> Option<String> {
    let mut reasons = Vec::new();
    for one in given {
        match one.said() {
            Said::Blocked(reason) => reasons.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                if halts(&json) {
                    return None;
                }
                if json.get("decision").and_then(Value::as_str) == Some("block") {
                    reasons.push(or_default(
                        text(&json, "reason").unwrap_or_default(),
                        one.hook,
                    ));
                }
                reasons.extend(context(&json));
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    joined(reasons)
}

#[cfg(test)]
/// Tests for reading what hooks answered.
mod tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use serde_json::json;

    use super::*;
    use crate::config::{HookEvent, HookOrigin};

    /// A hook for `event` running `command`.
    fn hook(event: HookEvent, command: &str) -> Hook {
        Hook {
            event,
            matcher: String::new(),
            command: command.to_owned(),
            timeout: None,
            file: PathBuf::from("/h.json"),
            origin: HookOrigin::User,
            claude: false,
            plugin: None,
        }
    }

    /// An exit with `code` and `stdout` and `stderr`.
    fn exited(code: i32, stdout: &str, stderr: &str) -> Outcome {
        Outcome::Exited {
            code,
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    /// Before a tool call: exit 2 and `deny` block with their reasons, `allow` and the legacy `approve` allow, a later rewrite wins, failures pass and silence denies.
    #[test]
    fn before_a_call() {
        let guard = hook(HookEvent::PreToolUse, "guard");
        let one = |outcome| {
            before_tool(&[Given {
                hook: &guard,
                outcome,
            }])
        };
        assert_eq!(
            one(exited(2, "", "no rm please\n")).gate,
            Gate::Deny("no rm please".into())
        );
        let denied = json!({"hookSpecificOutput": {"hookEventName": "PreToolUse", "permissionDecision": "deny", "permissionDecisionReason": "writes are frozen"}});
        assert_eq!(
            one(exited(0, &denied.to_string(), "")).gate,
            Gate::Deny("writes are frozen".into())
        );
        assert_eq!(
            one(exited(0, r#"{"decision":"block"}"#, "")).gate,
            Gate::Deny("the hook `guard` blocked it".into())
        );
        assert_eq!(
            one(exited(0, r#"{"decision":"approve"}"#, "")).gate,
            Gate::Allow
        );
        assert_eq!(one(exited(1, "", "crashed")).gate, Gate::Pass);
        assert_eq!(one(exited(0, "plain words", "")).gate, Gate::Pass);
        let Gate::Deny(reason) = one(Outcome::TimedOut(Duration::from_secs(1))).gate else {
            panic!("a silent hook let the call through");
        };
        assert!(reason.contains("gave no answer"));
        let rewrite = |command: &str| {
            json!({"hookSpecificOutput": {"permissionDecision": "allow", "updatedInput": {"command": command}, "additionalContext": format!("rewrote to {command}")}}).to_string()
        };
        let both = before_tool(&[
            Given {
                hook: &guard,
                outcome: exited(0, &rewrite("ls -a"), ""),
            },
            Given {
                hook: &guard,
                outcome: exited(0, &rewrite("ls -l"), ""),
            },
        ]);
        assert_eq!(both.gate, Gate::Allow);
        assert_eq!(both.arguments, Some(json!({"command": "ls -l"})));
        assert_eq!(
            both.note.as_deref(),
            Some("rewrote to ls -a\n\nrewrote to ls -l")
        );
        let mixed = before_tool(&[
            Given {
                hook: &guard,
                outcome: exited(0, r#"{"decision":"approve"}"#, ""),
            },
            Given {
                hook: &guard,
                outcome: exited(2, "", "no"),
            },
        ]);
        assert_eq!(mixed.gate, Gate::Deny("no".into()));
    }

    /// After a tool call: blocks and context become the note; failures and plain text add nothing.
    #[test]
    fn after_a_call() {
        let check = hook(HookEvent::PostToolUse, "check");
        let note = |outcome| {
            after_tool(&[Given {
                hook: &check,
                outcome,
            }])
        };
        assert_eq!(
            note(exited(
                0,
                r#"{"decision":"block","reason":"lint failed"}"#,
                ""
            )),
            Some("lint failed".into())
        );
        assert_eq!(
            note(exited(
                0,
                r#"{"hookSpecificOutput":{"additionalContext":"3 tests ran"}}"#,
                ""
            )),
            Some("3 tests ran".into())
        );
        assert_eq!(
            note(exited(2, "", "formatting is off")),
            Some("formatting is off".into())
        );
        assert_eq!(note(exited(0, "fine", "")), None);
        assert_eq!(note(exited(7, "", "boom")), None);
        assert_eq!(note(Outcome::Unstarted("gone".into())), None);
    }

    /// A prompt: plain text and context are added, any block wins, and a hook that fails lets the prompt through.
    #[test]
    fn a_prompt() {
        let check = hook(HookEvent::UserPromptSubmit, "check");
        let given = |outcome| {
            prompt(&[Given {
                hook: &check,
                outcome,
            }])
        };
        assert_eq!(
            given(exited(0, "branch: main\n", "")),
            Submitted::WithContext("branch: main".into())
        );
        assert_eq!(
            given(exited(
                0,
                r#"{"decision":"block","reason":"no secrets"}"#,
                ""
            )),
            Submitted::Blocked("no secrets".into())
        );
        assert_eq!(
            given(exited(2, "", "not now")),
            Submitted::Blocked("not now".into())
        );
        assert_eq!(given(exited(1, "", "x")), Submitted::Go);
        assert_eq!(
            given(Outcome::TimedOut(Duration::from_secs(30))),
            Submitted::Go
        );
    }

    /// The end of a turn: a block asks to go on with its reason, `continue: false` always lets it end, and failures let it end.
    #[test]
    fn the_end_of_a_turn() {
        let check = hook(HookEvent::Stop, "check");
        let one = |outcome| {
            stop(&[Given {
                hook: &check,
                outcome,
            }])
        };
        assert_eq!(
            one(exited(
                0,
                r#"{"decision":"block","reason":"run the tests"}"#,
                ""
            )),
            Some("run the tests".into())
        );
        assert_eq!(one(exited(2, "", "not done")), Some("not done".into()));
        assert_eq!(one(exited(0, "", "")), None);
        assert_eq!(one(Outcome::TimedOut(Duration::from_secs(1))), None);
        let ended = stop(&[
            Given {
                hook: &check,
                outcome: exited(2, "", "not done"),
            },
            Given {
                hook: &check,
                outcome: exited(0, r#"{"continue":false}"#, ""),
            },
        ]);
        assert_eq!(ended, None);
    }
}
