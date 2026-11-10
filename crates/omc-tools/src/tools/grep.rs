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

use super::read::{Named, named_functions};
use super::snapshot::tag_file;
use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Matches returned when the caller gives no limit.
const DEFAULT_LIMIT: usize = 100;

/// Largest limit a caller may ask for.
const MAX_LIMIT: usize = 500;

/// The most matches a result may hold for each match in a function to be given the function's name; a longer result is a survey, where the names would cost more than they tell.
const NAMED_LIMIT: usize = 20;

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

    /// Reading changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Search file contents with a regular expression. Honors .gitignore and skips binary \
         files. Returns matches as path:line:text, capped at the limit, followed by the snapshot \
         tag of each file with matches; pass a tag to edit. In a short result, a match inside a \
         function of a code file names it before the text, as path:line in name:text."
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

    /// The matches as a list, and whether the limit cut it.
    fn data_shape(&self) -> Option<&'static str> {
        Some(
            "{ matches: { path: string; line: number; text: string; within?: string }[]; truncated: boolean }",
        )
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
        let (text, data) =
            tokio::task::spawn_blocking(move || search(&sandbox, &target, &args, limit))
                .await
                .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text).with_data(data))
    }
}

/// One matching line.
struct Match {
    /// Display path of the file.
    path: String,
    /// The line number, counted from one.
    line: u64,
    /// The line, cut to [`MAX_LINE_CHARS`] characters.
    text: String,
    /// The name of the function the line is in, when the result is short enough to say and the line is in one.
    within: Option<String>,
}

impl Match {
    /// The match as `path:line:text`, with the function it is in after the line number when that is known, so that the text stays the line as it is to its end.
    fn shown(&self) -> String {
        match &self.within {
            Some(name) => format!("{}:{} in {name}:{}", self.path, self.line, self.text),
            None => format!("{}:{}:{}", self.path, self.line, self.text),
        }
    }

    /// The match as a JSON object with `path`, `line` and `text`, and `within` when the function it is in is known.
    fn value(&self) -> Value {
        match &self.within {
            Some(name) => {
                json!({ "path": self.path, "line": self.line, "text": self.text, "within": name })
            }
            None => json!({ "path": self.path, "line": self.line, "text": self.text }),
        }
    }
}

/// Collects matches of one file into a shared list, stopping at the limit.
struct Collector<'a> {
    /// Display path of the file being searched.
    path: String,
    /// Matches gathered so far, across files.
    found: &'a mut Vec<Match>,
    /// Most matches to gather.
    limit: usize,
}

impl Sink for Collector<'_> {
    type Error = io::Error;

    /// Records the match and asks to stop once full.
    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, io::Error> {
        let text = String::from_utf8_lossy(mat.bytes());
        self.found.push(Match {
            path: self.path.clone(),
            line: mat.line_number().unwrap_or(0),
            text: clip(text.trim_end_matches(['\n', '\r'])),
            within: None,
        });
        Ok(self.found.len() < self.limit)
    }
}

/// Gives each match in `found` the name of the function its line is in, when its file is code and the line is inside a named function but is not the line the function starts on. `files` lists the files with matches, each with the place in `found` its matches start at.
fn name_functions(found: &mut [Match], files: &[(std::path::PathBuf, usize)]) {
    for (index, (path, start)) in files.iter().enumerate() {
        let end = files.get(index + 1).map_or(found.len(), |(_, next)| *next);
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let functions = named_functions(path, &text);
        for found in &mut found[*start..end] {
            found.within =
                innermost(&functions, found.line as usize).map(|named| named.name.clone());
        }
    }
}

/// The function in `functions` that holds `line` most closely, the one that starts last, unless `line` is the line it starts on, which names it already.
fn innermost(functions: &[Named], line: usize) -> Option<&Named> {
    functions
        .iter()
        .rfind(|named| named.first <= line && line <= named.last)
        .filter(|named| named.first != line)
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_string(),
    }
}

/// Walks `target` and searches every file, returning the formatted result and the matches as data.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a bad regex or glob.
fn search(
    sandbox: &Sandbox,
    target: &Path,
    args: &Args,
    limit: usize,
) -> Result<(String, Value), ToolError> {
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
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut files: Vec<(std::path::PathBuf, usize)> = Vec::new();
    for entry in walker.build().flatten() {
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        let path = sandbox.display(entry.path());
        let before = found.len();
        let sink = Collector {
            path: path.clone(),
            found: &mut found,
            limit,
        };
        let _ = searcher.search_path(&matcher, entry.path(), sink);
        if found.len() > before {
            files.push((entry.path().to_path_buf(), before));
            if let Ok(file_tag) = tag_file(entry.path()) {
                tags.push((path, file_tag));
            }
        }
        if found.len() >= limit {
            break;
        }
    }
    let truncated = found.len() >= limit;
    if found.len() <= NAMED_LIMIT {
        name_functions(&mut found, &files);
    }
    let data = json!({
        "matches": found.iter().map(Match::value).collect::<Vec<_>>(),
        "truncated": truncated,
    });
    if found.is_empty() {
        return Ok(("No matches found".to_string(), data));
    }
    let mut text = found
        .iter()
        .map(Match::shown)
        .collect::<Vec<_>>()
        .join("\n");
    if truncated {
        text.push_str(&format!(
            "\n[results truncated at {limit} matches; narrow the pattern, path or glob]"
        ));
    }
    text.push_str("\n[tags]");
    for (path, tag) in tags {
        text.push_str(&format!("\n{path}: {tag}"));
    }
    Ok((text, data))
}

#[cfg(test)]
mod tests {
    use super::super::snapshot::tag;
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
        assert!(
            all.text
                .starts_with("a.rs:2:let needle = 1;\nb.txt:1:needle here\n[tags]\n")
        );
        assert!(all.text.contains(&format!(
            "a.rs: {}",
            tag(b"fn main() {}\nlet needle = 1;\n")
        )));
        assert!(
            all.text
                .contains(&format!("b.txt: {}", tag(b"needle here\n")))
        );
        let only_rs = tool
            .call(json!({ "pattern": "needle", "glob": "*.rs" }))
            .await
            .unwrap();
        assert!(
            only_rs
                .text
                .starts_with("a.rs:2:let needle = 1;\n[tags]\na.rs: ")
        );
        assert!(!only_rs.text.contains("b.txt"));
    }

    /// A match inside a named function of a code file says which, in the text and in the data, while the line a function starts on, a match outside any function and a match in a file that is not code say nothing; a long result names no functions.
    #[tokio::test]
    async fn matches_name_the_function_they_are_in() {
        let (tool, dir) = tool("grep-within");
        std::fs::write(
            dir.join("a.rs"),
            "const KEY: u32 = 1;\nfn outer() {\n    let key = KEY;\n    fn inner() {\n        let key = 2;\n    }\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.txt"), "key\n").unwrap();
        let out = tool
            .call(json!({ "pattern": "(?i)key|fn outer" }))
            .await
            .unwrap();
        assert!(out.text.starts_with(
            "a.rs:1:const KEY: u32 = 1;\na.rs:2:fn outer() {\na.rs:3 in outer:    let key = KEY;\na.rs:5 in inner:        let key = 2;\nb.txt:1:key\n[tags]"
        ));
        let matches = out.data.unwrap()["matches"].clone();
        assert_eq!(matches[2]["within"], "outer");
        assert!(matches[0].get("within").is_none());
        let many = "fn f() {\n".to_owned() + &"    let key = 1;\n".repeat(NAMED_LIMIT + 1) + "}\n";
        std::fs::write(dir.join("c.rs"), many).unwrap();
        let long = tool
            .call(json!({ "pattern": "key", "glob": "c.rs" }))
            .await
            .unwrap();
        assert!(!long.text.contains(" in f:"));
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
                .filter(|l| l.starts_with("many.txt:") && !l.starts_with("many.txt: "))
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
        assert!(out.text.starts_with("ok.txt:1:needle\n[tags]\nok.txt: "));
        assert!(!out.text.contains("secret") && !out.text.contains("bin.dat"));
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
