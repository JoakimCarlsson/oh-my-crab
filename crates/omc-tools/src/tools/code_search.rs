//! The `code_search` tool: find code by what it does as well as by its words.

use std::sync::Arc;

use async_trait::async_trait;
use omc_index::{CodeIndex, Found, Hit};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Caller, NoCaller, Sandbox, Tool, ToolError, ToolOutput};

/// Results given when the caller gives no limit.
const DEFAULT_LIMIT: usize = 8;

/// The most results a caller may ask for.
const MAX_LIMIT: usize = 30;

/// The most lines of a chunk shown under its header.
const SNIPPET_LINES: usize = 12;

/// The most characters of one line shown.
const MAX_LINE_CHARS: usize = 200;

/// Searches the project's code index.
pub struct CodeSearch {
    /// The index of the project the sandbox is rooted in.
    index: Arc<CodeIndex>,
}

impl CodeSearch {
    /// The tool searching the index of the project `sandbox` is rooted in, or nothing when the code index is turned off or there is no cache folder to keep it in.
    pub fn for_sandbox(sandbox: &Sandbox) -> Option<Self> {
        CodeIndex::for_project(sandbox.root()).map(Self::with_index)
    }

    /// The tool searching `index`.
    pub fn with_index(index: CodeIndex) -> Self {
        Self {
            index: Arc::new(index),
        }
    }
}

/// Arguments of a `code_search` call.
#[derive(Deserialize)]
struct Args {
    /// What to look for.
    query: String,
    /// The glob, folder or file the results must be in.
    path: Option<String>,
    /// The most results to give.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for CodeSearch {
    /// Returns `code_search`.
    fn name(&self) -> &'static str {
        "code_search"
    }

    /// Only reads the project; the index it writes is crab's own cache.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/code_search.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What the code does or is called, in plain words or identifiers."
                },
                "path": {
                    "type": "string",
                    "description": "Keep to files this glob matches (`*.rs`, `src/**/*.ts`), or to this folder or file."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_LIMIT,
                    "description": "The most results; 8 by default."
                }
            },
            "required": ["query"]
        })
    }

    /// Shows the query and the path filter.
    fn describe(&self, args: &Value) -> String {
        let query = args["query"].as_str().unwrap_or("?");
        match args["path"].as_str() {
            Some(path) => format!("{query} in {path}"),
            None => query.to_owned(),
        }
    }

    /// The results as a list of chunks, and whether the search went by keyword alone.
    fn data_shape(&self) -> Option<&'static str> {
        Some(
            "{ results: { path: string; start: number; end: number; text: string }[]; keyword_only: boolean; hidden: number }",
        )
    }

    /// Brings the index up to date and searches it, showing every file it finds.
    ///
    /// # Errors
    ///
    /// Fails as [`CodeSearch::call_through`] does.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_through("", args, &NoCaller).await
    }

    /// Brings the index up to date and searches it, leaving out the files `caller` says the permission rules keep from the agent, as they would keep `read` and `grep` from them.
    ///
    /// # Errors
    ///
    /// Fails when the query is blank or `path` is not a valid glob, and when the index cannot be opened, read or written.
    async fn call_through(
        &self,
        _id: &str,
        args: Value,
        caller: &dyn Caller,
    ) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if args.query.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "`query` is empty; say what to look for".to_owned(),
            ));
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let mut found = self
            .index
            .search(&args.query, args.path.as_deref(), limit * 2)
            .await
            .map_err(|error| match error {
                omc_index::IndexError::Glob(reason) => {
                    ToolError::InvalidArguments(format!("invalid glob: {reason}"))
                }
                other => ToolError::Failed(other.to_string()),
            })?;
        let root = self.index.root().to_path_buf();
        let before = found.hits.len();
        found
            .hits
            .retain(|hit| caller.may_read(&root.join(&hit.path)));
        let hidden = before - found.hits.len();
        found.hits.truncate(limit);
        let data = json!({
            "results": found.hits.iter().map(|hit| json!({
                "path": hit.path,
                "start": hit.start,
                "end": hit.end,
                "text": hit.text,
            })).collect::<Vec<_>>(),
            "keyword_only": found.keyword_only.is_some(),
            "hidden": hidden,
        });
        Ok(ToolOutput::new(shown(&found, hidden, &args.query)).with_data(data))
    }
}

/// What the model is shown of `found`, the results of a search for `query` less `hidden` ones the permission rules keep from the agent: a note when the search went by keyword alone, the index stopped short or results were left out, then each chunk as `path:first-last` over its best lines, numbered.
fn shown(found: &Found, hidden: usize, query: &str) -> String {
    let mut notes = Vec::new();
    if hidden > 0 {
        let noun = if hidden == 1 { "result" } else { "results" };
        notes.push(format!(
            "[{hidden} {noun} left out: permission rules keep their files from you]"
        ));
    }
    if let Some(reason) = &found.keyword_only {
        notes.push(format!("[keyword search only: {reason}]"));
    }
    if found.refreshed.capped {
        notes.push(format!(
            "[the index stopped at {} files; narrow `path` to search the rest]",
            found.refreshed.files
        ));
    }
    if found.hits.is_empty() {
        notes.push("No code matches.".to_owned());
        return notes.join("\n");
    }
    let terms = omc_index::query_terms(query);
    let results: Vec<String> = found.hits.iter().map(|hit| snippet(hit, &terms)).collect();
    notes.push(results.join("\n\n"));
    notes.join("\n")
}

/// `hit` as `path:first-last` over at most [`SNIPPET_LINES`] of its lines, numbered: the run of lines holding the most of `terms`, the first lines when none does.
fn snippet(hit: &Hit, terms: &[String]) -> String {
    let lines: Vec<&str> = hit.text.lines().collect();
    let first = best_window(&lines, terms);
    let shown_lines = &lines[first..lines.len().min(first + SNIPPET_LINES)];
    let mut out = format!("{}:{}-{}", hit.path, hit.start, hit.end);
    for (offset, line) in shown_lines.iter().enumerate() {
        out.push_str(&format!(
            "\n{:>6}\t{}",
            hit.start + first + offset,
            clip(line)
        ));
    }
    if lines.len() > shown_lines.len() {
        out.push_str(&format!(
            "\n[{} of {} lines shown]",
            shown_lines.len(),
            lines.len()
        ));
    }
    out
}

/// The first line of the run of [`SNIPPET_LINES`] lines of `lines` holding the most mentions of `terms`, the earliest among equals.
fn best_window(lines: &[&str], terms: &[String]) -> usize {
    if lines.len() <= SNIPPET_LINES || terms.is_empty() {
        return 0;
    }
    let hits: Vec<usize> = lines
        .iter()
        .map(|line| {
            let lower = line.to_lowercase();
            terms
                .iter()
                .filter(|term| lower.contains(term.as_str()))
                .count()
        })
        .collect();
    let mut best = (0, hits[..SNIPPET_LINES].iter().sum::<usize>());
    let mut sum = best.1;
    for start in 1..=lines.len() - SNIPPET_LINES {
        sum = sum + hits[start + SNIPPET_LINES - 1] - hits[start - 1];
        if sum > best.1 {
            best = (start, sum);
        }
    }
    best.0
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_owned(),
    }
}

#[cfg(test)]
/// Tests for the `code_search` tool.
mod tests {
    use super::*;
    use crate::testutil::scratch;
    use omc_index::{Embed, Embedder};

    /// An embedder that points every text the same way.
    struct Flat;

    impl Embed for Flat {
        /// Names the test model.
        fn model(&self) -> &str {
            "flat"
        }

        /// Uses four numbers.
        fn dimensions(&self) -> usize {
            4
        }

        /// Gives each text the same vector.
        fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
            texts.iter().map(|_| vec![0.5; 4]).collect()
        }
    }

    /// The tool over a scratch project named `name`, embedding with `embedder`, and the project's folder.
    fn tool(name: &str, embedder: Option<Embedder>) -> (CodeSearch, std::path::PathBuf) {
        let dir = scratch(name);
        let root = dir.join("project");
        std::fs::create_dir_all(&root).unwrap();
        let index = CodeIndex::new(&root, &dir.join("index.sqlite3"), embedder.map(Arc::new));
        (CodeSearch::with_index(index), root)
    }

    /// A search names the file and lines of each chunk, shows its best lines numbered, and sees a file written after the first call.
    #[tokio::test]
    async fn finds_chunks_with_numbered_lines() {
        let (tool, root) = tool("code-search", Some(Embedder::with_model(Arc::new(Flat))));
        std::fs::write(
            root.join("retry.py"),
            "import time\n\n\ndef retry_with_backoff(call):\n    time.sleep(1)\n    return call()\n",
        )
        .unwrap();
        let out = tool
            .call(json!({"query": "retry with backoff"}))
            .await
            .unwrap();
        assert!(
            out.text.starts_with("retry.py:1-6\n     1\timport time"),
            "{}",
            out.text
        );
        assert!(out.text.contains("     4\tdef retry_with_backoff(call):"));
        assert_eq!(out.data.as_ref().unwrap()["keyword_only"], false);
        std::fs::write(root.join("later.py"), "def render_page():\n    pass\n").unwrap();
        let later = tool
            .call(json!({"query": "render page", "limit": 1}))
            .await
            .unwrap();
        assert!(later.text.starts_with("later.py:1-2"), "{}", later.text);
        assert_eq!(later.text.matches(".py:").count(), 1);
    }

    /// The path filter keeps to matching files; an empty query and a broken glob are refused; no match says so.
    #[tokio::test]
    async fn filters_and_refuses() {
        let (tool, root) = tool("code-search-filter", None);
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(root.join("web/app.ts"), "function startServer() {}\n").unwrap();
        std::fs::write(root.join("server.go"), "func startServer() {}\n").unwrap();
        let only = tool
            .call(json!({"query": "startServer", "path": "*.go"}))
            .await
            .unwrap();
        assert!(only.text.starts_with("server.go:1-1"), "{}", only.text);
        assert!(!only.text.contains("app.ts"));
        let folder = tool
            .call(json!({"query": "startServer", "path": "web"}))
            .await
            .unwrap();
        assert!(folder.text.starts_with("web/app.ts"));
        assert!(matches!(
            tool.call(json!({"query": "  "})).await,
            Err(ToolError::InvalidArguments(_))
        ));
        assert!(matches!(
            tool.call(json!({"query": "x", "path": "[a"})).await,
            Err(ToolError::InvalidArguments(_))
        ));
        let none = tool.call(json!({"query": "kubernetes"})).await.unwrap();
        assert_eq!(none.text, "No code matches.");
    }

    /// Without the model the results say the search was by keyword alone, and why.
    #[tokio::test]
    async fn says_when_it_falls_back_to_keywords() {
        let dir = scratch("code-search-offline-model");
        let (tool, root) = tool(
            "code-search-offline",
            Some(Embedder::from_source(dir, "http://127.0.0.1:1/none")),
        );
        std::fs::write(root.join("a.rs"), "fn parse_args() {}\n").unwrap();
        let out = tool.call(json!({"query": "parse args"})).await.unwrap();
        assert!(
            out.text.starts_with("[keyword search only: "),
            "{}",
            out.text
        );
        assert!(out.text.contains("a.rs:1-1"));
        assert_eq!(out.data.unwrap()["keyword_only"], true);
    }

    /// A caller whose permission rules keep every file named `.secret` from the agent.
    struct Rules;

    #[async_trait]
    impl Caller for Rules {
        /// Has no tool.
        fn has(&self, _name: &str) -> bool {
            false
        }

        /// Fails, since there is no tool to call.
        async fn call(&self, name: &str, _args: Value) -> Result<ToolOutput, String> {
            Err(format!("unknown tool: {name}"))
        }

        /// Keeps files named `.secret` from the agent.
        fn may_read(&self, path: &std::path::Path) -> bool {
            path.file_name().is_none_or(|name| name != ".secret")
        }
    }

    /// Files the permission rules keep from the agent are left out of the results, and the results say how many.
    #[tokio::test]
    async fn leaves_out_files_the_rules_deny() {
        let (tool, root) = tool("code-search-denied", None);
        std::fs::write(root.join(".secret"), "token = hunter2\n").unwrap();
        std::fs::write(root.join("auth.py"), "def check_token(token):\n    pass\n").unwrap();
        let out = tool
            .call_through("1", json!({"query": "token"}), &Rules)
            .await
            .unwrap();
        assert!(!out.text.contains("hunter2"), "{}", out.text);
        assert!(
            out.text
                .starts_with("[1 result left out: permission rules keep their files from you]")
        );
        assert!(out.text.contains("auth.py:1-2"));
        assert_eq!(out.data.unwrap()["hidden"], 1);
        let open = tool.call(json!({"query": "token"})).await.unwrap();
        assert!(open.text.contains("hunter2"));
    }

    /// The window of a long chunk is the one holding the most query terms.
    #[test]
    fn shows_the_best_lines_of_long_chunks() {
        let mut lines: Vec<String> = (1..=30).map(|n| format!("filler {n}")).collect();
        lines[24] = "the needle is here".to_owned();
        let hit = Hit {
            path: "a.txt".to_owned(),
            start: 10,
            end: 39,
            text: lines.join("\n"),
            score: 1.0,
        };
        let shown = snippet(&hit, &["needle".to_owned()]);
        assert!(
            shown.starts_with("a.txt:10-39\n    23\tfiller 14"),
            "{shown}"
        );
        assert!(shown.contains("    34\tthe needle is here"));
        assert!(shown.ends_with("[12 of 30 lines shown]"));
        assert_eq!(best_window(&["a", "b"], &["a".to_owned()]), 0);
    }
}
