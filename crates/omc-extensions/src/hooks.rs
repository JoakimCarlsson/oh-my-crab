//! What extensions are told around the agent loop and the conversation, and what their answers come to.
//!
//! The extensions that want an event are asked one after another, in the order they are declared, so that each sees what the ones before it changed. An extension that gives no answer in time, or stops before it answers, is taken to have no opinion, except before a tool call, where the call is refused because its check never happened.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use omc_agent::{Batched, CompactTrigger, Compacting, Followed, Gate, Hooks, Permit, Screened};
use omc_providers::{Reply, ToolCall, ToolResult};
use serde_json::{Map, Value, json};

use crate::host::Extensions;
use crate::process::Failure;

/// What the extensions made of a prompt that is about to start a turn.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Prompted {
    /// The prompt goes to the model.
    #[default]
    Go,
    /// The prompt goes to the model, as `prompt` when an extension rewrote it, with `context` before it when an extension added some.
    Changed {
        /// The prompt in place of the reader's, when an extension rewrote it.
        prompt: Option<String>,
        /// Context told to the model before the prompt.
        context: Option<String>,
    },
    /// The prompt is not sent, for this reason.
    Blocked(String),
}

/// The extensions of a conversation, as the main agent or one subagent runs them.
#[derive(Clone)]
pub struct ExtensionHooks {
    /// The extensions.
    host: Arc<Extensions>,
    /// The subagent they run for, as its id and kind, or nothing for the main agent.
    agent: Option<(String, String)>,
}

/// The text of `key` in `answer`, when it is non-empty text.
fn text(answer: &Value, key: &str) -> Option<String> {
    answer
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Whether `answer` sets `key` to `true`.
fn flag(answer: &Value, key: &str) -> bool {
    answer.get(key) == Some(&Value::Bool(true))
}

/// `parts` joined by blank lines, or nothing when there are none.
fn joined(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// The fields about `call`, made with `arguments`, that an extension is told: the tool's name, its arguments and the call's id.
fn about_call(call: &ToolCall, arguments: &Value) -> Map<String, Value> {
    let mut about = Map::new();
    about.insert("tool_name".into(), json!(call.name));
    about.insert("tool_input".into(), arguments.clone());
    about.insert("tool_use_id".into(), json!(call.id));
    about
}

/// `value` as the fields of an object; nothing when it is not one.
fn fields(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(fields) => fields,
        _ => Map::new(),
    }
}

impl ExtensionHooks {
    /// The extensions of `host`, run for the main agent.
    pub fn new(host: Arc<Extensions>) -> Self {
        Self { host, agent: None }
    }

    /// Whether any extension wants `event`.
    pub fn wants(&self, event: &str) -> bool {
        self.host.wants(event)
    }

    /// `fields` with the subagent's id and kind added when these run for one.
    fn with_agent(&self, mut fields: Map<String, Value>) -> Map<String, Value> {
        if let Some((id, kind)) = &self.agent {
            fields.insert("agent_id".into(), json!(id));
            fields.insert("agent_type".into(), json!(kind));
        }
        fields
    }

    /// Asks each extension that wants `event` in turn, with the fields `fields` makes from what the ones before it answered, and hands each answer to `take`, which says whether to go on to the next; an answer that is not an object, or no answer, is given as it is.
    async fn ask_each(
        &self,
        event: &str,
        mut fields: impl FnMut() -> Map<String, Value>,
        mut take: impl FnMut(&str, Result<Value, Failure>) -> bool,
    ) {
        for one in self.host.wanting(event) {
            let payload = self.host.payload(event, self.with_agent(fields()));
            let answer = one.ask(&self.host.setting, payload).await;
            if !take(&one.spec.name, answer) {
                break;
            }
        }
    }

    /// Tells every extension that wants `event` of it, with `fields`, not waiting for answers.
    async fn tell(&self, event: &str, fields: Map<String, Value>) {
        self.host.tell(event, self.with_agent(fields)).await;
    }

    /// Asks the extensions that want `turn_start` about `prompt`, which is about to start a turn: one may block it with `block` and `reason`, rewrite it with `prompt`, which the next one sees, and add `context`.
    pub async fn turn_start(&self, prompt: &str) -> Prompted {
        let mut current = prompt.to_owned();
        let mut rewritten = false;
        let mut context = Vec::new();
        let mut blocked = None;
        let shown = std::sync::Mutex::new(current.clone());
        self.ask_each(
            "turn_start",
            || fields(json!({"prompt": shown.lock().unwrap_or_else(|held| held.into_inner()).clone()})),
            |name, answer| {
                let Ok(answer) = answer else {
                    return true;
                };
                if flag(&answer, "block") {
                    blocked = Some(
                        text(&answer, "reason")
                            .unwrap_or_else(|| format!("the extension {name} blocked it")),
                    );
                    return false;
                }
                if let Some(prompt) = answer.get("prompt").and_then(Value::as_str) {
                    current = prompt.to_owned();
                    *shown.lock().unwrap_or_else(|held| held.into_inner()) = current.clone();
                    rewritten = true;
                }
                context.extend(text(&answer, "context"));
                true
            },
        )
        .await;
        if let Some(reason) = blocked {
            return Prompted::Blocked(reason);
        }
        if !rewritten && context.is_empty() {
            return Prompted::Go;
        }
        Prompted::Changed {
            prompt: rewritten.then_some(current),
            context: joined(context),
        }
    }

    /// Tells the extensions that want `turn_end` that a turn ended, with the reason it failed when it did.
    pub async fn turn_end(&self, error: Option<&str>) {
        self.tell("turn_end", fields(json!({"error": error}))).await;
    }

    /// Asks the extensions that want `session_start` that the conversation started, as `source` says (`startup`, `clear`, `resume` or `compact`), with `model`; returns the `context` they add, in order.
    pub async fn session_start(&self, source: &str, model: Option<&str>) -> Option<String> {
        let mut context = Vec::new();
        self.ask_each(
            "session_start",
            || fields(json!({"source": source, "model": model})),
            |_, answer| {
                if let Ok(answer) = answer {
                    context.extend(text(&answer, "context"));
                }
                true
            },
        )
        .await;
        joined(context)
    }

    /// Tells the extensions that want `session_end` that the conversation ended, for `reason`.
    pub async fn session_end(&self, reason: &str) {
        self.tell("session_end", fields(json!({"reason": reason})))
            .await;
    }

    /// Tells the extensions that want `notification` that crab waits on the reader: `kind` is the kind of notification, `title` and `message` what it says.
    pub async fn notification(&self, kind: &str, title: &str, message: &str) {
        self.tell(
            "notification",
            fields(json!({"kind": kind, "title": title, "message": message})),
        )
        .await;
    }

    /// Tells the extensions that want `model_change` that the conversation's model changed from `from` to `to`.
    pub async fn model_change(&self, from: &str, to: &str) {
        self.tell("model_change", fields(json!({"from": from, "to": to})))
            .await;
    }

    /// What the extensions that want `tool_result` make of `call`, which ran with `arguments` and gave `output`, failing when `is_error` is set, after `took`.
    async fn after(
        &self,
        call: &ToolCall,
        arguments: &Value,
        output: &str,
        is_error: bool,
        took: Option<Duration>,
    ) -> Followed {
        let shown = std::sync::Mutex::new(output.to_owned());
        let mut replaced = None;
        let mut notes = Vec::new();
        let mut halt = None;
        self.ask_each(
            "tool_result",
            || {
                let mut about = about_call(call, arguments);
                about.insert(
                    "output".into(),
                    json!(
                        shown
                            .lock()
                            .unwrap_or_else(|held| held.into_inner())
                            .clone()
                    ),
                );
                about.insert("is_error".into(), json!(is_error));
                if let Some(took) = took {
                    about.insert(
                        "duration_ms".into(),
                        json!(u64::try_from(took.as_millis()).unwrap_or(u64::MAX)),
                    );
                }
                about
            },
            |_, answer| {
                let Ok(answer) = answer else {
                    return true;
                };
                if let Some(output) = answer.get("output").and_then(Value::as_str) {
                    *shown.lock().unwrap_or_else(|held| held.into_inner()) = output.to_owned();
                    replaced = Some(output.to_owned());
                }
                notes.extend(text(&answer, "context"));
                halt = halt.take().or_else(|| text(&answer, "stop"));
                true
            },
        )
        .await;
        Followed {
            note: joined(notes),
            output: replaced,
            halt,
        }
    }

    /// Asks the extensions that want `stop` (`subagent_stop` for a subagent) whether the agent should go on instead of ending with `answer`; `again` says it already went on because of such a check. The `continue` reasons they give, joined.
    async fn going_on(&self, answer: &str, again: bool) -> Option<String> {
        let event = if self.agent.is_some() {
            "subagent_stop"
        } else {
            "stop"
        };
        let mut reasons = Vec::new();
        self.ask_each(
            event,
            || fields(json!({"answer": answer, "again": again})),
            |_, answer| {
                if let Ok(answer) = answer {
                    reasons.extend(text(&answer, "continue"));
                }
                true
            },
        )
        .await;
        joined(reasons)
    }
}

#[async_trait]
impl Hooks for ExtensionHooks {
    /// Asks the extensions that want `tool_call`, each with the arguments the ones before it left: `block` refuses the call with `reason`, `input` rewrites the arguments, `decision` of `allow`, `ask` or `deny` decides about asking the reader, `context` adds a note and `stop` stops the turn after the batch. An extension that gives no answer refuses the call.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened {
        let shown = std::sync::Mutex::new(arguments.clone());
        let mut rewritten = None;
        let mut notes = Vec::new();
        let mut halt = None;
        let mut denied = None;
        let mut asked = false;
        let mut allowed = false;
        self.ask_each(
            "tool_call",
            || about_call(call, &shown.lock().unwrap_or_else(|held| held.into_inner())),
            |name, answer| {
                let answer = match answer {
                    Ok(answer) => answer,
                    Err(failure) => {
                        denied = Some(format!(
                            "the extension {name} gave no answer, so the call was not run: {failure}"
                        ));
                        return false;
                    }
                };
                halt = halt.take().or_else(|| text(&answer, "stop"));
                notes.extend(text(&answer, "context"));
                let decision = text(&answer, "decision");
                if flag(&answer, "block") || decision.as_deref() == Some("deny") {
                    denied = Some(
                        text(&answer, "reason")
                            .unwrap_or_else(|| format!("the extension {name} blocked it")),
                    );
                    return false;
                }
                if let Some(input) = answer.get("input").filter(|input| input.is_object()) {
                    *shown.lock().unwrap_or_else(|held| held.into_inner()) = input.clone();
                    rewritten = Some(input.clone());
                }
                match decision.as_deref() {
                    Some("ask") => asked = true,
                    Some("allow") => allowed = true,
                    _ => {}
                }
                true
            },
        )
        .await;
        let gate = match denied {
            Some(reason) => Gate::Deny(reason),
            None if asked => Gate::Ask,
            None if allowed => Gate::Allow,
            None => Gate::Pass,
        };
        Screened {
            gate,
            arguments: rewritten,
            note: joined(notes),
            halt,
        }
    }

    /// Asks the extensions that want `tool_result` about the result: `output` replaces it, `context` adds a note and `stop` stops the turn after the batch.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Followed {
        self.after(call, arguments, output, false, None).await
    }

    /// Asks the extensions that want `tool_result` about the failure, as about a result with `is_error` set.
    async fn after_tool_failed(
        &self,
        call: &ToolCall,
        arguments: &Value,
        error: &str,
        took: Duration,
    ) -> Followed {
        self.after(call, arguments, error, true, Some(took)).await
    }

    /// Asks the extensions that want `permission_request` about a call the reader is about to be asked about: `decision` of `allow` runs it, with `input` as its arguments and `rules` added for the conversation, and `deny` refuses it with `reason`, stopping the turn when `stop` is set.
    async fn permission_request(&self, call: &ToolCall, arguments: &Value) -> Permit {
        let mut permit = Permit::Pass;
        self.ask_each(
            "permission_request",
            || {
                let mut about = about_call(call, arguments);
                about.remove("tool_use_id");
                about
            },
            |name, answer| {
                let Ok(answer) = answer else {
                    return true;
                };
                match text(&answer, "decision").as_deref() {
                    Some("deny") => {
                        permit = Permit::Deny {
                            reason: text(&answer, "reason")
                                .unwrap_or_else(|| format!("the extension {name} denied it")),
                            halt: flag(&answer, "stop"),
                        };
                        false
                    }
                    Some("allow") => {
                        let rules = answer
                            .get("rules")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect();
                        permit = Permit::Allow {
                            arguments: answer.get("input").filter(|v| v.is_object()).cloned(),
                            rules,
                        };
                        true
                    }
                    _ => true,
                }
            },
        )
        .await;
        permit
    }

    /// Asks the extensions that want `tool_batch` about every call of a batch with its result: `context` adds a note after the results and `stop` stops the turn instead of sending them to the model.
    async fn after_batch(&self, calls: &[ToolCall], results: &[ToolResult]) -> Batched {
        let listed: Vec<Value> = calls
            .iter()
            .map(|call| {
                let mut about = about_call(call, &call.arguments);
                if let Some(result) = results.iter().find(|result| result.call_id == call.id) {
                    about.insert("output".into(), json!(result.text));
                    about.insert("is_error".into(), json!(result.is_error));
                }
                Value::Object(about)
            })
            .collect();
        let mut notes = Vec::new();
        let mut halt = None;
        self.ask_each(
            "tool_batch",
            || fields(json!({"calls": listed})),
            |_, answer| {
                if let Ok(answer) = answer {
                    notes.extend(text(&answer, "context"));
                    halt = halt.take().or_else(|| text(&answer, "stop"));
                }
                true
            },
        )
        .await;
        Batched {
            note: joined(notes),
            halt,
        }
    }

    /// Tells the extensions that want `message` what the model replied: its text, its thinking and its tool calls.
    async fn replied(&self, reply: &Reply) {
        let calls: Vec<Value> = reply
            .tool_calls
            .iter()
            .map(|call| json!({"id": call.id, "name": call.name, "arguments": call.arguments}))
            .collect();
        self.tell(
            "message",
            fields(json!({
                "role": "assistant",
                "text": reply.text,
                "thinking": reply.thinking,
                "tool_calls": calls,
            })),
        )
        .await;
    }

    /// Tells the extensions that want `compact_end` the summary a compaction made.
    async fn after_compact(&self, trigger: CompactTrigger, summary: &str) {
        self.tell(
            "compact_end",
            fields(json!({"trigger": trigger.name(), "summary": summary})),
        )
        .await;
    }

    /// Asks the extensions that want `stop`, or `subagent_stop` for a subagent, whether to go on.
    async fn stop(&self, answer: &str, again: bool) -> Option<String> {
        self.going_on(answer, again).await
    }

    /// Asks the extensions that want `compact_start` about a compaction: `block` keeps it from happening, with `reason`, and `instructions` adds to what the summary should give attention to.
    async fn before_compact(&self, trigger: CompactTrigger, focus: Option<&str>) -> Compacting {
        let mut blocked = None;
        let mut instructions = Vec::new();
        self.ask_each(
            "compact_start",
            || fields(json!({"trigger": trigger.name(), "focus": focus})),
            |name, answer| {
                let Ok(answer) = answer else {
                    return true;
                };
                if flag(&answer, "block") {
                    blocked = Some(
                        text(&answer, "reason")
                            .unwrap_or_else(|| format!("the extension {name} blocked it")),
                    );
                    return false;
                }
                instructions.extend(text(&answer, "instructions"));
                true
            },
        )
        .await;
        match blocked {
            Some(reason) => Compacting::Blocked(reason),
            None => Compacting::Go(joined(instructions)),
        }
    }

    /// Asks the extensions that want `subagent_start` about a subagent that is about to start, and returns the `context` they add; nothing for the main agent.
    async fn subagent_start(&self) -> Option<String> {
        self.agent.as_ref()?;
        let mut context = Vec::new();
        self.ask_each("subagent_start", Map::new, |_, answer| {
            if let Ok(answer) = answer {
                context.extend(text(&answer, "context"));
            }
            true
        })
        .await;
        joined(context)
    }

    /// The same extensions, run for the subagent `id` of the kind `kind`.
    fn for_subagent(&self, id: &str, kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Self {
            host: self.host.clone(),
            agent: Some((id.to_owned(), kind.to_owned())),
        })
    }
}
