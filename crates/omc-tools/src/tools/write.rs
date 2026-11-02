//! The `write` tool: create or overwrite a file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use crate::tool::{marked_lines, parse_args};
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Creates a file or replaces its whole content.
pub struct Write {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl Write {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
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

    /// Writes the file on a blocking thread.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let path = self.sandbox.resolve(&args.path)?;
        let text = tokio::task::spawn_blocking(move || write(&path, &args))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
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
}
