//! The `write` tool: create or overwrite a file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use super::snapshot::tag;
use crate::language::{Languages, Written, settle};
use crate::tool::{marked_lines, parse_args};
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Creates a file or replaces its whole content.
pub struct Write {
    /// The directory paths are confined to.
    sandbox: Sandbox,
    /// The language service that formats the written file and says what is wrong with it.
    languages: Languages,
}

impl Write {
    /// Creates the tool confined to `sandbox`, with no language service.
    pub fn new(sandbox: Sandbox) -> Self {
        Self::with_languages(sandbox, Languages::default())
    }

    /// Creates the tool confined to `sandbox`, which has the files it writes formatted by `languages` and reports what it says is wrong with them.
    pub fn with_languages(sandbox: Sandbox, languages: Languages) -> Self {
        Self { sandbox, languages }
    }
}

/// Arguments of a `write` call.
#[derive(Deserialize)]
struct Args {
    /// File to write, relative to the working directory.
    path: String,
    /// The complete new content.
    content: String,
}

#[async_trait]
impl Tool for Write {
    /// Returns `write`.
    fn name(&self) -> &'static str {
        "write"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Create a file, or overwrite it completely, with the given content. Missing parent \
         directories are created. Prefer edit for changing part of an existing file. The user \
         is asked to approve each write."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to write, relative to the working directory." },
                "content": { "type": "string", "description": "The complete file content." }
            },
            "required": ["path", "content"]
        })
    }

    /// Writes change files, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the file, the size of the content and all of it.
    fn describe(&self, args: &Value) -> String {
        let path = self.sandbox.describe(args["path"].as_str().unwrap_or("?"));
        let content = args["content"].as_str().unwrap_or_default();
        format!(
            "{path} ({} lines, {} bytes)\n{}",
            content.lines().count(),
            content.len(),
            marked_lines('+', content)
        )
    }

    /// Writes the file on a blocking thread, then has the language service format it and say what is wrong with it.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let path = self.sandbox.resolve(&args.path)?;
        let mut file = Written {
            path: path.clone(),
            shown: args.path.clone(),
            text: args.content.clone(),
            formatted: false,
        };
        let mut text = tokio::task::spawn_blocking(move || write(&path, &args))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        let diagnostics = settle(&self.languages, std::slice::from_mut(&mut file)).await;
        if file.formatted {
            text.push_str(&format!(
                "; formatted by the language server to {} bytes, tag {}",
                file.text.len(),
                tag(file.text.as_bytes())
            ));
        }
        if !diagnostics.is_empty() {
            text.push_str("\n\n");
            text.push_str(&diagnostics);
        }
        Ok(ToolOutput::new(text))
    }
}

/// Writes `args.content` to `path`, creating missing parent directories.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when `path` is a directory and
/// [`ToolError::Io`] when the write fails.
fn write(path: &std::path::Path, args: &Args) -> Result<String, ToolError> {
    if path.is_dir() {
        return Err(ToolError::Failed(format!(
            "{} is a directory, not a file",
            args.path
        )));
    }
    let existed = path.exists();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    write_atomic(path, args.content.as_bytes())?;
    let verb = if existed { "Overwrote" } else { "Created" };
    Ok(format!(
        "{verb} {} ({} bytes)",
        args.path,
        args.content.len()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds a `write` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Write, std::path::PathBuf) {
        let dir = scratch(name);
        (Write::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// A new file is created with its parent directories, then overwritten.
    #[tokio::test]
    async fn creates_then_overwrites() {
        let (tool, dir) = tool("write-basic");
        let created = tool
            .call(json!({ "path": "a/b/c.txt", "content": "one" }))
            .await
            .unwrap();
        assert!(created.text.starts_with("Created"));
        let overwritten = tool
            .call(json!({ "path": "a/b/c.txt", "content": "two" }))
            .await
            .unwrap();
        assert!(overwritten.text.starts_with("Overwrote"));
        assert_eq!(
            std::fs::read_to_string(dir.join("a/b/c.txt")).unwrap(),
            "two"
        );
    }

    /// Paths outside the working directory and directories are rejected.
    #[tokio::test]
    async fn rejects_escapes_and_directories() {
        let (tool, dir) = tool("write-reject");
        std::fs::create_dir(dir.join("d")).unwrap();
        let escape = tool
            .call(json!({ "path": "../outside.txt", "content": "x" }))
            .await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
        let directory = tool.call(json!({ "path": "d", "content": "x" })).await;
        assert!(matches!(directory, Err(ToolError::Failed(_))));
    }

    /// A dangling symlink pointing outside cannot be used to write outside.
    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_writing_through_a_dangling_symlink() {
        let (tool, dir) = tool("write-dangling");
        let outside = scratch("write-dangling-target");
        std::os::unix::fs::symlink(outside.join("new.txt"), dir.join("link")).unwrap();
        let result = tool
            .call(json!({ "path": "link", "content": "escaped" }))
            .await;
        assert!(matches!(result, Err(ToolError::OutsidePath(_))));
        assert!(!outside.join("new.txt").exists());
    }

    /// A language service formats the written file, and the result gives the tag of what is on disk and what is wrong with it; without one, the result is as it always was.
    #[tokio::test]
    async fn formats_and_reports_diagnostics() {
        use crate::language::tests::{Fake, diagnostic};
        use crate::{Languages, Severity};
        let dir = scratch("write-languages");
        let fake = Fake {
            formats: true,
            ..Fake::default()
        };
        fake.diagnostics.lock().unwrap().push(diagnostic(
            &dir.join("m.rs"),
            1,
            Severity::Warning,
            "unused",
        ));
        let tool = Write::with_languages(
            Sandbox::new(&dir).unwrap(),
            Languages::new(Some(std::sync::Arc::new(fake))),
        );
        let out = tool
            .call(json!({ "path": "m.rs", "content": "fn x() {}\n" }))
            .await
            .unwrap();
        let on_disk = std::fs::read_to_string(dir.join("m.rs")).unwrap();
        assert_eq!(on_disk, "FN X() {}\n");
        assert_eq!(
            out.text,
            format!(
                "Created m.rs (10 bytes); formatted by the language server to 10 bytes, tag {}\n\nDiagnostics: 0 error(s), 1 warning(s)\nm.rs:1:5 warning: unused (rustc)",
                tag(on_disk.as_bytes())
            )
        );
        let (plain, _) = tool_without_languages(&dir);
        let out = plain
            .call(json!({ "path": "m.rs", "content": "fn x() {}\n" }))
            .await
            .unwrap();
        assert_eq!(out.text, "Overwrote m.rs (10 bytes)");
    }

    /// A `write` tool rooted at `dir` with no language service.
    fn tool_without_languages(dir: &std::path::Path) -> (Write, std::path::PathBuf) {
        (Write::new(Sandbox::new(dir).unwrap()), dir.to_path_buf())
    }
}
