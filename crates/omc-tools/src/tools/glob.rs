//! The `glob` tool: find files by path pattern, newest first.

use std::path::Path;
use std::time::SystemTime;

use async_trait::async_trait;
use globset::GlobBuilder;
use ignore::WalkBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Paths returned when the caller gives no limit.
const DEFAULT_LIMIT: usize = 200;

/// Largest limit a caller may ask for.
const MAX_LIMIT: usize = 1000;

/// Finds files whose path matches a glob.
pub struct Glob {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl Glob {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of a `glob` call.
#[derive(Deserialize)]
struct Args {
    /// Glob pattern, such as `**/*.rs`.
    pattern: String,
    /// Directory to search; defaults to the working directory.
    path: Option<String>,
    /// Most paths to return.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Glob {
    /// Returns `glob`.
    fn name(&self) -> &'static str {
        "glob"
    }

    /// Reading changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Find files by glob pattern. Honors .gitignore. A pattern without a slash, such as \
         \"*.rs\", matches at any depth. Results are sorted by modification time, newest first."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Glob pattern, e.g. \"**/*.rs\" or \"src/*.toml\"." },
                "path": { "type": "string", "description": "Directory to search. Defaults to the working directory." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum paths. Defaults to 200, at most 1000." }
            },
            "required": ["pattern"]
        })
    }

    /// Shows the pattern and the searched directory.
    fn describe(&self, args: &Value) -> String {
        let pattern = args["pattern"].as_str().unwrap_or("?");
        match args["path"].as_str() {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.to_string(),
        }
    }

    /// The paths as a list, and whether the limit cut it.
    fn data_shape(&self) -> Option<&'static str> {
        Some("{ paths: string[]; truncated: boolean }")
    }

    /// Runs the walk on a blocking thread.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let target = self.sandbox.resolve(args.path.as_deref().unwrap_or("."))?;
        if !target.is_dir() {
            return Err(ToolError::Failed(format!(
                "{} is not a directory",
                args.path.as_deref().unwrap_or(".")
            )));
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let sandbox = self.sandbox.clone();
        let (text, data) =
            tokio::task::spawn_blocking(move || find(&sandbox, &target, &args.pattern, limit))
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text).with_data(data))
    }
}

/// Walks `target` and lists files matching `pattern`, newest first, as text and as data.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when `pattern` is not a valid glob.
fn find(
    sandbox: &Sandbox,
    target: &Path,
    pattern: &str,
    limit: usize,
) -> Result<(String, Value), ToolError> {
    let pattern = if pattern.contains('/') {
        pattern.trim_start_matches("./").to_string()
    } else {
        format!("**/{pattern}")
    };
    let matcher = GlobBuilder::new(&pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| ToolError::InvalidArguments(format!("invalid glob: {e}")))?
        .compile_matcher();
    let mut hits: Vec<(SystemTime, String)> = WalkBuilder::new(target)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| entry.file_name() != ".git")
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| {
            entry
                .path()
                .strip_prefix(target)
                .is_ok_and(|relative| matcher.is_match(relative))
        })
        .map(|entry| {
            let modified = entry
                .metadata()
                .ok()
                .and_then(|meta| meta.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            (modified, sandbox.display(entry.path()))
        })
        .collect();
    if hits.is_empty() {
        let data = json!({ "paths": [], "truncated": false });
        return Ok(("No files found matching pattern".to_string(), data));
    }
    hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = hits.len();
    let paths: Vec<String> = hits.into_iter().take(limit).map(|(_, path)| path).collect();
    let data = json!({ "paths": paths, "truncated": total > limit });
    let mut text = paths.join("\n");
    if total > limit {
        text.push_str(&format!(
            "\n[showing {limit} of {total} matches; narrow the pattern or path]"
        ));
    }
    Ok((text, data))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::testutil::scratch;

    /// Builds a `glob` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Glob, std::path::PathBuf) {
        let dir = scratch(name);
        (Glob::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Sets the modification time of `path` to `seconds` after the epoch.
    fn touch(path: &Path, seconds: u64) {
        let file = std::fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
            .unwrap();
    }

    /// Matches are sorted newest first and bare patterns match at any depth.
    #[tokio::test]
    async fn sorts_by_modification_time() {
        let (tool, dir) = tool("glob-sort");
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("old.rs"), "").unwrap();
        std::fs::write(dir.join("sub/new.rs"), "").unwrap();
        std::fs::write(dir.join("skip.txt"), "").unwrap();
        touch(&dir.join("old.rs"), 1000);
        touch(&dir.join("sub/new.rs"), 2000);
        let out = tool.call(json!({ "pattern": "*.rs" })).await.unwrap();
        assert_eq!(out.text, "sub/new.rs\nold.rs");
    }

    /// Slash patterns are anchored and `.gitignore` is honored.
    #[tokio::test]
    async fn anchors_slash_patterns_and_honors_gitignore() {
        let (tool, dir) = tool("glob-anchor");
        std::fs::create_dir(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.join("top.rs"), "").unwrap();
        std::fs::write(dir.join("gen.rs"), "").unwrap();
        std::fs::write(dir.join(".gitignore"), "gen.rs\n").unwrap();
        let out = tool.call(json!({ "pattern": "src/*.rs" })).await.unwrap();
        assert_eq!(out.text, "src/a.rs");
        let all = tool.call(json!({ "pattern": "**/*.rs" })).await.unwrap();
        assert!(!all.text.contains("gen.rs"));
    }

    /// Results are capped, empty scans say so, and escaping paths are rejected.
    #[tokio::test]
    async fn caps_results_and_rejects_escapes() {
        let (tool, dir) = tool("glob-cap");
        for i in 0..10 {
            std::fs::write(dir.join(format!("f{i}.txt")), "").unwrap();
        }
        let out = tool
            .call(json!({ "pattern": "*.txt", "limit": 3 }))
            .await
            .unwrap();
        assert_eq!(out.text.lines().count(), 4);
        assert!(out.text.contains("showing 3 of 10"));
        let none = tool.call(json!({ "pattern": "*.zip" })).await.unwrap();
        assert_eq!(none.text, "No files found matching pattern");
        let escape = tool.call(json!({ "pattern": "*", "path": ".." })).await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
    }
}
