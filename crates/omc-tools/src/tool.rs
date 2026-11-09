//! The [`Tool`] trait every built-in tool implements.

use async_trait::async_trait;
use serde_json::Value;

use unicode_width::UnicodeWidthChar;

use crate::{Registry, ToolError};

/// How one file changed in a tool call.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileChange {
    /// The file, as shown to the user.
    pub path: String,
    /// What the file held before.
    pub old: String,
    /// What it holds after.
    pub new: String,
}

/// An image a tool hands the model to look at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// The MIME type, such as `image/png`.
    pub media_type: String,
    /// The image's bytes, base64-encoded.
    pub data: String,
}

/// What a successful tool call returns to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// The text the model reads as the tool result.
    pub text: String,
    /// The files the call changed, for the client to show; empty for most tools.
    pub changes: Vec<FileChange>,
    /// Images the model sees after the text; empty for most tools.
    pub images: Vec<Image>,
    /// What the text says, as a value a program can work with, for the tools whose output has a shape of its own; the model reads the text, and a script gets this instead.
    pub data: Option<Value>,
}

impl ToolOutput {
    /// Wraps `text` as a tool output that changed no files and holds no images.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            changes: Vec::new(),
            images: Vec::new(),
            data: None,
        }
    }

    /// Attaches `data`, what the text says as a value a program can work with.
    pub fn with_data(mut self, data: Value) -> Self {
        self.data = Some(data);
        self
    }

    /// Attaches `images` for the model to look at.
    pub fn with_images(mut self, images: Vec<Image>) -> Self {
        self.images = images;
        self
    }

    /// Records the files the call changed.
    pub fn with_changes(mut self, changes: Vec<FileChange>) -> Self {
        self.changes = changes;
        self
    }
}

/// What permission rules look at in a call, beyond the name of its tool.
///
/// Rules such as `Bash(cargo test:*)` or `Edit(src/**)` are matched against
/// this: a command line, the files a call touches, the folder it works through
/// or the site it reaches. A tool whose calls differ in none of these has
/// [`Subject::Whole`], and only rules about the whole tool apply to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Subject {
    /// Nothing finer than the tool itself.
    #[default]
    Whole,
    /// A shell command line, as it is run.
    Command(String),
    /// The files the call reads or changes, as absolute paths.
    Files(Vec<std::path::PathBuf>),
    /// A file or a folder the call works through, everything below it included, as an absolute path.
    Tree(std::path::PathBuf),
    /// The host name of the site the call reaches, lowercased.
    Site(String),
}

/// Whether a call may run at the same time as other calls of the same model reply.
///
/// The calls of one reply run in the model's order, except that a run of
/// shared calls one after another runs at once; an exclusive call waits for
/// everything before it and holds back everything after it, so that what calls
/// change happens in the order the model asked for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Concurrency {
    /// The call only looks, and may overlap the shared calls next to it.
    Shared,
    /// The call changes something or has an effect beyond its result, and runs alone.
    #[default]
    Exclusive,
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

    /// Whether a call with `args` may overlap other calls of the same reply.
    ///
    /// By default a tool that is [`Tool::read_only`] is [`Concurrency::Shared`]
    /// and every other tool is [`Concurrency::Exclusive`]. A tool that only
    /// looks but steers the conversation or talks to the reader says it is
    /// exclusive, and a tool that changes something in some calls only says
    /// which calls those are.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        if self.read_only() {
            Concurrency::Shared
        } else {
            Concurrency::Exclusive
        }
    }

    /// Whether the tool checks its arguments against its schema itself, so that the [`crate::Registry`] only puts them right before a call and leaves turning them away to the tool.
    fn checks_arguments(&self) -> bool {
        false
    }

    /// Whether the user must approve a call before it runs.
    fn needs_approval(&self) -> bool {
        false
    }

    /// What the user approves when they approve the call with `args`, or
    /// nothing when the call needs no approval.
    ///
    /// An "always allow" answer is remembered under this scope, so it covers
    /// later calls with the same scope and no others. By default a tool that
    /// [`Tool::needs_approval`] has one scope, its own name; a tool where calls
    /// differ in what they reach, or where only some calls need approval,
    /// returns a narrower scope per call.
    fn approval_scope(&self, _args: &Value) -> Option<String> {
        self.needs_approval().then(|| self.name().to_owned())
    }

    /// What permission rules look at in a call with `args`: its command line, its files, its folder or its site.
    ///
    /// By default [`Subject::Whole`], so that only rules about the whole tool apply.
    fn subject(&self, _args: &Value) -> Subject {
        Subject::Whole
    }

    /// Returns a one-line description of a call with `args`, shown to the user.
    fn describe(&self, args: &Value) -> String {
        args.to_string()
    }

    /// A copy of the tool that cannot start background runs, for a tool that can; nothing for the rest.
    fn foreground(&self) -> Option<std::sync::Arc<dyn Tool>> {
        None
    }

    /// A copy of the tool that only looks, refusing the calls that would change something, for a tool that is not [`Tool::read_only`] although most of its calls only look; nothing for the rest.
    ///
    /// [`crate::Registry::read_only`] keeps this copy, so that plan mode and read-only agents can still make the calls that only look.
    fn looking(&self) -> Option<std::sync::Arc<dyn Tool>> {
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

    /// Runs the tool for the model's call numbered `id`, for tools that run other tools, which they reach through `caller` so that those calls are checked, asked about and reported like the model's own; every other tool ignores `caller`.
    ///
    /// # Errors
    ///
    /// Same as [`Tool::call`].
    async fn call_through(
        &self,
        id: &str,
        args: Value,
        _caller: &dyn Caller,
    ) -> Result<ToolOutput, ToolError> {
        self.call_as(id, args).await
    }

    /// A copy of the tool made for an agent whose tools are `tools`, for a tool that depends on what else the agent has; nothing for the rest.
    fn fitted(&self, _tools: &Registry) -> Option<std::sync::Arc<dyn Tool>> {
        None
    }

    /// The shape of [`ToolOutput::data`], written as a TypeScript type, for a tool whose successful calls carry it; nothing for a tool whose output is only text.
    fn data_shape(&self) -> Option<&'static str> {
        None
    }
}

/// How a tool that runs other tools reaches them: the way the agent runs the model's own calls, with the same argument checks, hooks, questions to the reader and reports.
#[async_trait]
pub trait Caller: Send + Sync {
    /// Whether there is a tool called `name` to call.
    fn has(&self, name: &str) -> bool;

    /// Runs the tool called `name` with `args`, as a call nested under the one this caller was made for.
    ///
    /// # Errors
    ///
    /// Returns the text the model would read for a call that did not succeed: unknown, with arguments that cannot be put right, blocked by a hook, denied by the reader, or failed.
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, String>;

    /// How long the calls made so far have waited for the reader to answer whether they may run, a question still open included; a tool with a time limit leaves that time out of it.
    fn waited(&self) -> std::time::Duration {
        std::time::Duration::ZERO
    }
}

/// A [`Caller`] that has no tools, for running a tool outside the agent loop.
pub struct NoCaller;

#[async_trait]
impl Caller for NoCaller {
    /// Has no tool.
    fn has(&self, _name: &str) -> bool {
        false
    }

    /// Fails, since there is no tool to call.
    async fn call(&self, name: &str, _args: Value) -> Result<ToolOutput, String> {
        Err(format!("unknown tool: {name}"))
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
