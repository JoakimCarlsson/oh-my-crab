//! The `edit` tool: exact-match string replacement in a file.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Replaces one exact occurrence of a string in a file.
pub struct Edit {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl Edit {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of an `edit` call in replace mode.
#[derive(Deserialize)]
struct Args {
    /// File to edit, relative to the working directory.
    path: String,
    /// Text to find; it must occur exactly once.
    old_string: String,
    /// Text to put in its place.
    new_string: String,
}

#[async_trait]
impl Tool for Edit {
    /// Returns `edit`.
    fn name(&self) -> &'static str {
        "edit"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Edit a file by replacing old_string with new_string. old_string must match the file \
         exactly, whitespace included, and must occur exactly once; add surrounding lines to \
         make it unique. Fails without changing the file otherwise. The user is asked to \
         approve each edit."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File to edit, relative to the working directory." },
                "old_string": { "type": "string", "description": "Exact text to replace. Must match exactly once." },
                "new_string": { "type": "string", "description": "Replacement text." }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    /// Edits change files, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the file being edited and the size of the change.
    fn describe(&self, args: &Value) -> String {
        let path = args["path"].as_str().unwrap_or("?");
        let old = args["old_string"].as_str().unwrap_or_default();
        let new = args["new_string"].as_str().unwrap_or_default();
        format!("{path}\n- {}\n+ {}", preview(old), preview(new))
    }

    /// Performs the replacement on a blocking thread.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let path = self.sandbox.resolve(&args.path)?;
        let text = tokio::task::spawn_blocking(move || replace(&path, &args))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text))
    }
}

/// Returns the first line of `text`, shortened, for display.
fn preview(text: &str) -> String {
    let first = text.lines().next().unwrap_or_default();
    let more = if text.lines().count() > 1 { " …" } else { "" };
    match first.char_indices().nth(80) {
        Some((end, _)) => format!("{}…{more}", &first[..end]),
        None => format!("{first}{more}"),
    }
}

/// Replaces the single occurrence of `args.old_string` in the file at `path`.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the file is not text, `old_string` is
/// empty or identical to `new_string`, or it matches zero or several times.
fn replace(path: &std::path::Path, args: &Args) -> Result<String, ToolError> {
    if args.old_string.is_empty() {
        return Err(ToolError::Failed("old_string must not be empty".into()));
    }
    if args.old_string == args.new_string {
        return Err(ToolError::Failed(
            "old_string and new_string are identical; nothing to change".into(),
        ));
    }
    let content = std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::InvalidData => {
            ToolError::Failed(format!("{} is not a UTF-8 text file", args.path))
        }
        _ => ToolError::from(e),
    })?;
    match content.matches(&args.old_string).count() {
        0 => Err(ToolError::Failed(format!(
            "old_string was not found in {} (0 matches); read the file and copy the text exactly",
            args.path
        ))),
        1 => {
            write_atomic(
                path,
                content
                    .replacen(&args.old_string, &args.new_string, 1)
                    .as_bytes(),
            )?;
            Ok(format!("Edited {}: replaced 1 occurrence", args.path))
        }
        count => Err(ToolError::Failed(format!(
            "old_string matches {count} times in {}; include more surrounding context so it \
             matches exactly once",
            args.path
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds an `edit` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Edit, std::path::PathBuf) {
        let dir = scratch(name);
        (Edit::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Builds the arguments of an edit call.
    fn call(path: &str, old: &str, new: &str) -> Value {
        json!({ "path": path, "old_string": old, "new_string": new })
    }

    /// Zero matches is an error that leaves the file alone.
    #[tokio::test]
    async fn zero_matches_is_an_error() {
        let (tool, dir) = tool("edit-zero");
        std::fs::write(dir.join("f.txt"), "alpha\n").unwrap();
        let result = tool.call(call("f.txt", "beta", "gamma")).await;
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("0 matches")));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "alpha\n"
        );
    }

    /// Exactly one match is replaced and no temporary file is left behind.
    #[tokio::test]
    async fn one_match_is_replaced_atomically() {
        let (tool, dir) = tool("edit-one");
        std::fs::write(dir.join("f.txt"), "alpha\nbeta\n").unwrap();
        let out = tool.call(call("f.txt", "beta", "gamma")).await.unwrap();
        assert!(out.text.contains("replaced 1 occurrence"));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "alpha\ngamma\n"
        );
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("f.txt")]);
    }

    /// Several matches is an error that names the count and changes nothing.
    #[tokio::test]
    async fn many_matches_is_an_error() {
        let (tool, dir) = tool("edit-many");
        std::fs::write(dir.join("f.txt"), "x x x\n").unwrap();
        let result = tool.call(call("f.txt", "x", "y")).await;
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("matches 3 times")));
        assert_eq!(
            std::fs::read_to_string(dir.join("f.txt")).unwrap(),
            "x x x\n"
        );
    }

    /// Escaping paths, empty and no-op edits are rejected.
    #[tokio::test]
    async fn rejects_escapes_and_degenerate_edits() {
        let (tool, dir) = tool("edit-reject");
        std::fs::write(dir.join("f.txt"), "a\n").unwrap();
        let escape = tool.call(call("../outside.txt", "a", "b")).await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
        assert!(tool.call(call("f.txt", "", "b")).await.is_err());
        assert!(tool.call(call("f.txt", "a", "a")).await.is_err());
    }

    /// File permissions survive an edit.
    #[tokio::test]
    async fn keeps_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (tool, dir) = tool("edit-mode");
        let path = dir.join("run.sh");
        std::fs::write(&path, "echo a\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        tool.call(call("run.sh", "a", "b")).await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
}
