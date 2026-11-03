//! The `set_title` tool: the agent names its conversation.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Host, Tool, ToolError, ToolOutput};

/// The longest title that is kept, in characters.
const LIMIT: usize = 80;

/// Arguments of a `set_title` call.
#[derive(Deserialize)]
struct Args {
    /// The title.
    title: String,
}

/// Names the conversation when it has no name.
pub struct SetTitle {
    /// Where the name goes.
    host: Arc<dyn Host>,
}

impl SetTitle {
    /// The tool, naming through `host`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

#[async_trait]
impl Tool for SetTitle {
    /// Returns `set_title`.
    fn name(&self) -> &'static str {
        "set_title"
    }

    /// Naming changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/set_title.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "A short title for the conversation." }
            },
            "required": ["title"]
        })
    }

    /// Describes a call by the title it gives.
    fn describe(&self, args: &Value) -> String {
        args["title"].as_str().unwrap_or_default().to_owned()
    }

    /// Names the conversation, or says it already has a name.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let title: String = args.title.split_whitespace().collect::<Vec<_>>().join(" ");
        let title: String = title.chars().take(LIMIT).collect();
        if title.is_empty() {
            return Err(ToolError::InvalidArguments("the title is empty".to_owned()));
        }
        if self.host.name(&title) {
            Ok(ToolOutput::new(format!("Named the conversation: {title}")))
        } else {
            Ok(ToolOutput::new(
                "The conversation already has a name, which stays.",
            ))
        }
    }
}

#[cfg(test)]
/// Tests for the `set_title` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// The first name is taken, trimmed and cut, and a later one is refused.
    #[tokio::test]
    async fn the_first_name_stays() {
        let host = Arc::new(RecordingHost::default());
        let tool = SetTitle::new(host.clone());
        let first = tool
            .call(json!({"title": "  Fix   the bug "}))
            .await
            .unwrap();
        assert!(first.text.contains("Fix the bug"));
        let second = tool.call(json!({"title": "Other"})).await.unwrap();
        assert!(second.text.contains("already has a name"));
        assert_eq!(host.named.lock().unwrap().as_deref(), Some("Fix the bug"));
        assert!(tool.call(json!({"title": "  "})).await.is_err());
    }
}
