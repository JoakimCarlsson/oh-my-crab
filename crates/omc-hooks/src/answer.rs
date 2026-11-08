//! What the hooks of one event answered, read the way Claude Code reads them, and what that comes to.
//!
//! Exit code 0 is an answer: standard output holding a JSON object is read for its fields, and other text is context for a prompt and ignored elsewhere. Exit code 2 blocks, with standard error as the reason. Any other exit code is a failure that lets things go on. A hook that timed out or could not start gave no answer: before a tool call that denies the call, since the check never happened, and everywhere else it lets things go on.

use omc_agent::{Batched, Compacting, Followed, Gate, Permit, Screened};
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
                    Ok(json) if json.is_object() => {
                        self.tell_reader(&json);
                        Said::Json(json)
                    }
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

    /// Writes the `systemMessage` of `json`, which the hook meant for the reader, to standard error, which the client shows the reader; crab has no other way to show it.
    fn tell_reader(&self, json: &Value) {
        if let Some(message) = text(json, "systemMessage") {
            eprintln!(
                "hook message: {} ({}): {message}",
                self.hook.label(),
                self.hook.file.display()
            );
        }
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

/// Why `json` asks for everything to stop, with `continue` set to `false`: its `stopReason`, or a reason naming `hook`; nothing when it does not ask.
fn halt_of(json: &Value, hook: &Hook) -> Option<String> {
    halts(json).then(|| or_default(text(json, "stopReason").unwrap_or_default(), hook))
}

/// What the hooks before a tool call came to: denied when any denied, blocked or gave no answer, with every reason; the reader asked when one asked and none denied; allowed when one allowed and none denied or asked; the arguments the last hook that rewrote them gave; the notes they added, in order; and, when one said `continue: false`, the turn stopping after the batch, its call denied.
pub(crate) fn before_tool(given: &[Given<'_>]) -> Screened {
    let mut denials = Vec::new();
    let mut allowed = false;
    let mut asked = false;
    let mut halt = None;
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
                if let Some(stopped) = halt_of(&json, one.hook) {
                    denials.push(stopped.clone());
                    halt = halt.or(Some(stopped));
                    continue;
                }
                match decision.as_deref() {
                    Some("deny") => denials.push(or_default(reason, one.hook)),
                    Some("ask") => asked = true,
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
    } else if asked {
        Gate::Ask
    } else if allowed {
        Gate::Allow
    } else {
        Gate::Pass
    };
    Screened {
        gate,
        arguments,
        note: joined(notes),
        halt,
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

/// What the hooks after a tool call came to: the note they add to its result, the reasons of those that blocked, by exit code 2 or `decision: "block"`, and the context they added, in order; the result the last hook that replaced it gave, with `updatedToolOutput` or `updatedMCPToolOutput`, text as it is and other values as JSON; and, when one said `continue: false`, the turn stopping after the batch.
pub(crate) fn after_tool(given: &[Given<'_>]) -> Followed {
    let mut notes = Vec::new();
    let mut output = None;
    let mut halt = None;
    for one in given {
        match one.said() {
            Said::Blocked(reason) => notes.extend(Some(reason).filter(|r| !r.is_empty())),
            Said::Json(json) => {
                if json.get("decision").and_then(Value::as_str) == Some("block") {
                    notes.extend(text(&json, "reason"));
                }
                notes.extend(context(&json));
                let specific = json.get("hookSpecificOutput");
                if let Some(replaced) = specific
                    .and_then(|specific| {
                        specific
                            .get("updatedToolOutput")
                            .or_else(|| specific.get("updatedMCPToolOutput"))
                    })
                    .filter(|replaced| !replaced.is_null())
                {
                    output = Some(match replaced {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    });
                }
                halt = halt.or_else(|| halt_of(&json, one.hook));
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    Followed {
        note: joined(notes),
        output,
        halt,
    }
}

/// What the hooks of a call the reader is about to be asked about came to, from the `decision` of their `hookSpecificOutput`: denied when any said `behavior: "deny"`, with every `message`, stopping the turn when one set `interrupt`; otherwise allowed when one said `"allow"`, with the arguments the last `updatedInput` gave and the allow rules of the `addRules` entries of `updatedPermissions`; otherwise the reader is asked. Exit code 2 is not honoured, as in Claude Code, and a hook that gave no answer leaves the question to the reader.
pub(crate) fn permission(given: &[Given<'_>]) -> Permit {
    let mut denials = Vec::new();
    let mut interrupt = false;
    let mut allowed = false;
    let mut arguments = None;
    let mut rules = Vec::new();
    for one in given {
        let Said::Json(json) = one.said() else {
            continue;
        };
        let decision = json
            .get("hookSpecificOutput")
            .and_then(|specific| specific.get("decision"))
            .cloned()
            .unwrap_or_default();
        match text(&decision, "behavior").as_deref() {
            Some("deny") => {
                denials.push(or_default(
                    text(&decision, "message").unwrap_or_default(),
                    one.hook,
                ));
                interrupt |= decision.get("interrupt") == Some(&Value::Bool(true));
            }
            Some("allow") => {
                allowed = true;
                if let Some(updated) = decision.get("updatedInput").filter(|v| v.is_object()) {
                    arguments = Some(updated.clone());
                }
                rules.extend(allow_rules(&decision));
            }
            _ => {}
        }
    }
    if !denials.is_empty() {
        Permit::Deny {
            reason: denials.join("\n"),
            halt: interrupt,
        }
    } else if allowed {
        Permit::Allow { arguments, rules }
    } else {
        Permit::Pass
    }
}

/// The allow rules, in Claude Code's syntax, that the `addRules` entries with `behavior: "allow"` of the `updatedPermissions` of `decision` add; crab keeps them for the conversation only, whatever `destination` says.
fn allow_rules(decision: &Value) -> Vec<String> {
    let mut rules = Vec::new();
    for entry in decision
        .get("updatedPermissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if text(entry, "type").as_deref() != Some("addRules")
            || text(entry, "behavior").as_deref() != Some("allow")
        {
            continue;
        }
        for rule in entry
            .get("rules")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(tool) = text(rule, "toolName") else {
                continue;
            };
            rules.push(match text(rule, "ruleContent") {
                Some(content) => format!("{tool}({content})"),
                None => tool,
            });
        }
    }
    rules
}

/// What the hooks after a batch of tool calls came to: the context they added, in order, as a note after the results; and the turn stopping now when one blocked, by exit code 2, `decision: "block"` or `continue: false`, with every reason.
pub(crate) fn batch(given: &[Given<'_>]) -> Batched {
    let mut notes = Vec::new();
    let mut stops = Vec::new();
    for one in given {
        match one.said() {
            Said::Blocked(reason) => stops.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                if let Some(stopped) = halt_of(&json, one.hook) {
                    stops.push(stopped);
                } else if json.get("decision").and_then(Value::as_str) == Some("block") {
                    stops.push(or_default(
                        text(&json, "reason").unwrap_or_default(),
                        one.hook,
                    ));
                }
                notes.extend(context(&json));
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    Batched {
        note: joined(notes),
        halt: joined(stops),
    }
}

/// Why the hooks before a model switch keep it from happening: exit code 2, `decision: "block"`, a `permissionDecision` of `deny`, or of `ask`, which crab cannot put to the reader and so counts as a refusal, and a hook that timed out or could not start; nothing when the switch goes ahead.
pub(crate) fn model_switch(given: &[Given<'_>]) -> Option<String> {
    let mut blocks = Vec::new();
    for one in given {
        if let Some(silence) = one.silence() {
            blocks.push(format!(
                "the hook `{}` gave no answer: {silence}",
                one.hook.command
            ));
            continue;
        }
        match one.said() {
            Said::Blocked(reason) => blocks.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                let specific = json.get("hookSpecificOutput").cloned().unwrap_or_default();
                let refused = matches!(
                    text(&specific, "permissionDecision").as_deref(),
                    Some("deny" | "ask")
                ) || json.get("decision").and_then(Value::as_str) == Some("block");
                if refused {
                    blocks.push(or_default(
                        text(&specific, "permissionDecisionReason")
                            .or_else(|| text(&json, "reason"))
                            .unwrap_or_default(),
                        one.hook,
                    ));
                }
            }
            Said::Text(_) | Said::Nothing => {}
        }
    }
    joined(blocks)
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

/// Reports on standard error what a hook that cannot block wrote to standard error when it exited 2, since the reader is shown it and the model is not.
fn shown_to_reader(one: &Given<'_>, said: &str) {
    if !said.is_empty() {
        eprintln!(
            "hook error: {} ({}): {said}",
            one.hook.label(),
            one.hook.file.display()
        );
    }
}

/// The context the hooks of a conversation or a subagent starting add: each one's `additionalContext`, and, when `plain` is set, as for `SessionStart`, the plain text it printed, in order; nothing when there is none. These hooks cannot block: exit code 2 shows standard error to the reader only.
pub(crate) fn started(given: &[Given<'_>], plain: bool) -> Option<String> {
    let mut added = Vec::new();
    for one in given {
        match one.said() {
            Said::Json(json) => added.extend(context(&json)),
            Said::Text(text) if plain && !text.is_empty() => added.push(text),
            Said::Blocked(said) => shown_to_reader(one, &said),
            Said::Text(_) | Said::Nothing => {}
        }
    }
    joined(added)
}

/// What the hooks before a compaction came to: blocked when any blocked, by exit code 2 or `decision: "block"`; otherwise going on, with the plain text they printed as instructions for the summary, in order.
pub(crate) fn compacting(given: &[Given<'_>]) -> Compacting {
    let mut blocks = Vec::new();
    let mut instructions = Vec::new();
    for one in given {
        match one.said() {
            Said::Blocked(reason) => blocks.push(or_default(reason, one.hook)),
            Said::Json(json) => {
                if json.get("decision").and_then(Value::as_str) == Some("block") {
                    blocks.push(or_default(
                        text(&json, "reason").unwrap_or_default(),
                        one.hook,
                    ));
                }
            }
            Said::Text(text) if !text.is_empty() => instructions.push(text),
            Said::Text(_) | Said::Nothing => {}
        }
    }
    if !blocks.is_empty() {
        return Compacting::Blocked(blocks.join("\n"));
    }
    Compacting::Go(joined(instructions))
}

/// Reads what the hooks of an event that has no decision to make answered, such as `SessionEnd` and `Notification`, only to report on standard error the ones that failed or wrote to standard error with exit code 2.
pub(crate) fn heard(given: &[Given<'_>]) {
    for one in given {
        if let Said::Blocked(said) = one.said() {
            shown_to_reader(one, &said);
        }
    }
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
            .note
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

    /// A conversation starting: plain text and `additionalContext` are added when plain text counts, only `additionalContext` otherwise, and exit code 2 adds nothing.
    #[test]
    fn a_start() {
        let check = hook(HookEvent::SessionStart, "check");
        let one = |outcome, plain| {
            started(
                &[Given {
                    hook: &check,
                    outcome,
                }],
                plain,
            )
        };
        assert_eq!(
            one(exited(0, "the secret is kiwi\n", ""), true),
            Some("the secret is kiwi".into())
        );
        assert_eq!(one(exited(0, "the secret is kiwi\n", ""), false), None);
        let json = r#"{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"on main"}}"#;
        assert_eq!(one(exited(0, json, ""), false), Some("on main".into()));
        assert_eq!(one(exited(2, "", "nope"), true), None);
        assert_eq!(one(Outcome::TimedOut(Duration::from_secs(1)), true), None);
    }

    /// Before a compaction: exit code 2 and `decision: "block"` block it, plain text becomes instructions, and a failure lets it go on.
    #[test]
    fn before_a_compaction() {
        let check = hook(HookEvent::PreCompact, "check");
        let one = |outcome| {
            compacting(&[Given {
                hook: &check,
                outcome,
            }])
        };
        assert_eq!(
            one(exited(2, "", "not now")),
            Compacting::Blocked("not now".into())
        );
        assert_eq!(
            one(exited(0, r#"{"decision":"block","reason":"saving"}"#, "")),
            Compacting::Blocked("saving".into())
        );
        assert_eq!(
            one(exited(0, "keep the API notes\n", "")),
            Compacting::Go(Some("keep the API notes".into()))
        );
        assert_eq!(one(exited(1, "", "boom")), Compacting::Go(None));
        assert_eq!(
            one(Outcome::TimedOut(Duration::from_secs(1))),
            Compacting::Go(None)
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

    /// One hook for `event` that ended as `outcome`, read by `read`.
    fn alone<T>(event: HookEvent, outcome: Outcome, read: fn(&[Given<'_>]) -> T) -> T {
        let check = hook(event, "check");
        read(&[Given {
            hook: &check,
            outcome,
        }])
    }

    /// `ask` has the reader asked, below a denial; `continue: false` before a call denies it and stops the turn; after a call `updatedToolOutput` replaces the result, as text or as JSON.
    #[test]
    fn asking_stopping_and_replacing() {
        let asked = r#"{"hookSpecificOutput":{"permissionDecision":"ask"}}"#;
        assert_eq!(
            alone(HookEvent::PreToolUse, exited(0, asked, ""), before_tool).gate,
            Gate::Ask
        );
        let stopped = alone(
            HookEvent::PreToolUse,
            exited(0, r#"{"continue":false,"stopReason":"build is red"}"#, ""),
            before_tool,
        );
        assert_eq!(stopped.gate, Gate::Deny("build is red".into()));
        assert_eq!(stopped.halt.as_deref(), Some("build is red"));
        let replaced = alone(
            HookEvent::PostToolUse,
            exited(
                0,
                r#"{"hookSpecificOutput":{"updatedToolOutput":"[redacted]"}}"#,
                "",
            ),
            after_tool,
        );
        assert_eq!(replaced.output.as_deref(), Some("[redacted]"));
        let structured = alone(
            HookEvent::PostToolUse,
            exited(
                0,
                r#"{"hookSpecificOutput":{"updatedMCPToolOutput":{"stdout":"x"}}}"#,
                "",
            ),
            after_tool,
        );
        assert_eq!(structured.output.as_deref(), Some(r#"{"stdout":"x"}"#));
    }

    /// A permission request: `allow` with a rewrite and rules, `deny` with a message and `interrupt`, and exit code 2 or silence leaving it to the reader.
    #[test]
    fn a_permission_request() {
        let allow = json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": {
            "behavior": "allow",
            "updatedInput": {"command": "npm run lint"},
            "updatedPermissions": [{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "npm run *"}, {"toolName": "Read"}], "behavior": "allow", "destination": "session"}]
        }}});
        assert_eq!(
            alone(
                HookEvent::PermissionRequest,
                exited(0, &allow.to_string(), ""),
                permission
            ),
            Permit::Allow {
                arguments: Some(json!({"command": "npm run lint"})),
                rules: vec!["Bash(npm run *)".into(), "Read".into()],
            }
        );
        let deny = r#"{"hookSpecificOutput":{"decision":{"behavior":"deny","message":"no deploys","interrupt":true}}}"#;
        assert_eq!(
            alone(
                HookEvent::PermissionRequest,
                exited(0, deny, ""),
                permission
            ),
            Permit::Deny {
                reason: "no deploys".into(),
                halt: true
            }
        );
        assert_eq!(
            alone(
                HookEvent::PermissionRequest,
                exited(2, "", "nope"),
                permission
            ),
            Permit::Pass
        );
        assert_eq!(
            alone(
                HookEvent::PermissionRequest,
                Outcome::TimedOut(Duration::from_secs(1)),
                permission
            ),
            Permit::Pass
        );
    }

    /// After a batch, context becomes a note and a block stops the turn; before a model switch, exit 2, `deny`, `ask` and silence keep it from happening.
    #[test]
    fn a_batch_and_a_model_switch() {
        let noted = alone(
            HookEvent::PostToolBatch,
            exited(
                0,
                r#"{"hookSpecificOutput":{"additionalContext":"run pytest"}}"#,
                "",
            ),
            batch,
        );
        assert_eq!(noted.note.as_deref(), Some("run pytest"));
        assert_eq!(noted.halt, None);
        let stopped = alone(
            HookEvent::PostToolBatch,
            exited(0, r#"{"decision":"block","reason":"too many edits"}"#, ""),
            batch,
        );
        assert_eq!(stopped.halt.as_deref(), Some("too many edits"));
        assert_eq!(
            alone(
                HookEvent::PreModelSwitch,
                exited(2, "", "stay put"),
                model_switch
            )
            .as_deref(),
            Some("stay put")
        );
        let ask = r#"{"hookSpecificOutput":{"permissionDecision":"ask","permissionDecisionReason":"costly"}}"#;
        assert_eq!(
            alone(HookEvent::PreModelSwitch, exited(0, ask, ""), model_switch).as_deref(),
            Some("costly")
        );
        assert!(
            alone(
                HookEvent::PreModelSwitch,
                Outcome::TimedOut(Duration::from_secs(1)),
                model_switch
            )
            .is_some()
        );
        assert_eq!(
            alone(
                HookEvent::PreModelSwitch,
                exited(0, r#"{"systemMessage":"switching"}"#, ""),
                model_switch
            ),
            None
        );
    }
}
