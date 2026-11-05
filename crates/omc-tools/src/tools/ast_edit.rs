//! The `ast_edit` tool: structural rewrites with tree-sitter patterns, applied to every file or to none.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use ast_grep_core::replacer::TemplateFix;
use ast_grep_core::tree_sitter::LanguageExt;
use ast_grep_language::SupportLang;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::ast::{Patterns, Query, files, language_name, language_names, line_at, read_source};
use super::atomic::write_atomic;
use super::snapshot::tag;
use crate::tool::{marked_lines, parse_args};
use crate::{FileChange, Sandbox, Tool, ToolError, ToolOutput};

/// Most preview lines shown for one file.
const PREVIEW_LINES: usize = 40;

/// Most files whose preview is shown; the rest are listed with their counts only.
const PREVIEW_FILES: usize = 20;

/// Rewrites code by syntax tree.
pub struct AstEdit {
    /// The directory paths are confined to.
    sandbox: Sandbox,
}

impl AstEdit {
    /// Creates the tool confined to `sandbox`.
    pub fn new(sandbox: Sandbox) -> Self {
        Self { sandbox }
    }
}

/// Arguments of an `ast_edit` call.
#[derive(Deserialize)]
struct Args {
    /// The pattern, one syntax node with metavariables.
    pattern: String,
    /// The code each match becomes, with the pattern's metavariables substituted.
    rewrite: String,
    /// Language to parse as; inferred from each file's extension otherwise.
    lang: Option<String>,
    /// File or directory to rewrite in; defaults to the working directory.
    path: Option<String>,
    /// Glob that file paths must match.
    glob: Option<String>,
    /// Whether to only report what would change.
    #[serde(default)]
    dry_run: bool,
}

/// One replaced region, as byte ranges of the old and the new text.
struct Span {
    /// Where the match was in the old text.
    old: (usize, usize),
    /// Where its replacement is in the new text.
    new: (usize, usize),
}

/// A file rewritten in memory and ready to be written.
struct Pending {
    /// Resolved location of the file.
    path: PathBuf,
    /// The file as the user sees it.
    shown: String,
    /// The file before the rewrite.
    old: String,
    /// The file after the rewrite.
    new: String,
    /// The replaced regions, in order.
    spans: Vec<Span>,
}

#[async_trait]
impl Tool for AstEdit {
    /// Returns `ast_edit`.
    fn name(&self) -> &'static str {
        "ast_edit"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Rewrite code by syntax tree across files: every match of pattern is replaced by rewrite, \
         with the pattern's metavariables substituted. Pattern syntax is as for ast_grep: $NAME \
         matches one node, $$$NAME zero or more nodes, $_ one node uncaptured, the same \
         metavariable twice must match identical code, and the pattern must parse as a single \
         complete node. Example: pattern `$X.unwrap()`, rewrite `$X?`; pattern \
         `assertEquals($A, $B)`, rewrite `assertThat($B).isEqualTo($A)`. Every metavariable in \
         rewrite must be captured by pattern. Matches never overlap; nested matches inside a \
         replaced one are left as they are, so run again if needed. Set dry_run to preview \
         without writing; run ast_grep first when unsure what matches. A call changes every file \
         or none, and returns per-file counts, a preview and new snapshot tags. The user is asked \
         to approve each call."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "Code pattern with $NAME, $$$NAME and $_ metavariables; must parse as one node." },
                "rewrite": { "type": "string", "description": "Replacement code; $NAME and $$$NAME insert what the pattern captured." },
                "lang": { "type": "string", "enum": language_names(), "description": "Language to parse as. Defaults to inferring each file's language from its extension." },
                "path": { "type": "string", "description": "File or directory to rewrite in. Defaults to the working directory." },
                "glob": { "type": "string", "description": "Only rewrite files whose path matches this glob, e.g. \"*.rs\" or \"src/**/*.ts\"." },
                "dry_run": { "type": "boolean", "description": "Report what would change without writing. Defaults to false." }
            },
            "required": ["pattern", "rewrite"]
        })
    }

    /// Rewrites change files, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows where the rewrite applies, then the pattern as removed lines and the rewrite as added ones.
    fn describe(&self, args: &Value) -> String {
        let pattern = args["pattern"].as_str().unwrap_or("?");
        let rewrite = args["rewrite"].as_str().unwrap_or("?");
        let mut head = format!("{} → {}", headline(pattern), headline(rewrite));
        let place = self.sandbox.describe(args["path"].as_str().unwrap_or("."));
        if !place.is_empty() && place != "." {
            head.push_str(&format!(" in {place}"));
        }
        if let Some(glob) = args["glob"].as_str() {
            head.push_str(&format!(" ({glob})"));
        }
        if args["dry_run"].as_bool() == Some(true) {
            head.push_str(" [dry run]");
        }
        format!(
            "{head}\n{}\n{}",
            marked_lines('-', pattern),
            marked_lines('+', rewrite)
        )
    }

    /// Plans the rewrite on a blocking thread and writes it unless this is a dry run.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let shown = args.path.as_deref().unwrap_or(".").to_owned();
        let target = self.sandbox.resolve(&shown)?;
        if !target.exists() {
            return Err(ToolError::Failed(format!("{shown} does not exist")));
        }
        let sandbox = self.sandbox.clone();
        tokio::task::spawn_blocking(move || apply(&sandbox, &target, &args))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))?
    }
}

/// The first non-blank line of `code`, trimmed, followed by how many more
/// non-blank lines it has, so that a title never hides that there is more.
fn headline(code: &str) -> String {
    let mut lines = code.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().unwrap_or("?");
    match lines.count() {
        0 => first.to_owned(),
        more => format!("{first} (+{more} more lines)"),
    }
}

/// Plans the rewrite and, unless it is a dry run, writes every changed file or none.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a bad pattern, rewrite, glob or
/// language; [`ToolError::Failed`] when a file changed while the rewrite was
/// being prepared; [`ToolError::Io`] when writing fails, after putting back
/// the files already written.
fn apply(sandbox: &Sandbox, target: &Path, args: &Args) -> Result<ToolOutput, ToolError> {
    let (pending, notes) = plan(sandbox, target, args)?;
    if pending.is_empty() {
        let mut text = "No matches found; nothing changed".to_owned();
        for note in notes {
            text.push_str(&format!("\n[skipped files: {note}]"));
        }
        return Ok(ToolOutput::new(text));
    }
    if !args.dry_run {
        for item in &pending {
            if read_source(&item.path).as_deref() != Some(item.old.as_str()) {
                return Err(ToolError::Failed(format!(
                    "{} changed while the rewrite was being prepared; nothing was written, \
                     run the rewrite again",
                    item.shown
                )));
            }
        }
        write_all(&pending)?;
    }
    let total: usize = pending.iter().map(|item| item.spans.len()).sum();
    let mut text = if args.dry_run {
        format!(
            "Dry run: {total} replacement(s) in {} file(s) would be made; nothing was written.",
            pending.len()
        )
    } else {
        format!("{total} replacement(s) in {} file(s).", pending.len())
    };
    for (index, item) in pending.iter().enumerate() {
        let file_tag = if args.dry_run {
            tag(item.old.as_bytes())
        } else {
            tag(item.new.as_bytes())
        };
        text.push_str(&format!(
            "\n\n{}: {} replacement(s), tag {file_tag}",
            item.shown,
            item.spans.len()
        ));
        if index < PREVIEW_FILES {
            text.push('\n');
            text.push_str(&preview(item));
        }
    }
    if pending.len() > PREVIEW_FILES {
        text.push_str(&format!(
            "\n\n[previews shown for the first {PREVIEW_FILES} files only]"
        ));
    }
    for note in notes {
        text.push_str(&format!("\n[skipped files: {note}]"));
    }
    let output = ToolOutput::new(text);
    if args.dry_run {
        return Ok(output);
    }
    let changes = pending
        .into_iter()
        .map(|item| FileChange {
            path: item.shown,
            old: item.old,
            new: item.new,
        })
        .collect();
    Ok(output.with_changes(changes))
}

/// Rewrites every matching file in memory and returns the changed files with notes on the languages the pattern did not fit.
///
/// # Errors
///
/// See [`apply`].
fn plan(
    sandbox: &Sandbox,
    target: &Path,
    args: &Args,
) -> Result<(Vec<Pending>, Vec<String>), ToolError> {
    let mut query = Query::new(&args.pattern, args.lang.as_deref())?;
    let mut checked: HashSet<SupportLang> = HashSet::new();
    let named_directly = target.is_file();
    let mut pending = Vec::new();
    for path in files(target, args.glob.as_deref())? {
        let Some(lang) = query.language_of(&path, named_directly) else {
            continue;
        };
        let Ok(pattern) = query.pattern(lang) else {
            continue;
        };
        if checked.insert(lang) {
            check_rewrite(&args.rewrite, pattern, lang)?;
        }
        let Some(old) = read_source(&path) else {
            continue;
        };
        let resolved = sandbox.resolve(&sandbox.display(&path))?;
        let root = lang.ast_grep(&old);
        let mut edits = root.root().replace_all(pattern, args.rewrite.as_str());
        if edits.is_empty() {
            continue;
        }
        edits.sort_by_key(|edit| edit.position);
        let mut new = String::with_capacity(old.len());
        let mut spans = Vec::new();
        let mut cursor = 0;
        for edit in edits {
            if edit.position < cursor {
                continue;
            }
            new.push_str(&old[cursor..edit.position]);
            let start = new.len();
            new.push_str(&String::from_utf8_lossy(&edit.inserted_text));
            spans.push(Span {
                old: (edit.position, edit.position + edit.deleted_length),
                new: (start, new.len()),
            });
            cursor = edit.position + edit.deleted_length;
        }
        new.push_str(&old[cursor..]);
        if new == old {
            continue;
        }
        pending.push(Pending {
            shown: sandbox.display(&resolved),
            path: resolved,
            old,
            new,
            spans,
        });
    }
    if pending.is_empty() && !query.compiled_any() {
        let failures = query.failures();
        if !failures.is_empty() {
            return Err(ToolError::InvalidArguments(failures.join("\n")));
        }
    }
    Ok((pending, query.failures()))
}

/// Checks that `rewrite` only uses metavariables that `pattern` captures, since any other would silently vanish.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] naming the metavariables the pattern does not capture.
fn check_rewrite(rewrite: &str, pattern: &Patterns, lang: SupportLang) -> Result<(), ToolError> {
    let template = TemplateFix::try_new(rewrite, &lang).map_err(|error| {
        ToolError::InvalidArguments(format!("rewrite is not a valid template: {error}"))
    })?;
    let defined = pattern.defined_vars();
    let mut missing: Vec<&str> = template
        .used_vars()
        .into_iter()
        .filter(|name| !defined.contains(name))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    missing.sort_unstable();
    Err(ToolError::InvalidArguments(format!(
        "rewrite uses metavariables the {} pattern does not capture: {}; capture them in \
         the pattern or remove them from the rewrite",
        language_name(lang),
        missing
            .iter()
            .map(|name| format!("${name}"))
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// Writes every pending file; when one write fails, puts the files already written back as they were.
///
/// # Errors
///
/// Returns the error of the write that failed.
fn write_all(pending: &[Pending]) -> Result<(), ToolError> {
    for (done, item) in pending.iter().enumerate() {
        if let Err(error) = write_atomic(&item.path, item.new.as_bytes()) {
            for written in &pending[..done] {
                let _ = write_atomic(&written.path, written.old.as_bytes());
            }
            return Err(error);
        }
    }
    Ok(())
}

/// Shows the changed lines of `item` as hunks of removed and added lines, merging replacements on the same or adjacent lines and capping the length.
fn preview(item: &Pending) -> String {
    let old_lines: Vec<&str> = item.old.lines().collect();
    let new_lines: Vec<&str> = item.new.lines().collect();
    let mut hunks: Vec<((usize, usize), (usize, usize))> = Vec::new();
    for span in &item.spans {
        let old = lines_of(&item.old, span.old);
        let new = lines_of(&item.new, span.new);
        match hunks.last_mut() {
            Some((last_old, last_new)) if old.0 <= last_old.1 + 1 => {
                last_old.1 = last_old.1.max(old.1);
                last_new.1 = last_new.1.max(new.1);
            }
            _ => hunks.push((old, new)),
        }
    }
    let mut out: Vec<String> = Vec::new();
    for (old, new) in hunks {
        out.push(format!("@@ line {}", old.0 + 1));
        out.extend(
            marked_lines('-', &slice(&old_lines, old))
                .lines()
                .map(str::to_owned),
        );
        out.extend(
            marked_lines('+', &slice(&new_lines, new))
                .lines()
                .map(str::to_owned),
        );
    }
    if out.len() > PREVIEW_LINES {
        let hidden = out.len() - PREVIEW_LINES;
        out.truncate(PREVIEW_LINES);
        out.push(format!("… {hidden} more preview lines"));
    }
    out.join("\n")
}

/// The 0-indexed first and last line of `text` that the byte range `span` touches.
fn lines_of(text: &str, span: (usize, usize)) -> (usize, usize) {
    let first = line_at(text, span.0);
    let last = line_at(text, span.1.saturating_sub(1).max(span.0));
    (first, last)
}

/// Joins the lines `range` of `lines`, both ends included.
fn slice(lines: &[&str], range: (usize, usize)) -> String {
    lines
        .iter()
        .skip(range.0)
        .take(range.1.saturating_sub(range.0) + 1)
        .copied()
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds an `ast_edit` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (AstEdit, PathBuf) {
        let dir = scratch(name);
        (AstEdit::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// The first line of the description, which approval prompts show as the
    /// title, names the rewrite and where it applies.
    #[test]
    fn describes_the_rewrite_on_the_first_line() {
        let (tool, dir) = tool("ast-edit-describe");
        std::fs::create_dir_all(dir.join("py")).unwrap();
        let whole = tool.describe(&json!({
            "pattern": "print($$$A)", "rewrite": "logging.info($$$A)", "dry_run": true
        }));
        assert_eq!(
            whole.lines().next(),
            Some("print($$$A) → logging.info($$$A) [dry run]")
        );
        let scoped = tool.describe(&json!({
            "pattern": "print($$$A)", "rewrite": "log($$$A)", "path": "py", "glob": "*.py"
        }));
        assert_eq!(
            scoped.lines().next(),
            Some("print($$$A) → log($$$A) in py (*.py)")
        );
    }

    /// A title never hides extra lines of a pattern or rewrite, blank lines first included.
    #[test]
    fn titles_say_when_there_is_more() {
        let (tool, _dir) = tool("ast-edit-headline");
        let hidden = tool.describe(&json!({
            "pattern": "f($X)", "rewrite": "\n\ng($X)\nevil()"
        }));
        assert_eq!(hidden.lines().next(), Some("f($X) → g($X) (+1 more lines)"));
        assert!(hidden.contains("+ evil()"), "{hidden}");
    }

    /// Metavariables carry the matched code into the rewrite, across files, and the result reports counts, a preview, tags and changes.
    #[tokio::test]
    async fn rewrites_with_metavariables() {
        let (tool, dir) = tool("ast-edit-rewrite");
        std::fs::write(
            dir.join("a.rs"),
            "fn f() {\n    let x = g().unwrap();\n    h(y.unwrap());\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.rs"), "fn k() { z.unwrap() }\n").unwrap();
        let out = tool
            .call(json!({ "pattern": "$X.unwrap()", "rewrite": "$X.expect(\"present\")" }))
            .await
            .unwrap();
        let a = std::fs::read_to_string(dir.join("a.rs")).unwrap();
        assert_eq!(
            a,
            "fn f() {\n    let x = g().expect(\"present\");\n    h(y.expect(\"present\"));\n}\n"
        );
        let b = std::fs::read_to_string(dir.join("b.rs")).unwrap();
        assert_eq!(b, "fn k() { z.expect(\"present\") }\n");
        assert!(out.text.starts_with("3 replacement(s) in 2 file(s)."));
        assert!(out.text.contains(&format!(
            "a.rs: 2 replacement(s), tag {}",
            tag(a.as_bytes())
        )));
        assert!(out.text.contains(
            "@@ line 2\n-     let x = g().unwrap();\n-     h(y.unwrap());\n+     let x = g().expect(\"present\");\n+     h(y.expect(\"present\"));"
        ));
        assert_eq!(out.changes.len(), 2);
    }

    /// Multi-node metavariables keep the captured list, and a repeated metavariable only matches identical code.
    #[tokio::test]
    async fn rewrites_lists_and_respects_repeats() {
        let (tool, dir) = tool("ast-edit-lists");
        std::fs::write(
            dir.join("a.py"),
            "log(a, b, c)\nx = y if y else z\nx = y if w else z\n",
        )
        .unwrap();
        tool.call(json!({ "pattern": "log($$$ARGS)", "rewrite": "logger.info($$$ARGS)" }))
            .await
            .unwrap();
        tool.call(json!({ "pattern": "$A if $A else $B", "rewrite": "$A or $B" }))
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("a.py")).unwrap(),
            "logger.info(a, b, c)\nx = y or z\nx = y if w else z\n"
        );
    }

    /// A dry run reports the replacements and current tags but writes nothing.
    #[tokio::test]
    async fn dry_run_leaves_files_untouched() {
        let (tool, dir) = tool("ast-edit-dry");
        let code = "print(1)\nprint(2)\n";
        std::fs::write(dir.join("a.py"), code).unwrap();
        let out = tool
            .call(json!({ "pattern": "print($X)", "rewrite": "log($X)", "dry_run": true }))
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("a.py")).unwrap(), code);
        assert!(
            out.text
                .starts_with("Dry run: 2 replacement(s) in 1 file(s)")
        );
        assert!(out.text.contains(&format!("tag {}", tag(code.as_bytes()))));
        assert!(
            out.text
                .contains("- print(1)\n- print(2)\n+ log(1)\n+ log(2)")
        );
        assert!(out.changes.is_empty());
    }

    /// When one file cannot be written, the files already written are put back.
    #[cfg(unix)]
    #[tokio::test]
    async fn multi_file_rewrite_is_all_or_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let (tool, dir) = tool("ast-edit-atomic");
        std::fs::create_dir(dir.join("locked")).unwrap();
        std::fs::write(dir.join("a.py"), "f(1)\n").unwrap();
        std::fs::write(dir.join("locked/b.py"), "f(2)\n").unwrap();
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o555))
            .unwrap();
        let result = tool
            .call(json!({ "pattern": "f($X)", "rewrite": "g($X)" }))
            .await;
        std::fs::set_permissions(dir.join("locked"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        assert!(result.is_err());
        assert_eq!(std::fs::read_to_string(dir.join("a.py")).unwrap(), "f(1)\n");
        assert_eq!(
            std::fs::read_to_string(dir.join("locked/b.py")).unwrap(),
            "f(2)\n"
        );
    }

    /// A bad pattern or a rewrite naming an uncaptured metavariable fails before anything is written.
    #[tokio::test]
    async fn refuses_bad_patterns_and_rewrites() {
        let (tool, dir) = tool("ast-edit-errors");
        std::fs::write(dir.join("a.rs"), "fn main() { f(1); }\n").unwrap();
        let bad = tool
            .call(json!({ "pattern": "f(", "rewrite": "g()", "lang": "rust" }))
            .await;
        assert!(matches!(bad, Err(ToolError::InvalidArguments(_))));
        let undefined = tool
            .call(json!({ "pattern": "f($X)", "rewrite": "g($Y)" }))
            .await;
        assert!(matches!(undefined, Err(ToolError::InvalidArguments(m)) if m.contains("$Y")));
        assert_eq!(
            std::fs::read_to_string(dir.join("a.rs")).unwrap(),
            "fn main() { f(1); }\n"
        );
        let none = tool
            .call(json!({ "pattern": "nope($X)", "rewrite": "g($X)" }))
            .await
            .unwrap();
        assert!(none.text.starts_with("No matches found"));
    }
}
