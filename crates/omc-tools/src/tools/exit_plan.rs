//! The `exit_plan_mode` tool: the plan, put to the reader for approval.

use std::sync::Arc;

use async_trait::async_trait;
use omc_context::{plan_abandoned, plan_approved, plan_approved_fresh, plan_fresh, plan_rejected};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Host, Notebook, PlanVerdict, Tool, ToolError, ToolOutput};

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
    /// The notebook that opens a fresh context window for a plan approved to be carried out in one, when the agent has one; without it such a plan is carried out in the window it was made in.
    notebook: Option<Arc<Notebook>>,
}

impl ExitPlanMode {
    /// The tool, putting plans to the reader through `host`; `todo` says whether the agent has a todo tool.
    pub fn new(host: Arc<dyn Host>, todo: bool) -> Self {
        Self {
            host,
            todo,
            notebook: None,
        }
    }

    /// The same tool, opening a fresh context window through `notebook` when the reader approves a plan to be carried out in one.
    #[must_use]
    pub fn with_notebook(self, notebook: Arc<Notebook>) -> Self {
        Self {
            notebook: Some(notebook),
            ..self
        }
    }

    /// What the agent is told when the reader approved `plan` with `note`, in a fresh context window when `fresh` is set and the agent has a notebook to open one with, which is asked for here.
    fn approved(&self, plan: &str, fresh: bool, note: Option<&str>) -> String {
        match self.notebook.as_ref().filter(|_| fresh) {
            Some(notebook) => {
                notebook.request_fresh(plan_fresh(plan, self.todo, note));
                plan_approved_fresh()
            }
            None => plan_approved(plan, self.todo, note),
        }
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

    /// Waits for the reader's verdict and tells the agent what it was: an approved plan is echoed back to be carried out, or opens a fresh context window that starts from it; a plan not approved yet or rejected is an error that says what the reader wrote.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if args.plan.trim().is_empty() {
            return Err(ToolError::InvalidArguments("the plan is empty".to_owned()));
        }
        match self.host.approve_plan(&args.plan).await {
            PlanVerdict::Approved { fresh, note } => Ok(ToolOutput::new(self.approved(
                &args.plan,
                fresh,
                note.as_deref(),
            ))),
            PlanVerdict::Refine(note) => Err(ToolError::Failed(plan_rejected(note.as_deref()))),
            PlanVerdict::Rejected(note) => Err(ToolError::Failed(plan_abandoned(note.as_deref()))),
        }
    }
}

#[cfg(test)]
/// Tests for the `exit_plan_mode` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// An approved plan is echoed back, a plan sent back is an error carrying the note, and the host saw the plan.
    #[tokio::test]
    async fn approval_decides_the_result() {
        let host = Arc::new(RecordingHost::default());
        let tool = ExitPlanMode::new(host.clone(), true);
        let rejected = tool.call(json!({"plan": "# P\nstep"})).await;
        assert!(rejected.is_err());
        *host.verdict.lock().unwrap() = Some(PlanVerdict::Refine(Some("split it".to_owned())));
        let Err(ToolError::Failed(refine)) = tool.call(json!({"plan": "# P\nstep"})).await else {
            panic!("a plan sent back is an error");
        };
        assert!(refine.contains("split it"));
        *host.verdict.lock().unwrap() = Some(PlanVerdict::Rejected(None));
        let Err(ToolError::Failed(stop)) = tool.call(json!({"plan": "# P\nstep"})).await else {
            panic!("a rejected plan is an error");
        };
        assert!(stop.contains("End your turn now"));
        *host.approve.lock().unwrap() = true;
        *host.verdict.lock().unwrap() = None;
        let approved = tool.call(json!({"plan": "# P\nstep"})).await.unwrap();
        assert!(approved.text.contains("# P"));
        assert_eq!(host.plans_proposed.lock().unwrap().len(), 4);
        assert_eq!(tool.describe(&json!({"plan": "\n# Title\nx"})), "Title");
    }

    /// A plan approved for a fresh context window asks the notebook for one that starts with the plan and the note; without a notebook it is carried out where it was made.
    #[tokio::test]
    async fn a_fresh_approval_opens_a_window_seeded_with_the_plan() {
        let host = Arc::new(RecordingHost::default());
        *host.verdict.lock().unwrap() = Some(PlanVerdict::Approved {
            fresh: true,
            note: Some("tests first".to_owned()),
        });
        let notebook = Arc::new(Notebook::new());
        let tool = ExitPlanMode::new(host.clone(), false).with_notebook(notebook.clone());
        let said = tool.call(json!({"plan": "# P\nstep"})).await.unwrap();
        assert!(said.text.contains("fresh context window"));
        let opening = notebook.take_reset().unwrap();
        assert_eq!(omc_context::seeded_plan(&opening), Some("# P\nstep"));
        assert!(opening.contains("tests first"));
        let bare = ExitPlanMode::new(host, false);
        let said = bare.call(json!({"plan": "# P\nstep"})).await.unwrap();
        assert!(said.text.contains("<plan>\n# P\nstep\n</plan>"));
    }
}
