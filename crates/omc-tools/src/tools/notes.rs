//! The `context_notes` and `new_context` tools: the agent's notebook and the way to continue in a fresh context window.

use std::sync::Arc;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Notebook, Tool, ToolError, ToolOutput};

/// Arguments of a `context_notes` call.
#[derive(Deserialize)]
struct Args {
    /// The new notebook, when it is to be replaced.
    text: Option<String>,
}

/// Reads or replaces the conversation's notebook.
pub struct ContextNotes {
    /// The notebook.
    notebook: Arc<Notebook>,
}

impl ContextNotes {
    /// The tool, working on `notebook`.
    pub fn new(notebook: Arc<Notebook>) -> Self {
        Self { notebook }
    }
}

#[async_trait]
impl Tool for ContextNotes {
    /// Returns `context_notes`.
    fn name(&self) -> &'static str {
        "context_notes"
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Changes the conversation's notebook or context, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/context_notes.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The new notebook, replacing the old one; an empty string clears it. Leave out to read the notebook."
                }
            }
        })
    }

    /// Describes a call as a read or a write.
    fn describe(&self, args: &Value) -> String {
        match args["text"].as_str() {
            Some(text) => format!("write {} bytes", text.len()),
            None => "read".to_owned(),
        }
    }

    /// Returns the notebook, or replaces it.
    ///
    /// # Errors
    ///
    /// Fails when the text is over the cap; the notebook is then kept.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        match args.text {
            None => {
                let text = self.notebook.read();
                if text.is_empty() {
                    Ok(ToolOutput::new(
                        "No context notes are stored for this conversation.",
                    ))
                } else {
                    Ok(ToolOutput::new(text))
                }
            }
            Some(text) => {
                self.notebook.replace(&text).map_err(ToolError::Failed)?;
                if text.is_empty() {
                    Ok(ToolOutput::new("Context notes cleared."))
                } else {
                    Ok(ToolOutput::new("Context notes saved."))
                }
            }
        }
    }
}

/// Asks for a fresh context window that carries only the notebook.
pub struct NewContext {
    /// The notebook.
    notebook: Arc<Notebook>,
}

impl NewContext {
    /// The tool, working on `notebook`.
    pub fn new(notebook: Arc<Notebook>) -> Self {
        Self { notebook }
    }
}

#[async_trait]
impl Tool for NewContext {
    /// Returns `new_context`.
    fn name(&self) -> &'static str {
        "new_context"
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Changes the conversation's notebook or context, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/new_context.md")
    }

    /// Returns the argument schema, which has no arguments.
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    /// Describes a call.
    fn describe(&self, _args: &Value) -> String {
        "start a fresh context window".to_owned()
    }

    /// Asks for the reset, which happens when the batch of calls has finished.
    ///
    /// # Errors
    ///
    /// Fails when the notebook is empty; nothing is then changed.
    async fn call(&self, _args: Value) -> Result<ToolOutput, ToolError> {
        self.notebook.request_reset().map_err(ToolError::Failed)?;
        Ok(ToolOutput::new("New context window requested."))
    }
}

#[cfg(test)]
/// Tests for the notebook tools.
mod tests {
    use super::*;

    /// Without text the notebook is returned, with text it is replaced, and an empty text clears it.
    #[tokio::test]
    async fn reads_writes_and_clears() {
        let tool = ContextNotes::new(Arc::new(Notebook::new()));
        let empty = tool.call(json!({})).await.unwrap();
        assert!(empty.text.contains("No context notes"));
        tool.call(json!({"text": "one"})).await.unwrap();
        assert_eq!(tool.call(json!({})).await.unwrap().text, "one");
        tool.call(json!({"text": ""})).await.unwrap();
        assert!(
            tool.call(json!({}))
                .await
                .unwrap()
                .text
                .contains("No context notes")
        );
    }

    /// A write over the cap fails with the limit and the old notebook stays.
    #[tokio::test]
    async fn rejects_a_write_over_the_cap() {
        let tool = ContextNotes::new(Arc::new(Notebook::new()));
        tool.call(json!({"text": "keep"})).await.unwrap();
        let error = tool
            .call(json!({"text": "x".repeat(crate::notebook::NOTEBOOK_LIMIT + 1)}))
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&crate::notebook::NOTEBOOK_LIMIT.to_string())
        );
        assert_eq!(tool.call(json!({})).await.unwrap().text, "keep");
    }

    /// A reset with an empty notebook fails and requests nothing.
    #[tokio::test]
    async fn a_reset_needs_notes() {
        let book = Arc::new(Notebook::new());
        let tool = NewContext::new(book.clone());
        assert!(tool.call(json!({})).await.is_err());
        assert!(book.take_reset().is_none());
        book.replace("n").unwrap();
        tool.call(json!({})).await.unwrap();
        assert!(book.take_reset().is_some());
    }
}
