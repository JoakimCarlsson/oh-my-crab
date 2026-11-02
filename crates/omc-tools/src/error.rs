//! The error type tools fail with.

use thiserror::Error;

/// Everything that can go wrong while a tool runs.
///
/// Errors are sent back to the model as the tool result, so every message is
/// written to be read by it.
#[derive(Debug, Error)]
pub enum ToolError {
    /// The arguments did not match the tool's schema.
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    /// A path resolves outside the working directory.
    #[error("path {0} is outside the working directory")]
    OutsidePath(String),
    /// A filesystem or process operation failed.
    #[error("{0}")]
    Io(String),
    /// The tool understood the request but could not carry it out.
    #[error("{0}")]
    Failed(String),
    /// No tool is registered under the requested name.
    #[error("unknown tool: {0}")]
    Unknown(String),
}

impl From<std::io::Error> for ToolError {
    /// Wraps an I/O error with its description.
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}
