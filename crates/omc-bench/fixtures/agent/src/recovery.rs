//! Getting a turn past failures that need not end it: requests that fail for a while, and replies that stop short or cannot be read.

use std::hash::{BuildHasher, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use omc_providers::{Message, ProviderError, Reply, ToolCall};
use omc_tools::sanitize;
use serde_json::Value;

use crate::AgentEvent;

/// How many times one request is sent again after failing in a way that is likely to pass.
pub(crate) const MAX_RETRIES: u32 = 8;

/// The wait before the first time a request is sent again; each later wait is twice the one before, give or take some jitter.
const FIRST_WAIT: Duration = Duration::from_secs(2);

/// The longest wait before a request is sent again; a provider that asks for a longer one is not waited for.
pub(crate) const MAX_WAIT: Duration = Duration::from_secs(60);

/// How many replies that stopped short, empty or cut off by the output limit before any tool call, a turn recovers from by nudging the model on.
pub(crate) const MAX_RECOVERIES: u32 = 3;

/// How many replies whose tool calls the output limit cut off a turn answers with a notice to take a smaller step before it fails; counted apart from [`MAX_RECOVERIES`], so harmless recoveries earlier in the turn do not use it up.
pub(crate) const MAX_CUT_CALLS: u32 = 3;

/// The serial number of the next wait shown, counted over the life of the process so that no two waits are shown under the same id, whatever turn they belong to.
static NEXT_WAIT: AtomicU64 = AtomicU64::new(1);

/// The name a wait before sending a request again is shown under, as if it were a tool call.
pub(crate) const RETRY: &str = "retry";

/// Sent along when the model gave an empty reply.
pub(crate) const EMPTY_REPLY: &str = include_str!("prompts/empty_reply.md");

/// Sent along when the output limit cut off a reply that had text but no tool calls.
pub(crate) const LENGTH_TEXT: &str = include_str!("prompts/length_text.md");

/// Sent along when the output limit was used up before the reply held any text or tool call.
pub(crate) const LENGTH_THINKING: &str = include_str!("prompts/length_thinking.md");

/// Added to the results of the calls of a reply the output limit cut off.
pub(crate) const LENGTH_CALLS: &str = include_str!("prompts/length_calls.md");

/// The result of each call of a reply the output limit cut off, none of which is run.
pub(crate) const CUT_CALL: &str = "Not run: the reply that made this call ran into the output limit, so its arguments may be incomplete.";

/// How long to wait before sending a request again after it failed with `error` for the `attempt`th time, counted from one.
///
/// Returns `None` when the request is not to be sent again: the failure is not one that passes, the attempts are used up, or the provider asked for a wait longer than [`MAX_WAIT`]. A wait the provider asked for is kept to; otherwise the wait doubles with each attempt, up to [`MAX_WAIT`], with jitter so that many clients do not all come back at once.
pub(crate) fn retry_wait(error: &ProviderError, attempt: u32) -> Option<Duration> {
    if attempt > MAX_RETRIES || !error.is_transient() {
        return None;
    }
    match error.retry_after() {
        Some(asked) => (asked <= MAX_WAIT).then_some(asked),
        None => Some(backoff(attempt, jitter())),
    }
}

/// The wait before the `attempt`th retry, counted from one: [`FIRST_WAIT`] doubled for each attempt before it and capped at [`MAX_WAIT`], then scaled to between half and all of that by `spread`, a number from nought up to one.
fn backoff(attempt: u32, spread: f64) -> Duration {
    let doublings = attempt.saturating_sub(1).min(16);
    let full = FIRST_WAIT.saturating_mul(1 << doublings).min(MAX_WAIT);
    full.mul_f64(0.5 + spread.clamp(0.0, 1.0) / 2.0)
}

/// A number from nought up to one that differs from call to call, taken from the randomly seeded keys of the standard hasher.
fn jitter() -> f64 {
    let bits = std::collections::hash_map::RandomState::new()
        .build_hasher()
        .finish();
    (bits >> 11) as f64 / (1u64 << 53) as f64
}

/// The whole seconds `wait` comes to, rounded up and at least one, as the reader is told it.
fn seconds(wait: Duration) -> u64 {
    (wait.as_secs_f64().ceil() as u64).max(1)
}

/// One wait before a failed request is sent again, as the reader is shown it.
pub(crate) struct Retry {
    /// The serial number of the wait in the process, so that each is shown on its own.
    serial: u64,
    /// Which time the request is to be sent again, counted from one.
    attempt: u32,
    /// How long the wait is.
    wait: Duration,
}

impl Retry {
    /// A wait of `wait` before the request is sent again for the `attempt`th time, counted from one, under a serial number no other wait of the process has.
    pub(crate) fn new(attempt: u32, wait: Duration) -> Self {
        Self {
            serial: NEXT_WAIT.fetch_add(1, Ordering::Relaxed),
            attempt,
            wait,
        }
    }

    /// The id the wait is shown under.
    fn id(&self) -> String {
        format!("{RETRY}-{}", self.serial)
    }

    /// The event that shows the wait starting after the request failed with `error`: which attempt comes, how soon, and why.
    pub(crate) fn started(&self, error: &ProviderError) -> AgentEvent {
        let reason = error.to_string();
        let first_line = reason.lines().next().unwrap_or_default();
        let short: String = first_line.chars().take(160).collect();
        AgentEvent::ToolStart {
            id: self.id(),
            name: RETRY.to_owned(),
            summary: sanitize(&format!(
                "attempt {} of {MAX_RETRIES} in {} s: {short}",
                self.attempt,
                seconds(self.wait)
            )),
        }
    }

    /// The event that shows the wait over, with the failure `error` it was for.
    pub(crate) fn finished(&self, error: &ProviderError) -> AgentEvent {
        AgentEvent::ToolDone {
            id: self.id(),
            name: RETRY.to_owned(),
            text: format!("{error}\n\nSent again after {} s.", seconds(self.wait)),
            is_error: false,
            changes: Vec::new(),
        }
    }
}

/// Adds `notice` to the end of `messages`, the messages of a request: into the last one when it is a prompt or a batch of tool results, which the model is about to answer, and as a message of its own after a reply.
pub(crate) fn add_notice(messages: &mut Vec<Message>, notice: &str) {
    let notice = notice.trim();
    match messages.last_mut() {
        Some(Message::User(text)) => {
            text.push_str("\n\n");
            text.push_str(notice);
        }
        Some(Message::ToolResults(results)) if !results.is_empty() => {
            if let Some(last) = results.last_mut() {
                last.text.push_str("\n\n");
                last.text.push_str(notice);
            }
        }
        _ => messages.push(Message::User(notice.to_owned())),
    }
}

/// The one reply made of `cut`, a reply the output limit cut off, and `rest`, the reply that carried it on.
///
/// The text and thinking run on from one into the other; the tool calls, cost and stop are those of `rest`, and so are the blocks to send back, unless it has none.
pub(crate) fn joined(cut: Reply, rest: Reply) -> Reply {
    let thinking = match (cut.thinking.is_empty(), rest.thinking.is_empty()) {
        (_, true) => cut.thinking,
        (true, false) => rest.thinking,
        (false, false) => format!("{}\n\n{}", cut.thinking, rest.thinking),
    };
    Reply {
        text: cut.text + &rest.text,
        thinking,
        opaque: if rest.opaque.is_empty() {
            cut.opaque
        } else {
            rest.opaque
        },
        ..rest
    }
}

/// `reply`, a whole reply that is to be carried on by the next one, with a paragraph break after its text so the two read as one.
pub(crate) fn carried_on(mut reply: Reply) -> Reply {
    if !reply.text.trim().is_empty() {
        reply.text.push_str("\n\n");
    }
    reply
}

/// The arguments of `call` as a JSON object, read out of a string when the model sent the object encoded as JSON text.
///
/// # Errors
///
/// Returns why the call cannot be run, for the model, when the arguments are not a JSON object and not a string holding one: the problem with them and what to do instead.
pub(crate) fn readable(call: &ToolCall) -> Result<Value, String> {
    let problem = match &call.arguments {
        Value::Object(_) => return Ok(call.arguments.clone()),
        Value::String(raw) => match serde_json::from_str::<Value>(raw) {
            Ok(object @ Value::Object(_)) => return Ok(object),
            Err(error) => format!("they are not valid JSON ({error})"),
            Ok(value) => format!("they hold {}, not a JSON object", kind(&value)),
        },
        other => format!("they are {}, not a JSON object", kind(other)),
    };
    Err(format!(
        "Not run: the arguments of this call could not be read: {problem}. Send the call again with its arguments as one JSON object that matches the schema of `{}`.",
        call.name
    ))
}

/// The kind of JSON value `value` is, with its article, as in "a string".
fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

#[cfg(test)]
/// Tests for the retry waits and the recovery helpers.
mod tests {
    use omc_providers::{Stop, ToolResult};
    use serde_json::json;

    use super::*;

    /// Returns a status error with `status`, `body` and the wait it asks for.
    fn status(status: &str, body: &str, retry_after: Option<Duration>) -> ProviderError {
        ProviderError::Status {
            status: status.to_owned(),
            body: body.to_owned(),
            retry_after,
        }
    }

    /// The wait doubles with each attempt between half and all of its full length, and stops growing at the cap.
    #[test]
    fn waits_grow_and_are_capped() {
        assert_eq!(backoff(1, 1.0), Duration::from_secs(2));
        assert_eq!(backoff(1, 0.0), Duration::from_secs(1));
        assert_eq!(backoff(3, 1.0), Duration::from_secs(8));
        assert_eq!(backoff(9, 1.0), MAX_WAIT);
        assert_eq!(backoff(40, 1.0), MAX_WAIT);
        for attempt in 1..=MAX_RETRIES {
            let wait = backoff(attempt, jitter());
            assert!(
                wait >= Duration::from_secs(1) && wait <= MAX_WAIT,
                "{wait:?}"
            );
        }
    }

    /// Only failures that pass are retried, a limited number of times, and a wait the provider asks for is kept to unless it is too long.
    #[test]
    fn retries_only_what_passes() {
        let busy = status("529 <unknown status code>", "overloaded", None);
        assert!(retry_wait(&busy, 1).is_some());
        assert!(retry_wait(&busy, MAX_RETRIES).is_some());
        assert!(retry_wait(&busy, MAX_RETRIES + 1).is_none());
        assert!(retry_wait(&status("401 Unauthorized", "", None), 1).is_none());
        let asked = status("429 Too Many Requests", "", Some(Duration::from_secs(7)));
        assert_eq!(retry_wait(&asked, 1), Some(Duration::from_secs(7)));
        let too_long = status("429 Too Many Requests", "", Some(Duration::from_secs(3600)));
        assert!(retry_wait(&too_long, 1).is_none());
    }

    /// A notice joins a prompt or the last tool result, and follows a reply as a message of its own.
    #[test]
    fn notices_join_what_the_model_answers() {
        let mut prompt = vec![Message::User("go".into())];
        add_notice(&mut prompt, "\nnote\n");
        assert_eq!(prompt, [Message::User("go\n\nnote".into())]);
        let result = |text: &str| ToolResult {
            call_id: "c".into(),
            name: "read".into(),
            text: text.into(),
            is_error: false,
            images: Vec::new(),
        };
        let mut results = vec![Message::ToolResults(vec![result("a"), result("b")])];
        add_notice(&mut results, "note");
        assert_eq!(
            results,
            [Message::ToolResults(vec![result("a"), result("b\n\nnote")])]
        );
        let mut replied = vec![Message::Assistant(Reply::default())];
        add_notice(&mut replied, "note");
        assert_eq!(replied[1], Message::User("note".into()));
    }

    /// A reply carried on after the output limit reads as one, with the later reply's calls.
    #[test]
    fn a_carried_on_reply_reads_as_one() {
        let cut = Reply {
            text: "Hello, wo".into(),
            thinking: "first".into(),
            opaque: vec![json!({ "type": "thinking" })],
            stop: Stop::Length,
            ..Reply::default()
        };
        let rest = Reply {
            text: "rld.".into(),
            tool_calls: vec![ToolCall {
                id: "c".into(),
                name: "read".into(),
                arguments: json!({}),
            }],
            ..Reply::default()
        };
        let whole = joined(cut, rest);
        assert_eq!(whole.text, "Hello, world.");
        assert_eq!(whole.thinking, "first");
        assert_eq!(whole.opaque.len(), 1);
        assert_eq!(whole.tool_calls.len(), 1);
        assert_eq!(whole.stop, Stop::Finished);
    }

    /// Arguments that are not an object are turned away with what is wrong with them, and an object sent as JSON text is read out of it.
    #[test]
    fn unreadable_arguments_are_explained() {
        let call = |arguments: Value| ToolCall {
            id: "c".into(),
            name: "edit".into(),
            arguments,
        };
        assert_eq!(
            readable(&call(json!({ "path": "a" }))),
            Ok(json!({ "path": "a" }))
        );
        assert_eq!(
            readable(&call(json!("{\"path\": \"a\"}"))),
            Ok(json!({ "path": "a" }))
        );
        let broken = readable(&call(json!("{\"path\": \"a\""))).unwrap_err();
        assert!(broken.contains("not valid JSON"), "{broken}");
        assert!(broken.contains("`edit`"), "{broken}");
        let listed = readable(&call(json!("[1, 2]"))).unwrap_err();
        assert!(
            listed.contains("hold an array, not a JSON object"),
            "{listed}"
        );
        let number = readable(&call(json!(3))).unwrap_err();
        assert!(
            number.contains("are a number, not a JSON object"),
            "{number}"
        );
    }

    /// Waits are shown under ids that never repeat, even for the same attempt of the same request.
    #[test]
    fn waits_are_shown_under_ids_of_their_own() {
        let first = Retry::new(1, Duration::from_secs(1));
        let again = Retry::new(1, Duration::from_secs(1));
        assert_ne!(first.id(), again.id());
    }
}
