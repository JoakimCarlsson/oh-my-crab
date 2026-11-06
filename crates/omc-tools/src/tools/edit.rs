//! The `edit` tool: line-anchored edits that fail when the file has changed since it was read.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use super::snapshot::tag;
use crate::language::{Languages, Written, settle};
use crate::tool::{marked_lines, parse_args};
use crate::{FileChange, Sandbox, Tool, ToolError, ToolOutput};

/// Lines of context shown around a changed region in the preview.
const PREVIEW_CONTEXT: usize = 2;

/// Most lines the preview of one file shows.
const PREVIEW_LINES: usize = 40;

/// Changes lines of files, citing the snapshot tag the model read.
pub struct Edit {
    /// The directory paths are confined to.
    sandbox: Sandbox,
    /// The language service that formats the edited files and says what is wrong with them.
    languages: Languages,
}

impl Edit {
    /// Creates the tool confined to `sandbox`, with no language service.
    pub fn new(sandbox: Sandbox) -> Self {
        Self::with_languages(sandbox, Languages::default())
    }

    /// Creates the tool confined to `sandbox`, which has the files it edits formatted by `languages` and reports what it says is wrong with them.
    pub fn with_languages(sandbox: Sandbox, languages: Languages) -> Self {
        Self { sandbox, languages }
    }
}

/// Arguments of an `edit` call.
#[derive(Deserialize)]
struct Args {
    /// One section per file to change.
    files: Vec<Section>,
}

/// The operations on one file.
#[derive(Deserialize)]
struct Section {
    /// File to edit, relative to the working directory.
    path: String,
    /// Snapshot tag from the latest `read`, `grep` or `edit` result for the file.
    tag: String,
    /// Operations, all anchored to line numbers of that snapshot.
    ops: Vec<Op>,
}

/// What one operation does to the lines it names.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Kind {
    /// Replace the line or range with the text.
    Replace,
    /// Put the text before the line.
    InsertBefore,
    /// Put the text after the line.
    InsertAfter,
    /// Remove the line or range.
    Delete,
}

/// One operation of a section.
#[derive(Deserialize)]
struct Op {
    /// What to do.
    op: Kind,
    /// The line to act on, or the first line of the range; 1-indexed.
    line: usize,
    /// The last line of the range, for `replace` and `delete`.
    end: Option<usize>,
    /// The new lines, for `replace`, `insert_before` and `insert_after`.
    text: Option<String>,
}

/// A file read, edited in memory and ready to be written.
struct Pending {
    /// Resolved location of the file.
    path: PathBuf,
    /// The file as the user sees it.
    shown: String,
    /// The file before the edit.
    old: String,
    /// The file after the edit.
    new: String,
    /// The first and one past the last line of `new` that the edit touched, 0-indexed.
    region: (usize, usize),
}

#[async_trait]
impl Tool for Edit {
    /// Returns `edit`.
    fn name(&self) -> &'static str {
        "edit"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        "Change files by line number. Each file section carries the tag from the latest read, \
         grep or edit result for that file, and operations anchored to the line numbers of that \
         snapshot: replace a line or a range, insert before or after a line, delete a line or a \
         range. All line numbers refer to the file as you read it, however many operations \
         come before. If the file changed since the tag was issued the call is rejected and \
         nothing is written; read the file again. A call either applies to every file or to \
         none. A successful edit returns the new tag and a preview of the changed lines, so you \
         can edit again without reading. The user is asked to approve each edit."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "files": {
                    "type": "array",
                    "minItems": 1,
                    "description": "One section per file.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string", "description": "File to edit, relative to the working directory." },
                            "tag": { "type": "string", "description": "Four-digit snapshot tag from the latest read, grep or edit result for this file." },
                            "ops": {
                                "type": "array",
                                "minItems": 1,
                                "description": "Operations anchored to line numbers of the tagged snapshot. They must not overlap.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "op": { "type": "string", "enum": ["replace", "insert_before", "insert_after", "delete"] },
                                        "line": { "type": "integer", "minimum": 1, "description": "The line to act on; the first line of a range. insert_after accepts 0 to insert at the top." },
                                        "end": { "type": "integer", "minimum": 1, "description": "Last line of the range, for replace and delete. Defaults to line." },
                                        "text": { "type": "string", "description": "The new lines, separated by newlines, without line numbers. Required except for delete; an empty string is one blank line, so use delete to remove lines." }
                                    },
                                    "required": ["op", "line"]
                                }
                            }
                        },
                        "required": ["path", "tag", "ops"]
                    }
                }
            },
            "required": ["files"]
        })
    }

    /// Edits change files, so they need approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows each file with the lines the operations remove and add.
    fn describe(&self, args: &Value) -> String {
        let Ok(args) = serde_json::from_value::<Args>(args.clone()) else {
            return args.to_string();
        };
        args.files
            .iter()
            .map(|section| self.describe_section(section))
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Applies the edits on a blocking thread, then has the language service format the files and say what is wrong with them.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let sandbox = self.sandbox.clone();
        let mut pending = tokio::task::spawn_blocking(move || apply(&sandbox, args))
            .await
            .map_err(|e| ToolError::Failed(e.to_string()))??;
        let mut written: Vec<Written> = pending
            .iter()
            .map(|item| Written {
                path: item.path.clone(),
                shown: item.shown.clone(),
                text: item.new.clone(),
                formatted: false,
            })
            .collect();
        let diagnostics = settle(&self.languages, &mut written).await;
        for (item, file) in pending.iter_mut().zip(&written) {
            item.new.clone_from(&file.text);
        }
        Ok(report(&pending, &written, &diagnostics))
    }
}

impl Edit {
    /// Describes one section: the path, then each operation with the old lines it removes and the new lines it adds.
    fn describe_section(&self, section: &Section) -> String {
        let shown = self.sandbox.describe(&section.path);
        let old = self
            .sandbox
            .resolve(&section.path)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| text.lines().map(str::to_string).collect::<Vec<_>>())
            .unwrap_or_default();
        let mut out = vec![shown];
        for op in &section.ops {
            let end = op.end.unwrap_or(op.line).max(op.line);
            let count = (end - op.line).saturating_add(1);
            let verb = match op.op {
                Kind::Replace => format!("replace lines {}-{end}", op.line),
                Kind::Delete => format!("delete lines {}-{end}", op.line),
                Kind::InsertBefore => format!("insert before line {}", op.line),
                Kind::InsertAfter => format!("insert after line {}", op.line),
            };
            out.push(verb);
            if matches!(op.op, Kind::Replace | Kind::Delete) {
                let removed = old
                    .iter()
                    .skip(op.line.saturating_sub(1))
                    .take(count)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !removed.is_empty() {
                    out.push(marked_lines('-', &removed));
                }
            }
            if let Some(text) = &op.text {
                out.push(marked_lines('+', text));
            }
        }
        out.join("\n")
    }
}

/// Edits every section in memory and, only when all of them succeed, writes the files; returns what was written.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when a file is not text, its tag is missing or
/// stale, a section repeats a file, or an operation is out of range, overlaps
/// another or lacks its text; [`ToolError::OutsidePath`] for a path outside the
/// sandbox. Nothing is written in any of these cases.
fn apply(sandbox: &Sandbox, args: Args) -> Result<Vec<Pending>, ToolError> {
    if args.files.is_empty() {
        return Err(ToolError::Failed("files must not be empty".into()));
    }
    let mut seen = HashSet::new();
    let mut pending = Vec::new();
    for section in &args.files {
        let path = sandbox.resolve(&section.path)?;
        if !seen.insert(path.clone()) {
            return Err(ToolError::Failed(format!(
                "{} appears in more than one section; put all its operations in one section",
                section.path
            )));
        }
        pending.push(prepare(sandbox, path, section)?);
    }
    for item in &pending {
        if std::fs::read(&item.path).map(|b| tag(&b)).ok().as_deref()
            != Some(tag(item.old.as_bytes()).as_str())
        {
            return Err(ToolError::Failed(format!(
                "{} changed while the edit was being prepared; read the file again and redo \
                 the edit",
                item.shown
            )));
        }
    }
    write_all(&pending)?;
    Ok(pending)
}

/// The result of an edit that wrote `pending`, whose files are now as `written` says, with the `diagnostics` block after the files when there is one: each file's tag and a preview of the region it changed, noting the files the language service formatted.
fn report(pending: &[Pending], written: &[Written], diagnostics: &str) -> ToolOutput {
    let mut report = Vec::new();
    let mut changes = Vec::new();
    for (item, file) in pending.iter().zip(written) {
        let formatted = if file.formatted {
            " (formatted by the language server)"
        } else {
            ""
        };
        report.push(format!(
            "{}: tag {}{formatted}\n{}",
            item.shown,
            tag(item.new.as_bytes()),
            preview(&item.new, item.region)
        ));
        changes.push(FileChange {
            path: item.shown.clone(),
            old: item.old.clone(),
            new: item.new.clone(),
        });
    }
    if !diagnostics.is_empty() {
        report.push(diagnostics.to_owned());
    }
    ToolOutput::new(report.join("\n\n")).with_changes(changes)
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

/// Reads the file of `section`, checks its tag and applies the operations in memory.
///
/// # Errors
///
/// See [`apply`].
fn prepare(sandbox: &Sandbox, path: PathBuf, section: &Section) -> Result<Pending, ToolError> {
    let bytes = std::fs::read(&path)?;
    let old = String::from_utf8(bytes.clone())
        .map_err(|_| ToolError::Failed(format!("{} is not a UTF-8 text file", section.path)))?;
    let current = tag(&bytes);
    if !section.tag.trim().eq_ignore_ascii_case(&current) {
        return Err(ToolError::Failed(format!(
            "{} has changed since it was read (you cited tag {:?}, it is now {current}); \
             read the file again and redo the edit against the new tag and line numbers",
            section.path, section.tag
        )));
    }
    let (new, region) = rewrite(&old, &section.ops)
        .map_err(|message| ToolError::Failed(format!("{}: {message}", section.path)))?;
    Ok(Pending {
        shown: sandbox.display(&path),
        path,
        old,
        new,
        region,
    })
}

/// A file split into lines, remembering how to join them again.
struct Lines {
    /// The lines without their `\n`; a `\r` before it stays, so untouched lines keep their bytes.
    bodies: Vec<String>,
    /// Whether the file uses CRLF, judged by its first line ending; new lines follow it.
    crlf: bool,
    /// Whether the last line ends with a newline.
    final_eol: bool,
    /// Whether the last line of the file ends with a `\r`.
    last_cr: bool,
}

impl Lines {
    /// Splits `text` into lines, noting CRLF endings and a final newline.
    fn split(text: &str) -> Self {
        let crlf = text.find('\n').is_some_and(|i| text[..i].ends_with('\r'));
        let mut bodies: Vec<String> = text.split('\n').map(str::to_string).collect();
        let final_eol = text.ends_with('\n');
        if final_eol || text.is_empty() {
            bodies.pop();
        }
        let last_cr = bodies.last().is_some_and(|b| b.ends_with('\r'));
        Self {
            bodies,
            crlf,
            final_eol,
            last_cr,
        }
    }

    /// Turns the lines of replacement `text` into bodies that follow the file's line endings.
    fn fresh(&self, text: &str) -> Vec<String> {
        let text = text.strip_suffix('\n').unwrap_or(text);
        text.split('\n')
            .map(|line| {
                let line = line.strip_suffix('\r').unwrap_or(line);
                if self.crlf {
                    format!("{line}\r")
                } else {
                    line.to_string()
                }
            })
            .collect()
    }

    /// Joins `bodies` back into text, ending with a newline when the file did.
    fn join(&self, mut bodies: Vec<String>) -> String {
        if !self.final_eol
            && !self.last_cr
            && let Some(last) = bodies.last_mut()
            && last.ends_with('\r')
        {
            last.pop();
        }
        let mut text = bodies.join("\n");
        if (self.final_eol || self.bodies.is_empty()) && !bodies.is_empty() {
            text.push('\n');
        }
        text
    }
}

/// A validated operation on the gaps and lines of a file.
struct Step {
    /// First line it removes, 1-indexed, when it removes any.
    first: usize,
    /// Last line it removes.
    last: usize,
    /// Lines it puts in their place.
    lines: Vec<String>,
}

/// Applies `ops` to `old` and returns the new text and the touched region of it.
///
/// Every operation is checked against the lines of `old`, so line numbers do
/// not shift as earlier operations apply.
///
/// # Errors
///
/// Returns a message when a line is out of range, text is missing, or two
/// operations overlap.
fn rewrite(old: &str, ops: &[Op]) -> Result<(String, (usize, usize)), String> {
    let file = Lines::split(old);
    let total = file.bodies.len();
    let mut ranges: Vec<Step> = Vec::new();
    let mut inserts: Vec<(usize, usize, Vec<String>)> = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let number = index + 1;
        let text = || {
            op.text
                .as_deref()
                .map(|t| file.fresh(t))
                .ok_or_else(|| format!("operation {number} needs text"))
        };
        match op.op {
            Kind::Replace | Kind::Delete => {
                let last = op.end.unwrap_or(op.line);
                if op.line == 0 || op.line > last || last > total {
                    return Err(format!(
                        "operation {number}: lines {}-{last} are outside the file, which has \
                         {total} lines",
                        op.line
                    ));
                }
                let lines = if op.op == Kind::Replace {
                    text()?
                } else {
                    Vec::new()
                };
                ranges.push(Step {
                    first: op.line,
                    last,
                    lines,
                });
            }
            Kind::InsertBefore | Kind::InsertAfter => {
                let gap = if op.op == Kind::InsertBefore {
                    op.line.wrapping_sub(1)
                } else {
                    op.line
                };
                let valid = if op.op == Kind::InsertBefore {
                    (1..=total + 1).contains(&op.line)
                } else {
                    op.line <= total
                };
                if !valid {
                    return Err(format!(
                        "operation {number}: line {} is outside the file, which has {total} lines",
                        op.line
                    ));
                }
                inserts.push((gap, index, text()?));
            }
        }
    }
    ranges.sort_by_key(|step| step.first);
    if let Some(pair) = ranges.windows(2).find(|w| w[1].first <= w[0].last) {
        return Err(format!(
            "operations on lines {}-{} and {}-{} overlap",
            pair[0].first, pair[0].last, pair[1].first, pair[1].last
        ));
    }
    if let Some((gap, _, _)) = inserts
        .iter()
        .find(|(gap, _, _)| ranges.iter().any(|r| r.first <= *gap && *gap < r.last))
    {
        return Err(format!(
            "an insert at line {gap} falls inside a range that another operation replaces or \
             deletes"
        ));
    }
    inserts.sort_by_key(|(gap, index, _)| (*gap, *index));

    let mut out: Vec<String> = Vec::new();
    let mut low = usize::MAX;
    let mut high = 0usize;
    let mut next_insert = inserts.iter().peekable();
    let mut next_range = ranges.iter().peekable();
    let mut line = 1usize;
    loop {
        let gap = line - 1;
        while let Some((_, _, lines)) = next_insert.next_if(|(g, _, _)| *g == gap) {
            low = low.min(out.len());
            high = high.max(out.len() + lines.len());
            out.extend(lines.iter().cloned());
        }
        if line > total {
            break;
        }
        match next_range.next_if(|step| step.first == line) {
            Some(step) => {
                low = low.min(out.len());
                high = high.max(out.len() + step.lines.len());
                out.extend(step.lines.iter().cloned());
                line = step.last + 1;
                for (_, _, lines) in inserts.iter().filter(|(g, _, _)| *g == step.last) {
                    low = low.min(out.len());
                    high = high.max(out.len() + lines.len());
                    out.extend(lines.iter().cloned());
                }
                while next_insert.next_if(|(g, _, _)| *g <= step.last).is_some() {}
            }
            None => {
                out.push(file.bodies[line - 1].clone());
                line += 1;
            }
        }
    }
    let region = if low == usize::MAX {
        (0, 0)
    } else {
        (low, high)
    };
    Ok((file.join(out), region))
}

/// Shows the lines of `text` in `region` with context, numbered.
fn preview(text: &str, region: (usize, usize)) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let from = region.0.saturating_sub(PREVIEW_CONTEXT).min(lines.len());
    let mut to = (region.1 + PREVIEW_CONTEXT).min(lines.len());
    let mut note = "";
    if to - from > PREVIEW_LINES {
        to = from + PREVIEW_LINES;
        note = "\n[preview truncated]";
    }
    if from == to {
        return "[file is now empty]".to_string();
    }
    let body = lines[from..to]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{line}", from + i + 1))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{body}{note}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds an `edit` tool rooted at the scratch directory `name`.
    fn tool(name: &str) -> (Edit, PathBuf) {
        let dir = scratch(name);
        (Edit::new(Sandbox::new(&dir).unwrap()), dir)
    }

    /// Writes `content` to `name` in `dir` and returns its tag.
    fn put(dir: &std::path::Path, name: &str, content: &str) -> String {
        std::fs::write(dir.join(name), content).unwrap();
        tag(content.as_bytes())
    }

    /// Builds a one-file edit call.
    fn call(path: &str, tag: &str, ops: Value) -> Value {
        json!({ "files": [{ "path": path, "tag": tag, "ops": ops }] })
    }

    /// Reads `name` in `dir`.
    fn get(dir: &std::path::Path, name: &str) -> String {
        std::fs::read_to_string(dir.join(name)).unwrap()
    }

    /// Replacing one line changes only that line and returns the new tag and a preview.
    #[tokio::test]
    async fn replaces_a_line() {
        let (tool, dir) = tool("edit-line");
        let t = put(&dir, "f.txt", "a\nb\nc\n");
        let out = tool
            .call(call(
                "f.txt",
                &t,
                json!([{ "op": "replace", "line": 2, "text": "B" }]),
            ))
            .await
            .unwrap();
        assert_eq!(get(&dir, "f.txt"), "a\nB\nc\n");
        assert!(out.text.contains(&format!("tag {}", tag(b"a\nB\nc\n"))));
        assert!(out.text.contains("     2\tB"));
        assert_eq!(out.changes.len(), 1);
        assert_eq!(out.changes[0].old, "a\nb\nc\n");
        assert_eq!(out.changes[0].new, "a\nB\nc\n");
    }

    /// A range is replaced by any number of lines.
    #[tokio::test]
    async fn replaces_a_range() {
        let (tool, dir) = tool("edit-range");
        let t = put(&dir, "f.txt", "a\nb\nc\nd\n");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "replace", "line": 2, "end": 3, "text": "x\ny\nz" }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "a\nx\ny\nz\nd\n");
    }

    /// Inserting before and after a line puts the text on the right side of it, also at both ends of the file.
    #[tokio::test]
    async fn inserts_before_and_after() {
        let (tool, dir) = tool("edit-insert");
        let t = put(&dir, "f.txt", "a\nb\n");
        tool.call(call(
            "f.txt",
            &t,
            json!([
                { "op": "insert_before", "line": 1, "text": "top" },
                { "op": "insert_after", "line": 1, "text": "after a" },
                { "op": "insert_before", "line": 2, "text": "before b" },
                { "op": "insert_after", "line": 2, "text": "bottom" }
            ]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "top\na\nafter a\nbefore b\nb\nbottom\n");
    }

    /// Deleting a line and a range removes exactly those lines, with line numbers from the original file.
    #[tokio::test]
    async fn deletes_lines_and_ranges() {
        let (tool, dir) = tool("edit-delete");
        let t = put(&dir, "f.txt", "1\n2\n3\n4\n5\n6\n");
        let out = tool
            .call(call(
                "f.txt",
                &t,
                json!([
                    { "op": "delete", "line": 2 },
                    { "op": "delete", "line": 4, "end": 5 }
                ]),
            ))
            .await
            .unwrap();
        assert_eq!(get(&dir, "f.txt"), "1\n3\n6\n");
        assert!(out.text.contains("tag"));
    }

    /// Two separate lines change in one call.
    #[tokio::test]
    async fn changes_two_separate_lines() {
        let (tool, dir) = tool("edit-two");
        let t = put(&dir, "f.txt", "a\nb\nc\nd\ne\n");
        tool.call(call(
            "f.txt",
            &t,
            json!([
                { "op": "replace", "line": 1, "text": "A" },
                { "op": "replace", "line": 5, "text": "E" }
            ]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "A\nb\nc\nd\nE\n");
    }

    /// Line endings and a missing final newline survive.
    #[tokio::test]
    async fn keeps_crlf_and_missing_final_newline() {
        let (tool, dir) = tool("edit-eol");
        let t = put(&dir, "f.txt", "a\r\nb\r\nc");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "replace", "line": 2, "text": "x\ny" }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "a\r\nx\r\ny\r\nc");
    }

    /// Text can be inserted into an empty file.
    #[tokio::test]
    async fn inserts_into_an_empty_file() {
        let (tool, dir) = tool("edit-empty");
        let t = put(&dir, "f.txt", "");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "insert_before", "line": 1, "text": "hello" }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "hello\n");
    }

    /// A tag that no longer matches is rejected and the file is untouched.
    #[tokio::test]
    async fn stale_tag_is_rejected() {
        let (tool, dir) = tool("edit-stale");
        let t = put(&dir, "f.txt", "a\nb\n");
        std::fs::write(dir.join("f.txt"), "a\nchanged\n").unwrap();
        let result = tool
            .call(call(
                "f.txt",
                &t,
                json!([{ "op": "replace", "line": 1, "text": "x" }]),
            ))
            .await;
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("read the file again")));
        assert_eq!(get(&dir, "f.txt"), "a\nchanged\n");
    }

    /// A missing tag is rejected before anything is written.
    #[tokio::test]
    async fn missing_tag_is_rejected() {
        let (tool, dir) = tool("edit-notag");
        put(&dir, "f.txt", "a\n");
        let result = tool
            .call(json!({ "files": [{ "path": "f.txt", "ops": [{ "op": "delete", "line": 1 }] }] }))
            .await;
        assert!(matches!(result, Err(ToolError::InvalidArguments(_))));
        assert_eq!(get(&dir, "f.txt"), "a\n");
    }

    /// When one of several files fails, none of them changes.
    #[tokio::test]
    async fn a_failing_section_changes_nothing() {
        let (tool, dir) = tool("edit-atomic");
        let a = put(&dir, "a.txt", "a\n");
        put(&dir, "b.txt", "b\n");
        let result = tool
            .call(json!({ "files": [
                { "path": "a.txt", "tag": a, "ops": [{ "op": "replace", "line": 1, "text": "A" }] },
                { "path": "b.txt", "tag": "0000", "ops": [{ "op": "replace", "line": 1, "text": "B" }] }
            ] }))
            .await;
        assert!(result.is_err());
        assert_eq!(get(&dir, "a.txt"), "a\n");
        assert_eq!(get(&dir, "b.txt"), "b\n");
    }

    /// Several files change in one successful call and each is reported.
    #[tokio::test]
    async fn edits_several_files() {
        let (tool, dir) = tool("edit-multi");
        let a = put(&dir, "a.txt", "a\n");
        let b = put(&dir, "b.txt", "b\n");
        let out = tool
            .call(json!({ "files": [
                { "path": "a.txt", "tag": a, "ops": [{ "op": "replace", "line": 1, "text": "A" }] },
                { "path": "b.txt", "tag": b, "ops": [{ "op": "replace", "line": 1, "text": "B" }] }
            ] }))
            .await
            .unwrap();
        assert_eq!(get(&dir, "a.txt"), "A\n");
        assert_eq!(get(&dir, "b.txt"), "B\n");
        assert_eq!(out.changes.len(), 2);
    }

    /// Bad ranges, overlaps, missing text and repeated files are rejected without a write.
    #[tokio::test]
    async fn rejects_bad_operations() {
        let (tool, dir) = tool("edit-bad");
        let t = put(&dir, "f.txt", "a\nb\nc\n");
        for ops in [
            json!([{ "op": "delete", "line": 4 }]),
            json!([{ "op": "delete", "line": 3, "end": 2 }]),
            json!([{ "op": "replace", "line": 1 }]),
            json!([{ "op": "insert_before", "line": 5, "text": "x" }]),
            json!([
                { "op": "delete", "line": 1, "end": 2 },
                { "op": "replace", "line": 2, "text": "x" }
            ]),
            json!([
                { "op": "delete", "line": 1, "end": 3 },
                { "op": "insert_after", "line": 1, "text": "x" }
            ]),
        ] {
            assert!(tool.call(call("f.txt", &t, ops)).await.is_err());
            assert_eq!(get(&dir, "f.txt"), "a\nb\nc\n");
        }
        let twice = tool
            .call(json!({ "files": [
                { "path": "f.txt", "tag": t, "ops": [{ "op": "delete", "line": 1 }] },
                { "path": "f.txt", "tag": t, "ops": [{ "op": "delete", "line": 2 }] }
            ] }))
            .await;
        assert!(twice.is_err());
        assert_eq!(get(&dir, "f.txt"), "a\nb\nc\n");
    }

    /// Paths outside the sandbox are rejected.
    #[tokio::test]
    async fn rejects_escapes() {
        let (tool, _dir) = tool("edit-escape");
        let result = tool
            .call(call(
                "../outside.txt",
                "0000",
                json!([{ "op": "delete", "line": 1 }]),
            ))
            .await;
        assert!(matches!(result, Err(ToolError::OutsidePath(_))));
    }

    /// The returned tag can be used for the next edit without reading.
    #[tokio::test]
    async fn chained_edits_use_the_returned_tag() {
        let (tool, dir) = tool("edit-chain");
        let t = put(&dir, "f.txt", "a\nb\n");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "insert_after", "line": 2, "text": "c" }]),
        ))
        .await
        .unwrap();
        let next = tag(b"a\nb\nc\n");
        tool.call(call("f.txt", &next, json!([{ "op": "delete", "line": 1 }])))
            .await
            .unwrap();
        assert_eq!(get(&dir, "f.txt"), "b\nc\n");
    }

    /// The approval prompt shows the old and new lines.
    #[test]
    fn describe_shows_old_and_new_lines() {
        let (tool, dir) = tool("edit-describe");
        put(&dir, "f.txt", "a\nb\n");
        let shown = tool.describe(&call(
            "f.txt",
            "0000",
            json!([{ "op": "replace", "line": 2, "text": "B" }]),
        ));
        assert!(shown.contains("replace lines 2-2"));
        assert!(shown.contains("- b"));
        assert!(shown.contains("+ B"));
    }

    /// The schema and description no longer mention the old string form.
    #[test]
    fn old_form_is_gone() {
        let (tool, _dir) = tool("edit-schema");
        let text = format!("{} {}", tool.description(), tool.schema());
        assert!(!text.contains("old_string") && !text.contains("new_string"));
    }

    /// File permissions survive an edit.
    #[cfg(unix)]
    #[tokio::test]
    async fn keeps_file_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let (tool, dir) = tool("edit-mode");
        let t = put(&dir, "run.sh", "echo a\n");
        let path = dir.join("run.sh");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        tool.call(call(
            "run.sh",
            &t,
            json!([{ "op": "replace", "line": 1, "text": "echo b" }]),
        ))
        .await
        .unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    /// A file with mixed line endings keeps every untouched line byte for byte, and CRLF in the text is not doubled.
    #[tokio::test]
    async fn mixed_endings_and_crlf_text_are_kept() {
        let (tool, dir) = tool("edit-mixed");
        let t = put(&dir, "f.txt", "a\r\nb\nc\r");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "replace", "line": 1, "text": "x\r\ny" }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "x\r\ny\r\nb\nc\r");
    }

    /// An absurd range end is a clean error and describe does not panic.
    #[tokio::test]
    async fn huge_range_end_is_an_error() {
        let (tool, dir) = tool("edit-huge");
        let t = put(&dir, "f.txt", "a\n");
        let args = call(
            "f.txt",
            &t,
            json!([{ "op": "delete", "line": 1, "end": u64::MAX }]),
        );
        assert!(tool.describe(&args).contains("delete lines 1-"));
        assert!(tool.call(args).await.is_err());
        assert_eq!(get(&dir, "f.txt"), "a\n");
    }

    /// When a later write fails, files already written are put back.
    #[test]
    fn failed_write_rolls_back_earlier_files() {
        let dir = scratch("edit-rollback");
        std::fs::write(dir.join("a.txt"), "old\n").unwrap();
        let item = |path: PathBuf| Pending {
            shown: "x".into(),
            path,
            old: "old\n".into(),
            new: "new\n".into(),
            region: (0, 1),
        };
        let pending = vec![item(dir.join("a.txt")), item(dir.join("missing/b.txt"))];
        assert!(write_all(&pending).is_err());
        assert_eq!(get(&dir, "a.txt"), "old\n");
    }

    /// A language service formats the edited file: the file on disk holds the formatted text, the tag reported is the tag of that text, so the next edit can use it, and what the service says is wrong follows.
    #[tokio::test]
    async fn formats_and_reports_diagnostics() {
        use crate::language::tests::{Fake, diagnostic};
        use crate::{Languages, Severity};
        let dir = scratch("edit-languages");
        let fake = Fake {
            formats: true,
            ..Fake::default()
        };
        fake.diagnostics.lock().unwrap().push(diagnostic(
            &dir.join("f.rs"),
            2,
            Severity::Error,
            "cannot find value",
        ));
        let tool = Edit::with_languages(
            Sandbox::new(&dir).unwrap(),
            Languages::new(Some(std::sync::Arc::new(fake))),
        );
        let t = put(&dir, "f.rs", "a\nb\n");
        let out = tool
            .call(call(
                "f.rs",
                &t,
                json!([{ "op": "replace", "line": 2, "text": "c" }]),
            ))
            .await
            .unwrap();
        assert_eq!(get(&dir, "f.rs"), "A\nC\n");
        let on_disk = tag(get(&dir, "f.rs").as_bytes());
        assert!(
            out.text.contains(&format!(
                "f.rs: tag {on_disk} (formatted by the language server)"
            )),
            "{}",
            out.text
        );
        assert!(
            out.text.ends_with(
                "Diagnostics: 1 error(s), 0 warning(s)\nf.rs:2:5 error: cannot find value (rustc)"
            ),
            "{}",
            out.text
        );
        assert_eq!(out.changes[0].new, "A\nC\n");
        tool.call(call(
            "f.rs",
            &on_disk,
            json!([{ "op": "delete", "line": 1 }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.rs"), "C\n");
    }
}
