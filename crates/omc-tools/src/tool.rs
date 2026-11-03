//! The [`Tool`] trait every built-in tool implements.

use async_trait::async_trait;
use serde_json::Value;

use unicode_width::UnicodeWidthChar;

use crate::ToolError;

/// How one file changed in a tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// The file, as shown to the user.
    pub path: String,
    /// What the file held before.
    pub old: String,
    /// What it holds after.
    pub new: String,
}

/// What a successful tool call returns to the model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    /// The text the model reads as the tool result.
    pub text: String,
    /// The files the call changed, for the client to show; empty for most tools.
    pub changes: Vec<FileChange>,
}

impl ToolOutput {
    /// Wraps `text` as a tool output that changed no files.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            changes: Vec::new(),
        }
    }

    /// Records the files the call changed.
    pub fn with_changes(mut self, changes: Vec<FileChange>) -> Self {
        self.changes = changes;
        self
    }
}

/// A capability the model can invoke by name with JSON arguments.
#[async_trait]
pub trait Tool: Send + Sync {
    /// The name the model calls the tool by.
    fn name(&self) -> &'static str;

    /// What the tool does, written for the model.
    fn description(&self) -> &str;

    /// The JSON schema of the arguments object.
    fn schema(&self) -> Value;

    /// Whether the tool only looks and never changes anything, so that plan mode and read-only agents may use it.
    fn read_only(&self) -> bool {
        false
    }

    /// Whether the user must approve a call before it runs.
    fn needs_approval(&self) -> bool {
        false
    }

    /// Returns a one-line description of a call with `args`, shown to the user.
    fn describe(&self, args: &Value) -> String {
        args.to_string()
    }

    /// A copy of the tool that cannot start background runs, for a tool that can; nothing for the rest.
    fn foreground(&self) -> Option<std::sync::Arc<dyn Tool>> {
        None
    }

    /// Runs the tool with `args`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError`] when the arguments are invalid or the operation
    /// fails; the caller reports it to the model instead of aborting.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError>;

    /// Runs the tool for the model's call numbered `id`, for tools that report on their own work under that id.
    ///
    /// # Errors
    ///
    /// Same as [`Tool::call`].
    async fn call_as(&self, _id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        self.call(args).await
    }
}

/// Deserializes `args` into the tool's argument struct.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when `args` does not fit `T`.
pub fn parse_args<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args).map_err(|e| ToolError::InvalidArguments(e.to_string()))
}

/// Replaces control characters in `text` with visible escapes, keeping newlines.
///
/// Used on anything shown in the approval prompt so a tool argument cannot
/// smuggle terminal escape sequences into the interface.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\n' => "\n".to_string(),
            '\t' => "    ".to_string(),
            c if c.is_control() || is_invisible(c) => c.escape_unicode().to_string(),
            c => c.to_string(),
        })
        .collect()
}

/// Returns whether `c` could render as nothing, reorder text or masquerade as
/// a space.
///
/// Every non-ASCII character that measures zero columns wide is covered, which
/// takes in format characters, bidi controls, variation selectors and combining
/// marks. Non-space whitespace, the bidi and tag ranges and the Hangul fillers,
/// which some terminals draw as blanks, are listed explicitly.
fn is_invisible(c: char) -> bool {
    if c.is_ascii() {
        return false;
    }
    c.width() == Some(0)
        || (c.is_whitespace() && c != ' ')
        || matches!(c,
            '\u{ad}' | '\u{61c}' | '\u{115f}' | '\u{1160}' | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}' | '\u{feff}' | '\u{ffa0}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e0fff}')
}

/// Formats `text` as diff lines, each starting with `marker`, so every line of
/// a change is visible.
pub fn marked_lines(marker: char, text: &str) -> String {
    text.lines()
        .map(|line| format!("{marker} {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Escape characters are made visible and newlines survive.
    #[test]
    fn sanitize_escapes_control_characters() {
        assert_eq!(sanitize("a\u{1b}[31mb\nc"), "a\\u{1b}[31mb\nc");
    }

    /// Bidi and zero-width characters are escaped.
    #[test]
    fn sanitize_escapes_invisible_characters() {
        assert_eq!(sanitize("a\u{202e}b\u{200b}c"), "a\\u{202e}b\\u{200b}c");
    }

    /// Variation selectors, combining marks and Hangul fillers are escaped.
    #[test]
    fn sanitize_escapes_zero_width_and_filler_characters() {
        assert_eq!(sanitize("a\u{fe0f}"), "a\\u{fe0f}");
        assert_eq!(sanitize("e\u{301}"), "e\\u{301}");
        assert_eq!(sanitize("\u{3164}"), "\\u{3164}");
        assert_eq!(sanitize("plain é 日本"), "plain é 日本");
    }

    /// Every line gets the marker.
    #[test]
    fn marked_lines_prefixes_each_line() {
        assert_eq!(marked_lines('+', "a\nb"), "+ a\n+ b");
    }
}
