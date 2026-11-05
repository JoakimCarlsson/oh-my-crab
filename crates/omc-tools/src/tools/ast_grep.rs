//! The `ast_grep` tool: structural search with tree-sitter patterns, honoring `.gitignore`.

use std::path::Path;

use ast_grep_core::tree_sitter::LanguageExt;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ast::{Query, captures, files, language_names, one_line, read_source};
use super::snapshot::tag;
use crate::tool::parse_args;
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// Matches returned when the caller gives no limit.
const DEFAULT_LIMIT: usize = 50;

/// Largest limit a caller may ask for.
const MAX_LIMIT: usize = 500;

/// Most lines of one match shown.
const SNIPPET_LINES: usize = 6;

/// Longest snippet line kept, in characters.
const MAX_LINE_CHARS: usize = 300;

/// Searches code by syntax tree.
pub struct AstGrep {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl AstGrep {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of an `ast_grep` call.
#[derive(Deserialize)]
struct Args {
    /// The pattern, one syntax node with metavariables.
    pattern: String,
    /// Language to parse as; inferred from each file's extension otherwise.
    lang: Option<String>,
    /// File or directory to search; defaults to the working directory.
    path: Option<String>,
    /// Glob that file paths must match.
    glob: Option<String>,
    /// Most matches to return.
    limit: Option<usize>,
}

#[async_trait]
impl Tool for AstGrep {
    /// Returns `ast_grep`.
    fn name(&self) -> &'static str {
        "ast_grep"
    }

    /// Searching changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Search code by syntax tree instead of text, so formatting, comments and line breaks do \
         not matter. The pattern is code in the target language with metavariables: $NAME matches \
         exactly one node (an expression, identifier, statement…), $$$NAME matches zero or more \
         nodes (arguments, statements, fields), $_ matches one node without capturing it. Names \
         are uppercase letters, digits and _. The same metavariable used twice must match \
         identical code: `$A == $A`. The pattern must parse as a single complete node: \
         `foo($$$ARGS)`, `fn $NAME($$$P) -> $R { $$$B }`, `if err != nil { $$$ }`; a fragment like \
         `foo(` or two statements fails, so wrap it in enough code to parse. Pass lang when files \
         lack a telling extension or the pattern means different things per language. Honors \
         .gitignore. Returns path:line:snippet per match with captured metavariables, then the \
         snapshot tag of each file with matches; pass a tag to edit."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Code pattern with $NAME, $$$NAME and $_ metavariables; must parse as one node." },
                "lang": { "type": "string", "enum": language_names(), "description": "Language to parse as. Defaults to inferring each file's language from its extension." },
                "path": { "type": "string", "description": "File or directory to search. Defaults to the working directory." },
                "glob": { "type": "string", "description": "Only search files whose path matches this glob, e.g. \"*.rs\" or \"src/**/*.ts\"." },
                "limit": { "type": "integer", "minimum": 1, "description": "Maximum matches. Defaults to 50, at most 500." }
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
        let shown = args.path.as_deref().unwrap_or(".").to_owned();
        let target = self.sandbox.resolve(&shown)?;
        if !target.exists() {
            return Err(ToolError::Failed(format!("{shown} does not exist")));
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let sandbox = self.sandbox.clone();
        let text = tokio::task::spawn_blocking(move || search(&sandbox, &target, &args, limit))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        Ok(ToolOutput::new(text))
    }
}

/// Walks `target`, matches the pattern in every file of a known language and formats the result.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a bad glob or language, or a
/// pattern that compiles for none of the languages searched.
fn search(
    sandbox: &Sandbox,
    target: &Path,
    args: &Args,
    limit: usize,
) -> Result<String, ToolError> {
    let mut query = Query::new(&args.pattern, args.lang.as_deref())?;
    let named_directly = target.is_file();
    let mut found: Vec<String> = Vec::new();
    let mut tags: Vec<(String, String)> = Vec::new();
    let mut truncated = false;
    for path in files(target, args.glob.as_deref())? {
        let Some(lang) = query.language_of(&path, named_directly) else {
            continue;
        };
        let Ok(pattern) = query.pattern(lang) else {
            continue;
        };
        let Some(source) = read_source(&path) else {
            continue;
        };
        let shown = sandbox.display(&path);
        let root = lang.ast_grep(&source);
        let before = found.len();
        for hit in root.root().find_all(pattern) {
            if found.len() >= limit {
                truncated = true;
                break;
            }
            let line = hit.start_pos().line() + 1;
            let mut entry = snippet(&shown, line, &hit.text());
            for (name, code) in captures(pattern, &hit, &source) {
                entry.push_str(&format!("\n    {name} = {}", one_line(&code)));
            }
            found.push(entry);
        }
        if found.len() > before {
            tags.push((shown, tag(source.as_bytes())));
        }
        if truncated {
            break;
        }
    }
    if found.is_empty() && !query.compiled_any() {
        let failures = query.failures();
        if !failures.is_empty() {
            return Err(ToolError::InvalidArguments(failures.join("\n")));
        }
    }
    let mut text = if found.is_empty() {
        "No matches found".to_owned()
    } else {
        found.join("\n")
    };
    if truncated {
        text.push_str(&format!(
            "\n[results truncated at {limit} matches; narrow the pattern, path or glob]"
        ));
    }
    for failure in query.failures() {
        text.push_str(&format!("\n[skipped files: {failure}]"));
    }
    if !tags.is_empty() {
        text.push_str("\n[tags]");
        for (path, file_tag) in tags {
            text.push_str(&format!("\n{path}: {file_tag}"));
        }
    }
    Ok(text)
}

/// Formats one match as `path:line:` followed by its code, continuation lines indented and long matches cut.
fn snippet(path: &str, line: usize, code: &str) -> String {
    let lines: Vec<&str> = code.lines().collect();
    let mut out = format!(
        "{path}:{line}:{}",
        clip(lines.first().copied().unwrap_or(""))
    );
    for more in lines.iter().skip(1).take(SNIPPET_LINES - 1) {
        out.push_str(&format!("\n  | {}", clip(more)));
    }
    if lines.len() > SNIPPET_LINES {
        out.push_str(&format!(
            "\n  | … {} more lines",
            lines.len() - SNIPPET_LINES
        ));
    }
    out
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds an `ast_grep` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (AstGrep, std::path::PathBuf) {
        let dir = scratch(name);
        (AstGrep::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Matches report path, line, snippet, captures and the file's tag, ignoring formatting.
    #[tokio::test]
    async fn finds_matches_with_captures() {
        let (tool, dir) = tool("ast-grep-captures");
        let code = "fn main() {\n    let v = compute(1,\n        two);\n    v.unwrap();\n}\n";
        std::fs::write(dir.join("a.rs"), code).unwrap();
        std::fs::write(dir.join("b.py"), "compute(1, two)\n").unwrap();
        let out = tool
            .call(json!({ "pattern": "compute($A, $$$REST)", "lang": "rust" }))
            .await
            .unwrap();
        assert!(
            out.text
                .starts_with("a.rs:2:compute(1,\n  |         two)\n")
        );
        assert!(out.text.contains("    $A = 1\n    $$$REST = two\n"));
        assert!(
            out.text
                .ends_with(&format!("[tags]\na.rs: {}", tag(code.as_bytes())))
        );
        assert!(!out.text.contains("b.py"));
        let both = tool
            .call(json!({ "pattern": "compute($A, $B)" }))
            .await
            .unwrap();
        assert!(both.text.contains("a.rs:2:") && both.text.contains("b.py:1:compute(1, two)"));
    }

    /// Calls match in languages whose top level cannot hold a bare call, such as Go and C.
    #[tokio::test]
    async fn matches_calls_in_go_and_c() {
        let (tool, dir) = tool("ast-grep-go-c");
        std::fs::write(
            dir.join("a.go"),
            "package main\nfunc main() {\n\tfmt.Println(x)\n\tfmt.Println(\"a\", y)\n}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("a.c"),
            "int main() { char *p = malloc(10); return 0; }\n",
        )
        .unwrap();
        let go = tool
            .call(json!({ "pattern": "fmt.Println($$$A)" }))
            .await
            .unwrap();
        assert!(go.text.contains("a.go:3:fmt.Println(x)") && go.text.contains("a.go:4:"));
        let c = tool.call(json!({ "pattern": "malloc($N)" })).await.unwrap();
        assert!(c.text.starts_with("a.c:1:malloc(10)\n    $N = 10\n"));
    }

    /// A metavariable used twice only matches identical code.
    #[tokio::test]
    async fn repeated_metavariables_must_agree() {
        let (tool, dir) = tool("ast-grep-repeat");
        std::fs::write(dir.join("a.py"), "x == x\nx == y\n").unwrap();
        let out = tool.call(json!({ "pattern": "$A == $A" })).await.unwrap();
        assert!(out.text.starts_with("a.py:1:x == x\n"));
        assert!(!out.text.contains("a.py:2:"));
    }

    /// The limit caps the result and ignored files are skipped.
    #[tokio::test]
    async fn caps_results_and_skips_ignored_files() {
        let (tool, dir) = tool("ast-grep-limit");
        std::fs::write(dir.join(".gitignore"), "gen.py\n").unwrap();
        std::fs::write(dir.join("gen.py"), "f(1)\n").unwrap();
        std::fs::write(dir.join("many.py"), "f(1)\n".repeat(10)).unwrap();
        let out = tool
            .call(json!({ "pattern": "f($X)", "limit": 3 }))
            .await
            .unwrap();
        assert_eq!(out.text.matches("many.py:").count(), 4);
        assert!(out.text.contains("truncated at 3 matches"));
        assert!(!out.text.contains("gen.py"));
    }

    /// A pattern that does not parse is an error that says why; no match is not.
    #[tokio::test]
    async fn reports_bad_patterns_and_empty_searches() {
        let (tool, dir) = tool("ast-grep-errors");
        std::fs::write(dir.join("a.rs"), "fn main() {}\n").unwrap();
        let bad = tool
            .call(json!({ "pattern": "fn main(", "lang": "rust" }))
            .await;
        assert!(matches!(bad, Err(ToolError::InvalidArguments(m)) if m.contains("parse")));
        let inferred = tool.call(json!({ "pattern": "fn main(" })).await;
        assert!(matches!(inferred, Err(ToolError::InvalidArguments(_))));
        let none = tool
            .call(json!({ "pattern": "nothing_here()" }))
            .await
            .unwrap();
        assert_eq!(none.text, "No matches found");
        let escape = tool.call(json!({ "pattern": "x", "path": ".." })).await;
        assert!(matches!(escape, Err(ToolError::OutsidePath(_))));
    }
}
