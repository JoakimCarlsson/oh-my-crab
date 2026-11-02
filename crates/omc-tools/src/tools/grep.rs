//! The `grep` tool: regex search over files, honoring `.gitignore`.

use std::io;
use std::path::Path;

use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkMatch};
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Matches returned when the caller gives no limit.
const DEFAULT_LIMIT: usize = 100;

/// Largest limit a caller may ask for.
const MAX_LIMIT: usize = 500;

/// Longest matched line kept, in characters.
const MAX_LINE_CHARS: usize = 500;

/// Searches file contents with a regular expression.
pub struct Grep {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl Grep {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of a `grep` call.
#[derive(Deserialize)]
struct Args {
    /// Regular expression to look for.
    pattern: String,
    /// File or directory to search; defaults to the working directory.
    path: Option<String>,
    /// Glob that file paths must match, such as `*.rs`.
    glob: Option<String>,
    /// Whether to ignore case.
    case_insensitive: Option<bool>,
    /// Most matches to return.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for Grep {
    /// Returns `grep`.
    fn name(&self) -> &'static str {
        "grep"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Search file contents with a regular expression. Honors .gitignore and skips binary \
         files. Returns matches as path:line:text, capped at the limit."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Regular expression (Rust regex syntax)." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the working directory." },
                "glob": { "type": "string", "description": "Only search files whose path matches this glob, e.g. \"*.rs\" or \"src/**/*.ts\"." },
                "case_insensitive": { "type": "boolean", "description": "Ignore case. Defaults to false." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum matches. Defaults to 100, at most 500." }
            },
            "required": ["pattern"]
        })
    }

    /// Shows the pattern and the searched path.
    fn describe(&self, args: &Value) -> String {
        let pattern = args["pattern"].as_str().unwrap_or("?");
        match args["path"].as_str() {
            Some(path) => format!("{pattern} in {path}"),
            None => pattern.to_string(),
        }
    }

    /// Runs the search on a blocking thread.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let target = self.sandbox.resolve(args.path.as_deref().unwrap_or("."))?;
        if !target.exists() {
            return Err(ToolError::Failed(format!(
                "{} does not exist",
                args.path.as_deref().unwrap_or(".")
            )));
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let sandbox = self.sandbox.clone();
        let text = tokio::task::spawn_blocking(move || search(&sandbox, &target, &args, limit))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text))
    }
}

/// Collects matches of one file into a shared list, stopping at the limit.
struct Collector<'a> {
    /// Display path of the file being searched.
    path: String,
    /// Matches gathered so far, across files.
    found: &'a mut Vec<String>,
    /// Most matches to gather.
    limit: usize,
}

impl Sink for Collector<'_> {
    type Error = io::Error;

    /// Records the match as `path:line:text` and asks to stop once full.
    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let number = mat.line_number().unwrap_or(0);
        let text = String::from_utf8_lossy(mat.bytes());
        let text = clip(text.trim_end_matches(['\n', '\r']));
        self.found.push(format!("{}:{number}:{text}", self.path));
        Ok(self.found.len() < self.limit)
    }
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_string(),
    }
}

/// Walks `target` and searches every file, returning the formatted result.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a bad regex or glob.
fn search(
    sandbox: &Sandbox,
    target: &Path,
    args: &Args,
    limit: usize,
) -> Result<String, ToolError> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(args.case_insensitive.unwrap_or(false))
        .build(&args.pattern)
        .map_err(|e| ToolError::InvalidArguments(format!("invalid regex: {e}")))?;
    let mut walker = WalkBuilder::new(target);
    walker.require_git(false).sort_by_file_path(|a, b| a.cmp(b));
    if let Some(glob) = &args.glob {
        let overrides = OverrideBuilder::new(target)
            .add(glob)
            .and_then(|builder| builder.build())
            .map_err(|e| ToolError::InvalidArguments(format!("invalid glob: {e}")))?;
        walker.overrides(overrides);
    }
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .build();
    let mut found = Vec::new();
    for entry in walker.build().flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let sink = Collector {
            path: sandbox.display(entry.path()),
            found: &mut found,
            limit,
        };
        let _ = searcher.search_path(&matcher, entry.path(), sink);
        if found.len() >= limit {
            break;
        }
    }
    if found.is_empty() {
        return Ok("No matches found".to_string());
    }
    let mut text = found.join("\n");
    if found.len() >= limit {
        text.push_str(&format!(
            "\n[results truncated at {limit} matches; narrow the pattern, path or glob]"
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds a `grep` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Grep, std::path::PathBuf) {
        let dir = scratch(name);
        (Grep::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Matches come back as path:line:text and the glob filters files.
    #[tokio::test]
    async fn finds_matches_with_paths_and_lines() {
        let (tool, dir) = tool("grep-basic");
        std::fs::write(dir.join("a.rs"), "fn main() {}\nlet needle = 1;\n").unwrap();
        std::fs::write(dir.join("b.txt"), "needle here\n").unwrap();
        let all = tool.call(json!({ "pattern": "needle" })).await.unwrap();
        assert_eq!(all.text, "a.rs:2:let needle = 1;\nb.txt:1:needle here");
        let only_rs = tool
            .call(json!({ "pattern": "needle", "glob": "*.rs" }))
            .await
            .unwrap();
        assert_eq!(only_rs.text, "a.rs:2:let needle = 1;");
    }

    /// The result is capped at the limit and says so.
    #[tokio::test]
    async fn caps_results_at_the_limit() {
        let (tool, dir) = tool("grep-limit");
        std::fs::write(dir.join("many.txt"), "hit\n".repeat(50)).unwrap();
        let out = tool
            .call(json!({ "pattern": "hit", "limit": 5 }))
            .await
            .unwrap();
        assert_eq!(
            out.text
                .lines()
                .filter(|l| l.starts_with("many.txt:"))
                .count(),
            5
        );
        assert!(out.text.contains("truncated at 5 matches"));
    }

    /// Files named in `.gitignore` and binary files are skipped.
    #[tokio::test]
    async fn skips_ignored_and_binary_files() {
        let (tool, dir) = tool("grep-ignore");
        std::fs::write(dir.join(".gitignore"), "secret.txt\n").unwrap();
        std::fs::write(dir.join("secret.txt"), "needle\n").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0\x01\x02").unwrap();
        std::fs::write(dir.join("ok.txt"), "needle\n").unwrap();
        let out = tool.call(json!({ "pattern": "needle" })).await.unwrap();
        assert_eq!(out.text, "ok.txt:1:needle");
    }

    /// No match, a bad regex and an escaping path are reported plainly.
    #[tokio::test]
    async fn reports_empty_invalid_and_escaping_searches() {
        let (tool, dir) = tool("grep-errors");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let none = tool.call(json!({ "pattern": "zzz" })).await.unwrap();
        assert_eq!(none.text, "No matches found");
        let bad = tool.call(json!({ "pattern": "(" })).await;
        assert!(matches!(bad, Err(ToolError::InvalidArguments(_))));
        let escape = tool.call(json!({ "pattern": "x", "path": ".." })).await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
    }
}
