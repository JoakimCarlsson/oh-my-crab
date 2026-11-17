//! The `todo` tool: the agent's task list, shown to the reader.

use std::sync::Arc;

use async_trait::async_trait;
use omc_wire::{PlanStep, Progress};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::dependencies::{mark_blocked, nodes_of, resolve};
use crate::tool::parse_args;
use crate::{Concurrency, Host, Tool, ToolError, ToolOutput};

/// One task of a `todo` call.
#[derive(Deserialize)]
struct Item {
    /// What the task is.
    content: String,
    /// How far along it is.
    #[serde(default)]
    status: String,
    /// The name other tasks depend on it by; its content names it too.
    #[serde(default)]
    id: Option<String>,
    /// The ids or contents of the tasks it waits on.
    #[serde(default)]
    depends_on: Vec<String>,
}

/// Arguments of a `todo` call.
#[derive(Deserialize)]
struct Args {
    /// The whole list.
    todos: Vec<Item>,
}

/// Replaces the task list the reader sees.
pub struct Todo {
    /// Where the list is shown.
    host: Arc<dyn Host>,
}

impl Todo {
    /// The tool, showing its lists through `host`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

/// How far along a task is, as the agent wrote it.
fn progress(status: &str) -> Progress {
    match status {
        "in_progress" | "doing" | "active" | "started" => Progress::Doing,
        "completed" | "done" | "complete" => Progress::Done,
        _ => Progress::Pending,
    }
}

#[async_trait]
impl Tool for Todo {
    /// Returns `todo`.
    fn name(&self) -> &'static str {
        "todo"
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Changes the list the reader sees, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/todo.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "todos": {
                    "type": "array",
                    "description": "The complete task list, replacing the previous one.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "content": { "type": "string", "description": "What the task is, imperative." },
                            "status": { "type": "string", "enum": ["pending", "in_progress", "completed"] },
                            "id": { "type": "string", "description": "A short name other tasks can depend on this one by; its content works too." },
                            "depends_on": { "type": "array", "items": { "type": "string" }, "description": "The ids or contents of the tasks this one waits on. Cycles are refused." }
                        },
                        "required": ["content", "status"]
                    }
                }
            },
            "required": ["todos"]
        })
    }

    /// Describes a call by the size of its list.
    fn describe(&self, args: &Value) -> String {
        let count = args["todos"].as_array().map_or(0, Vec::len);
        format!("{count} tasks")
    }

    /// Shows the list and tells the agent how it stands.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let mut steps: Vec<PlanStep> = args
            .todos
            .into_iter()
            .filter(|item| !item.content.trim().is_empty())
            .map(|item| PlanStep {
                text: item.content.trim().to_owned(),
                progress: progress(&item.status),
                id: item
                    .id
                    .map(|id| id.trim().to_owned())
                    .filter(|id| !id.is_empty()),
                depends_on: item
                    .depends_on
                    .into_iter()
                    .map(|name| name.trim().to_owned())
                    .filter(|name| !name.is_empty())
                    .collect(),
                blocked: false,
            })
            .collect();
        let graph = resolve(&nodes_of(&steps))
            .map_err(|reason| ToolError::Failed(format!("{reason}; send the list again")))?;
        mark_blocked(&mut steps, &graph);
        let doing = steps
            .iter()
            .filter(|s| s.progress == Progress::Doing)
            .count();
        if doing > 1 {
            return Err(ToolError::Failed(
                "only one task may be in_progress at a time; send the list again".to_owned(),
            ));
        }
        if let Some(step) = steps
            .iter()
            .find(|step| step.progress == Progress::Doing && step.blocked)
        {
            return Err(ToolError::Failed(format!(
                "`{}` is in_progress but waits on a task that is not completed; finish its dependencies first, then send the list again",
                step.text
            )));
        }
        let done = steps
            .iter()
            .filter(|s| s.progress == Progress::Done)
            .count();
        let blocked: Vec<&str> = steps
            .iter()
            .filter(|step| step.blocked)
            .map(|step| step.text.as_str())
            .collect();
        let mut summary = format!(
            "Todo list updated: {} tasks, {done} completed, {doing} in progress.",
            steps.len()
        );
        if !blocked.is_empty() {
            summary.push_str(&format!(
                "\nBlocked until their dependencies are completed: {}.",
                blocked.join("; ")
            ));
        }
        self.host.plan(steps);
        Ok(ToolOutput::new(summary))
    }
}

#[cfg(test)]
/// Tests for the `todo` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// A list replaces the plan and statuses map to progress.
    #[tokio::test]
    async fn replaces_the_plan() {
        let host = Arc::new(RecordingHost::default());
        let tool = Todo::new(host.clone());
        let output = tool
            .call(json!({"todos": [
                {"content": "a", "status": "completed"},
                {"content": "b", "status": "in_progress"},
                {"content": "c", "status": "pending"},
                {"content": " ", "status": "pending"}
            ]}))
            .await
            .unwrap();
        assert!(output.text.contains("3 tasks, 1 completed, 1 in progress"));
        let plans = host.plans.lock().unwrap();
        assert_eq!(plans.len(), 1);
        let progress: Vec<Progress> = plans[0].iter().map(|s| s.progress).collect();
        assert_eq!(
            progress,
            vec![Progress::Done, Progress::Doing, Progress::Pending]
        );
    }

    /// Two active tasks are refused and nothing is shown.
    #[tokio::test]
    async fn refuses_two_active_tasks() {
        let host = Arc::new(RecordingHost::default());
        let tool = Todo::new(host.clone());
        let result = tool
            .call(json!({"todos": [
                {"content": "a", "status": "in_progress"},
                {"content": "b", "status": "in_progress"}
            ]}))
            .await;
        assert!(result.is_err());
        assert!(host.plans.lock().unwrap().is_empty());
    }

    /// Tasks waiting on unfinished tasks are shown as blocked, by id or by content.
    #[tokio::test]
    async fn shows_blocked_tasks() {
        let host = Arc::new(RecordingHost::default());
        let tool = Todo::new(host.clone());
        let output = tool
            .call(json!({"todos": [
                {"content": "a", "status": "completed", "id": "first"},
                {"content": "b", "status": "in_progress", "depends_on": ["first"]},
                {"content": "c", "status": "pending", "depends_on": ["b"]},
                {"content": "d", "status": "pending"}
            ]}))
            .await
            .unwrap();
        assert!(
            output
                .text
                .contains("Blocked until their dependencies are completed: c.")
        );
        let plans = host.plans.lock().unwrap();
        let blocked: Vec<bool> = plans[0].iter().map(|s| s.blocked).collect();
        assert_eq!(blocked, vec![false, false, true, false]);
        assert_eq!(plans[0][2].depends_on, vec!["b".to_owned()]);
    }

    /// A cycle of dependencies is refused and nothing is shown.
    #[tokio::test]
    async fn refuses_cycles() {
        let host = Arc::new(RecordingHost::default());
        let tool = Todo::new(host.clone());
        let error = tool
            .call(json!({"todos": [
                {"content": "a", "status": "pending", "depends_on": ["b"]},
                {"content": "b", "status": "pending", "depends_on": ["a"]}
            ]}))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cycle"));
        assert!(host.plans.lock().unwrap().is_empty());
    }

    /// A task cannot be in progress while it waits on one that is not completed.
    #[tokio::test]
    async fn refuses_starting_a_blocked_task() {
        let host = Arc::new(RecordingHost::default());
        let tool = Todo::new(host.clone());
        let result = tool
            .call(json!({"todos": [
                {"content": "a", "status": "pending"},
                {"content": "b", "status": "in_progress", "depends_on": ["a"]}
            ]}))
            .await;
        assert!(result.is_err());
        assert!(host.plans.lock().unwrap().is_empty());
    }
}
