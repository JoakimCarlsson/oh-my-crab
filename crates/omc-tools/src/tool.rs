//! The [`Tool`] trait every built-in tool implements.

use async_trait::async_trait;
use serde_json::Value;

use crate::ToolError;

/// What a successful tool call returns to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// The text the model reads as the tool result.
    pub text: String,
}

impl ToolOutput {
    /// Wraps `text` as a tool output.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// A capability the model can invoke by name with JSON arguments.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls the tool by.
    fn name(&self) -> &'static str;

    /// What the tool does, written for the model.
    fn description(&self) -> &'static str;

    /// The JSON schema of the arguments object.
    fn schema(&self) -> Value;

    /// Whether the user must approve a call before it runs.
    fn needs_approval(&self) -> bool {
        false
    }

    /// Returns a one-line description of a call with `args`, shown to the user.
    fn describe(&self, args: &Value) -> String {
        args.to_string()
    }

    /// Runs the tool with `args`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] when the arguments are invalid or the operation
    /// fails; the caller reports it to the model instead of aborting.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError>;
}

/// Deserializes `args` into the tool's argument struct.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when `args` does not fit `T`.
pub fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))
}
