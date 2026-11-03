//! The `wait` tool: block until the next background run ends.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Jobs, Tool, ToolError, ToolOutput};

/// Waits for the background runs the conversation started.
pub struct Wait {
    /// The runs to wait for.
    jobs: Arc<Jobs>,
}

impl Wait {
    /// Creates the tool waiting on `jobs`.
    pub fn new(jobs: Arc<Jobs>) -> Self {
        Self { jobs }
    }
}

#[async_trait]
impl Tool for Wait {
    /// Returns `wait`.
    fn name(&self) -> &'static str {
        "wait"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Block until the next background `bash` command or `task` subagent ends, and return its handle, status \
         and output. A run that ended earlier and was not yet reported is returned at once, so call `wait` once \
         per run you started. `completed` means the run ended, not that it succeeded: a `bash` command's exit code is the first line of its output. Fails at once when nothing is running. Use it when you have nothing else to do \
         until a background run is done."
    }

    /// Returns the argument schema: none.
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    /// Waiting changes nothing, so plan mode may use it.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes a call.
    fn describe(&self, _args: &Value) -> String {
        "wait for a background run".to_owned()
    }

    /// Waits for the next run to end and reports it.
    async fn call(&self, _args: Value) -> Result<ToolOutput, ToolError> {
        let finished = self.jobs.wait().await?;
        Ok(ToolOutput::new(finished.report()))
    }
}

#[cfg(test)]
/// Tests for the `wait` tool.
mod tests {
    use super::*;

    /// The handle, status and output of a run come back, and a second wait with nothing left fails.
    #[tokio::test]
    async fn reports_a_run_and_fails_when_nothing_is_left() {
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Wait::new(jobs.clone());
        let id = jobs.spawn("job".into(), None, async { Ok("fine".to_owned()) });
        let out = tool.call(json!({})).await.unwrap();
        assert_eq!(out.text, format!("{id} completed: job\nfine"));
        assert!(matches!(
            tool.call(json!({})).await,
            Err(ToolError::Failed(_))
        ));
    }
}
