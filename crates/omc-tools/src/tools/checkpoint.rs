//! The `checkpoint` and `rewind` tools: mark a point in the conversation, then replace everything after it with a report.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Checkpoint, Tool, ToolError, ToolOutput};

/// Arguments of a `checkpoint` call.
#[derive(Deserialize)]
struct CheckpointArgs {
    /// What the investigation is meant to find out, when the model says.
    goal: Option<String>,
}

/// Marks the current point in the conversation.
pub struct CheckpointTool {
    /// The checkpoint.
    checkpoint: Arc<Checkpoint>,
}

impl CheckpointTool {
    /// The tool, working on `checkpoint`.
    pub fn new(checkpoint: Arc<Checkpoint>) -> Self {
        Self { checkpoint }
    }
}

#[async_trait]
impl Tool for CheckpointTool {
    /// Returns `checkpoint`.
    fn name(&self) -> &'static str {
        "checkpoint"
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/checkpoint.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "goal": {
                    "type": "string",
                    "description": "What the investigation is meant to find out. Optional."
                }
            }
        })
    }

    /// Describes a call.
    fn describe(&self, args: &Value) -> String {
        match args["goal"].as_str() {
            Some(goal) if !goal.trim().is_empty() => format!("checkpoint: {goal}"),
            _ => "mark a checkpoint".to_owned(),
        }
    }

    /// Marks the checkpoint, which is fixed when the batch of calls has finished.
    ///
    /// # Errors
    ///
    /// Fails when a checkpoint is already active; nothing is then changed.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: CheckpointArgs = parse_args(args)?;
        self.checkpoint.begin().map_err(ToolError::Failed)?;
        let goal = args
            .goal
            .map(|goal| goal.trim().to_owned())
            .filter(|goal| !goal.is_empty());
        Ok(ToolOutput::new(match goal {
            Some(goal) => format!(
                "Checkpoint marked: {goal}\nExplore, then call rewind with a report of what you found."
            ),
            None => "Checkpoint marked. Explore, then call rewind with a report of what you found."
                .to_owned(),
        }))
    }
}

/// Arguments of a `rewind` call.
#[derive(Deserialize)]
struct RewindArgs {
    /// What replaces the exploration.
    report: String,
}

/// Replaces everything since the checkpoint with a report.
pub struct Rewind {
    /// The checkpoint.
    checkpoint: Arc<Checkpoint>,
}

impl Rewind {
    /// The tool, working on `checkpoint`.
    pub fn new(checkpoint: Arc<Checkpoint>) -> Self {
        Self { checkpoint }
    }
}

#[async_trait]
impl Tool for Rewind {
    /// Returns `rewind`.
    fn name(&self) -> &'static str {
        "rewind"
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/rewind.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "report": {
                    "type": "string",
                    "description": "What you found and what you still need from the exploration; it replaces the removed messages. Must not be empty."
                }
            },
            "required": ["report"]
        })
    }

    /// Describes a call.
    fn describe(&self, _args: &Value) -> String {
        "rewind to the checkpoint".to_owned()
    }

    /// Asks for the rewind, which happens when the batch of calls has finished.
    ///
    /// # Errors
    ///
    /// Fails when there is no checkpoint to rewind to or the report is empty; nothing is then changed.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: RewindArgs = parse_args(args)?;
        self.checkpoint
            .request_rewind(&args.report)
            .map_err(ToolError::Failed)?;
        Ok(ToolOutput::new("Rewind requested."))
    }
}

#[cfg(test)]
/// Tests for the checkpoint tools.
mod tests {
    use super::*;

    /// The tools mark, refuse a second mark, and ask for a rewind with the report.
    #[tokio::test]
    async fn mark_then_rewind() {
        let checkpoint = Arc::new(Checkpoint::new());
        let mark = CheckpointTool::new(checkpoint.clone());
        let rewind = Rewind::new(checkpoint.clone());
        assert!(rewind.call(json!({"report": "r"})).await.is_err());
        mark.call(json!({})).await.unwrap();
        assert!(mark.call(json!({})).await.is_err());
        checkpoint.settle(3);
        assert!(rewind.call(json!({"report": " "})).await.is_err());
        rewind.call(json!({"report": "found"})).await.unwrap();
        assert_eq!(checkpoint.take_rewind(), Some((3, "found".to_owned())));
    }

    /// Both tools are read-only for approval purposes.
    #[test]
    fn both_are_read_only() {
        let checkpoint = Arc::new(Checkpoint::new());
        assert!(CheckpointTool::new(checkpoint.clone()).read_only());
        assert!(Rewind::new(checkpoint).read_only());
    }
}
