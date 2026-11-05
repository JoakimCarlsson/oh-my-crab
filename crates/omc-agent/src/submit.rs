//! The `submit_result` tool: how a subagent asked for a structured result hands it back.

use std::sync::{Arc, Mutex, MutexGuard};

use async_trait::async_trait;
use omc_tools::{Tool, ToolError, ToolOutput};
use serde_json::{Value, json};

use crate::schema::{fits, violations};

/// The name the subagent calls the tool by.
pub(crate) const SUBMIT_RESULT: &str = "submit_result";

/// What a subagent handed in through `submit_result`.
#[derive(Debug, Default)]
pub(crate) struct Submission {
    /// The last value that matched the schema.
    pub(crate) value: Option<Value>,
    /// Why the last value was turned away, when the last call was turned away.
    pub(crate) rejected: Option<String>,
}

/// The place a subagent's submissions are kept, shared by the tool and the run that reads them.
#[derive(Clone, Default)]
pub(crate) struct Slot(Arc<Mutex<Submission>>);

impl Slot {
    /// Locks the submission, going on with it when a panic poisoned it.
    pub(crate) fn lock(&self) -> MutexGuard<'_, Submission> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Takes a subagent's result, checks it against a schema and keeps it in a [`Slot`].
pub(crate) struct SubmitResult {
    /// The schema the result must match.
    schema: Value,
    /// Where an accepted result is kept.
    slot: Slot,
}

impl SubmitResult {
    /// The tool checking results against `schema` and keeping them in `slot`.
    pub(crate) fn new(schema: Value, slot: Slot) -> Self {
        Self { schema, slot }
    }

    /// The value `given` stands for: itself, or the JSON a string holds when only that fits the schema, as models sometimes send a structure as text.
    fn unwrap(&self, given: Value) -> Value {
        if let Value::String(text) = &given
            && !fits(&self.schema, &given)
            && let Ok(parsed) = serde_json::from_str::<Value>(text)
            && fits(&self.schema, &parsed)
        {
            return parsed;
        }
        given
    }
}

#[async_trait]
impl Tool for SubmitResult {
    /// Returns `submit_result`.
    fn name(&self) -> &'static str {
        SUBMIT_RESULT
    }

    /// Tells the subagent how to hand back its result.
    fn description(&self) -> &'static str {
        "Hand back your final result as structured data. `result` MUST match the schema. Call it once your work is done; a value that does not match is turned away with the reasons, so fix it and call again. After it is accepted, end with a one-line reply."
    }

    /// Returns the argument schema: the result, under `result`, matching the schema it was given.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": { "result": self.schema },
            "required": ["result"]
        })
    }

    /// Submitting changes nothing on disk, so read-only agents may use it.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes a call.
    fn describe(&self, _args: &Value) -> String {
        "submit the result".to_owned()
    }

    /// Keeps the result when it matches the schema, and otherwise says why it does not.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let Value::Object(mut args) = args else {
            return Err(ToolError::InvalidArguments(
                "expected an object with `result`".to_owned(),
            ));
        };
        let Some(given) = args.remove("result") else {
            return Err(ToolError::InvalidArguments("missing `result`".to_owned()));
        };
        let value = self.unwrap(given);
        let broken = violations(&self.schema, &value, "result");
        let mut submission = self.slot.lock();
        if broken.is_empty() {
            submission.value = Some(value);
            submission.rejected = None;
            return Ok(ToolOutput::new(
                "Result accepted. End now with a one-line reply.",
            ));
        }
        let reasons = format!("- {}", broken.join("\n- "));
        submission.rejected = Some(reasons.clone());
        Err(ToolError::Failed(format!(
            "the result does not match the schema:\n{reasons}\nFix it and call {SUBMIT_RESULT} again."
        )))
    }
}

#[cfg(test)]
/// Tests for the `submit_result` tool.
mod tests {
    use super::*;

    /// A tool asking for an object with a whole-number `count`, and its slot.
    fn counting() -> (SubmitResult, Slot) {
        let slot = Slot::default();
        let schema = json!({
            "type": "object",
            "properties": { "count": { "type": "integer" } },
            "required": ["count"]
        });
        (SubmitResult::new(schema, slot.clone()), slot)
    }

    /// A matching result is kept, and a later bad one is turned away without losing it.
    #[tokio::test]
    async fn keeps_matching_results() {
        let (tool, slot) = counting();
        tool.call(json!({ "result": { "count": 2 } }))
            .await
            .unwrap();
        assert_eq!(slot.lock().value, Some(json!({ "count": 2 })));
        let error = tool
            .call(json!({ "result": { "count": "two" } }))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("result.count: expected integer, got string")
        );
        let submission = slot.lock();
        assert_eq!(submission.value, Some(json!({ "count": 2 })));
        assert!(submission.rejected.is_some());
    }

    /// A structure sent as text is read as the structure.
    #[tokio::test]
    async fn reads_a_structure_sent_as_text() {
        let (tool, slot) = counting();
        tool.call(json!({ "result": "{\"count\": 3}" }))
            .await
            .unwrap();
        assert_eq!(slot.lock().value, Some(json!({ "count": 3 })));
    }

    /// The argument schema wraps the given schema under `result`.
    #[test]
    fn wraps_the_schema() {
        let (tool, _) = counting();
        assert_eq!(tool.schema()["properties"]["result"]["type"], "object");
        assert_eq!(tool.schema()["required"], json!(["result"]));
    }
}
