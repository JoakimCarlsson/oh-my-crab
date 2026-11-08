//! The `exit_plan_mode` tool: the plan, put to the reader for approval.

use std::sync::Arc;

use async_trait::async_trait;
use omc_context::{plan_approved, plan_rejected};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Host, Tool, ToolError, ToolOutput};

/// Arguments of an `exit_plan_mode` call.
#[derive(Deserialize)]
struct Args {
    /// The plan, as markdown.
    plan: String,
}

/// Submits the plan for approval.
pub struct ExitPlanMode {
    /// Where the plan is put to the reader.
    host: Arc<dyn Host>,
    /// Whether the agent has a `todo` tool to be told about.
    todo: bool,
}

impl ExitPlanMode {
    /// The tool, putting plans to the reader through `host`; `todo` says whether the agent has a todo tool.
    pub fn new(host: Arc<dyn Host>, todo: bool) -> Self {
        Self { host, todo }
    }
}

#[async_trait]
impl Tool for ExitPlanMode {
    /// Returns `exit_plan_mode`.
    fn name(&self) -> &'static str {
        "exit_plan_mode"
    }

    /// Submitting a plan changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Talks to the reader and ends plan mode, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/exit_plan_mode.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "plan": { "type": "string", "description": "The full plan, as markdown." }
            },
            "required": ["plan"]
        })
    }

    /// Describes a call by the plan's first line.
    fn describe(&self, args: &Value) -> String {
        args["plan"]
            .as_str()
            .and_then(|plan| plan.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or("plan")
            .trim_start_matches('#')
            .trim()
            .to_owned()
    }

    /// Waits for the reader's verdict and tells the agent what it was.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if args.plan.trim().is_empty() {
            return Err(ToolError::InvalidArguments("the plan is empty".to_owned()));
        }
        if self.host.approve_plan(&args.plan).await {
            Ok(ToolOutput::new(plan_approved(&args.plan, self.todo)))
        } else {
            Err(ToolError::Failed(plan_rejected()))
        }
    }
}

#[cfg(test)]
/// Tests for the `exit_plan_mode` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// An approved plan is echoed back, a rejected one is an error, and the host saw the plan.
    #[tokio::test]
    async fn approval_decides_the_result() {
        let host = Arc::new(RecordingHost::default());
        let tool = ExitPlanMode::new(host.clone(), true);
        let rejected = tool.call(json!({"plan": "# P\nstep"})).await;
        assert!(rejected.is_err());
        *host.approve.lock().unwrap() = true;
        let approved = tool.call(json!({"plan": "# P\nstep"})).await.unwrap();
        assert!(approved.text.contains("# P"));
        assert_eq!(host.plans_proposed.lock().unwrap().len(), 2);
        assert_eq!(tool.describe(&json!({"plan": "\n# Title\nx"})), "Title");
    }
}
