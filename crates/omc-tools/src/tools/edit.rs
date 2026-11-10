//! The `edit` tool: line-anchored edits that fail when the file has changed since it was read.

use std::collections::HashSet;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use super::atomic::write_atomic;
use super::conflict::{self, Take};
use super::read::{Fold, folds};
use super::snapshot::{is_outline_tag, outline_tag, tag};
use super::{generated, lookalike};
use crate::language::{Languages, Written, settle};
use crate::tool::{marked_lines, parse_args};
use crate::{FileChange, Sandbox, Subject, Tool, ToolError, ToolOutput};

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
    /// Snapshot tag from the latest `read`, `grep` or `edit` result for the file; empty when the model gave none, which only a section of inserts at the top may do.
    #[serde(default)]
    tag: String,
    /// Operations, all anchored to line numbers of that snapshot.
    ops: Vec<Op>,
    /// Whether the user explicitly asked for this file to be changed although it is generated.
    #[serde(default)]
    generated: bool,
}

/// What one operation does, as the model names it.
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
    /// Replace a merge conflict block with one of its sides or the text.
    Resolve,
}

/// One operation of a section, as the model sends it.
#[derive(Deserialize)]
struct Op {
    /// What to do.
    op: Kind,
    /// The line to act on, or the first line of the range; 1-indexed. Required except for `resolve`.
    line: Option<usize>,
    /// The last line of the range, for `replace` and `delete`.
    end: Option<usize>,
    /// The new lines, for `replace`, `insert_before` and `insert_after`, or what a `resolve` puts in place of the block.
    text: Option<String>,
    /// The number of the merge conflict a `resolve` replaces.
    conflict: Option<usize>,
    /// The side of the conflict a `resolve` keeps.
    take: Option<Take>,
}

/// What an operation does to lines, once a `resolve` has been turned into the lines it replaces.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    /// Replace the line or range with the text.
    Replace,
    /// Put the text before the line.
    InsertBefore,
    /// Put the text after the line.
    InsertAfter,
    /// Remove the line or range.
    Delete,
}

/// An operation anchored to concrete lines of the snapshot.
struct Placed {
    /// What to do.
    action: Action,
    /// The line to act on, or the first line of the range; 1-indexed.
    line: usize,
    /// The last line of the range, for replace and delete.
    end: Option<usize>,
    /// The new lines, for replace and the inserts.
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
    /// Whether the edit cited an outline's tag, so that the tag it returns is
    /// an outline tag too and the lines folded away stay locked.
    outlined: bool,
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
         come before. With the tag of an outline (it starts with O) only the lines it showed \
         can be changed: read a folded range first. An edit made with an outline's tag returns \
         the outline tag of the new contents, so folded lines stay locked until a read of their \
         range shows them. If the file changed since the tag was issued the call is rejected and \
         nothing is written; read the file again. A call either applies to every file or to \
         none. A successful edit returns the new tag and a preview of the changed lines, so you \
         can edit again without reading. An insert at line 0 puts text at the top of a file and \
         needs no tag, so a line can be added to the top of files that were never read. The \
         user is asked to approve each edit.\n\
         Merge conflicts: the resolve operation replaces the whole block of conflict N, \
         markers included, with one side or with your text; N is the number read lists the \
         conflicts by.\n\
         Generated files and lockfiles are refused: change their source or generator instead, \
         unless the user asked to change that file itself."
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
                            "tag": { "type": "string", "description": "Snapshot tag from the latest read, grep or edit result for this file: four hex digits, or O and four hex digits for an outline. Leave it out only when every operation of the section is an insert at line 0."},
                            "ops": {
                                "type": "array",
                                "minItems": 1,
                                "description": "Operations anchored to line numbers of the tagged snapshot. They must not overlap.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "op": { "type": "string", "enum": ["replace", "insert_before", "insert_after", "delete", "resolve"] },
                                        "line": { "type": "integer", "minimum": 0, "description": "The line to act on; the first line of a range. Required except for resolve. An insert at line 0, before or after, puts the text at the top of the file." },
                                        "end": { "type": "integer", "minimum": 1, "description": "Last line of the range, for replace and delete. Defaults to line." },
                                        "text": { "type": "string", "description": "The new lines, separated by newlines, without line numbers; each newline at the end adds a blank line. Required for replace and the inserts; an empty string is one blank line, so use delete to remove lines. For resolve, the lines that replace the conflict block, instead of take." },
                                        "conflict": { "type": "integer", "minimum": 1, "description": "For resolve: the number of the merge conflict, as read lists them." },
                                        "take": { "type": "string", "enum": ["ours", "theirs", "base", "both"], "description": "For resolve: the side of the conflict to keep; both is ours then theirs." }
                                    },
                                    "required": ["op"]
                                }
                            },
                            "generated": { "type": "boolean", "description": "Allow changing a generated file or lockfile. Only when the user explicitly asked to change this file itself." }
                        },
                        "required": ["path", "ops"]
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

    /// Every file the call edits, where each points.
    fn subject(&self, args: &Value) -> Subject {
        let files = args["files"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        Subject::Files(
            files
                .iter()
                .map(|section| {
                    self.sandbox
                        .locate(section["path"].as_str().unwrap_or_default())
                })
                .collect(),
        )
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
        let text = self
            .sandbox
            .resolve(&section.path)
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .unwrap_or_default();
        let old: Vec<&str> = text.lines().collect();
        let mut out = vec![shown];
        if section.generated {
            out.push("(changes a generated file, as the user asked)".to_owned());
        }
        let placed = match place(&text, &section.ops) {
            Ok(placed) => placed,
            Err(message) => {
                out.push(format!("[will be rejected: {message}]"));
                return out.join("\n");
            }
        };
        for (op, step) in section.ops.iter().zip(&placed) {
            let end = step.end.unwrap_or(step.line).max(step.line);
            let count = (end - step.line).saturating_add(1);
            let verb = match (op.op, step.action) {
                (Kind::Resolve, _) => format!(
                    "resolve conflict {} at lines {}-{end}, keeping {}",
                    op.conflict.unwrap_or_default(),
                    step.line,
                    op.take.map_or("the given text", Take::word)
                ),
                (_, Action::Replace) => format!("replace lines {}-{end}", step.line),
                (_, Action::Delete) => format!("delete lines {}-{end}", step.line),
                (_, Action::InsertBefore) => format!("insert before line {}", step.line),
                (_, Action::InsertAfter) => format!("insert after line {}", step.line),
            };
            out.push(verb);
            if matches!(step.action, Action::Replace | Action::Delete) {
                let removed = old
                    .iter()
                    .skip(step.line.saturating_sub(1))
                    .take(count)
                    .copied()
                    .collect::<Vec<_>>()
                    .join("\n");
                if !removed.is_empty() {
                    out.push(marked_lines('-', &removed));
                }
            }
            if let Some(text) = &step.text {
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
        let (next, note) = if item.outlined {
            (
                outline_tag(item.new.as_bytes()),
                " (an outline tag, as the edit cited one: lines the outline folds stay locked; \
                 read a folded range to edit inside it)",
            )
        } else {
            (tag(item.new.as_bytes()), "")
        };
        report.push(format!(
            "{}: tag {next}{note}{formatted}\n{}",
            item.shown,
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
    lookalike::require(sandbox, &path, &section.path)?;
    let bytes = std::fs::read(&path)?;
    let old = String::from_utf8(bytes.clone())
        .map_err(|_| ToolError::Failed(format!("{} is not a UTF-8 text file", section.path)))?;
    generated::refuse(
        &path,
        &section.path,
        &old,
        section.generated,
        "\"generated\": true in this file's section",
    )?;
    let cited = section.tag.trim();
    let outlined = is_outline_tag(cited);
    let current = if outlined {
        outline_tag(&bytes)
    } else {
        tag(&bytes)
    };
    if cited.is_empty() && !only_tops(&section.ops) {
        return Err(ToolError::InvalidArguments(format!(
            "{} needs the tag from the latest read, grep or edit result for it; only a section \
             whose every operation is an insert at line 0 may leave the tag out",
            section.path
        )));
    }
    if !cited.is_empty() && !cited.eq_ignore_ascii_case(&current) {
        return Err(ToolError::Failed(format!(
            "{} has changed since it was read (you cited tag {:?}, it is now {current}); \
             read the file again and redo the edit against the new tag and line numbers",
            section.path, section.tag
        )));
    }
    let ops = place(&old, &section.ops)
        .map_err(|message| ToolError::Failed(format!("{}: {message}", section.path)))?;
    if outlined {
        refuse_folded(&path, &old, section, &ops)?;
    }
    let (new, region) = rewrite(&old, &ops)
        .map_err(|message| ToolError::Failed(format!("{}: {message}", section.path)))?;
    Ok(Pending {
        shown: sandbox.display(&path),
        path,
        old,
        new,
        region,
        outlined,
    })
}

/// Whether every operation in `ops` is an insert at line 0, which puts text at the top of the file whatever the file holds, so that it needs no snapshot to be anchored to.
fn only_tops(ops: &[Op]) -> bool {
    ops.iter()
        .all(|op| matches!(op.op, Kind::InsertBefore | Kind::InsertAfter) && op.line == Some(0))
}

/// Refuses the operations `ops` of `section`, placed on the lines of the
/// snapshot, that act on a line the outline of the file at `path`, holding
/// `old`, folded away, since the model cited the tag of that outline and has
/// not seen those lines.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] naming the first such operation, the folded
/// range it touches and the selector that reads it.
fn refuse_folded(
    path: &std::path::Path,
    old: &str,
    section: &Section,
    ops: &[Placed],
) -> Result<(), ToolError> {
    let hidden = folds(path, old).unwrap_or_default();
    for (index, op) in ops.iter().enumerate() {
        let last = match op.action {
            Action::Replace | Action::Delete => op.end.unwrap_or(op.line).max(op.line),
            Action::InsertBefore | Action::InsertAfter => op.line,
        };
        if let Some(fold) = hidden
            .iter()
            .find(|fold| fold.first <= last && op.line <= fold.last)
        {
            return Err(folded_error(section, index + 1, *fold));
        }
    }
    Ok(())
}

/// The error for operation `number` of `section`, which touches the lines of
/// `fold` that the outline the model read left out.
fn folded_error(section: &Section, number: usize, fold: Fold) -> ToolError {
    ToolError::Failed(format!(
        "{path}: operation {number} touches lines {first}-{last}, which the outline you read \
         folded away; read {path}:{first}-{last} first and edit with the tag that read returns",
        path = section.path,
        first = fold.first,
        last = fold.last,
    ))
}

/// Anchors `ops` to the lines of `old`: a `resolve` becomes a replace, or a
/// delete when nothing is kept, of its whole conflict block; the other
/// operations keep the lines they name.
///
/// # Errors
///
/// Returns a message naming the operation when it lacks its line, or when a
/// `resolve` is malformed as [`resolve`] says.
fn place(old: &str, ops: &[Op]) -> Result<Vec<Placed>, String> {
    let conflicts = if ops.iter().any(|op| op.op == Kind::Resolve) {
        conflict::scan(old)
    } else {
        Vec::new()
    };
    let lines: Vec<&str> = old
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    ops.iter()
        .enumerate()
        .map(|(index, op)| {
            let number = index + 1;
            let action = match op.op {
                Kind::Replace => Action::Replace,
                Kind::InsertBefore => Action::InsertBefore,
                Kind::InsertAfter => Action::InsertAfter,
                Kind::Delete => Action::Delete,
                Kind::Resolve => return resolve(op, number, &conflicts, &lines),
            };
            let line = op
                .line
                .ok_or_else(|| format!("operation {number} needs line"))?;
            Ok(Placed {
                action,
                line,
                end: op.end,
                text: op.text.clone(),
            })
        })
        .collect()
}

/// Turns the `resolve` operation `op`, number `number`, into the replace or
/// delete of the whole block of its conflict among `conflicts`, found in the
/// file whose lines are `lines`.
///
/// # Errors
///
/// Returns a message when the operation names no conflict of the file, gives
/// both or neither of `take` and `text`, or takes a side its block lacks.
fn resolve(
    op: &Op,
    number: usize,
    conflicts: &[conflict::Conflict],
    lines: &[&str],
) -> Result<Placed, String> {
    let id = op.conflict.ok_or_else(|| {
        format!("operation {number}: resolve needs conflict, the number of the conflict to resolve")
    })?;
    let block = id
        .checked_sub(1)
        .and_then(|index| conflicts.get(index))
        .ok_or_else(|| {
            format!(
                "operation {number}: there is no conflict {id}; this snapshot of the file has {} \
                 (read the file with the :conflicts selector to list them)",
                conflicts.len()
            )
        })?;
    let kept = match (op.take, &op.text) {
        (Some(take), None) => {
            let kept = block
                .resolve(lines, take)
                .map_err(|message| format!("operation {number}: {message}"))?;
            (!kept.is_empty()).then(|| kept.join("\n"))
        }
        (None, Some(text)) => (!text.is_empty()).then(|| text.clone()),
        _ => {
            return Err(format!(
                "operation {number}: resolve takes either take or text, not both and not neither"
            ));
        }
    };
    Ok(Placed {
        action: if kept.is_some() {
            Action::Replace
        } else {
            Action::Delete
        },
        line: block.start,
        end: Some(block.end),
        text: kept,
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

    /// Turns the lines of replacement `text`, separated by newlines, into bodies that follow the file's line endings; a newline at the end of `text` separates its last line from one more, empty line.
    fn fresh(&self, text: &str) -> Vec<String> {
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
fn rewrite(old: &str, ops: &[Placed]) -> Result<(String, (usize, usize)), String> {
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
        match op.action {
            Action::Replace | Action::Delete => {
                let last = op.end.unwrap_or(op.line);
                if op.line == 0 || op.line > last || last > total {
                    return Err(format!(
                        "operation {number}: lines {}-{last} are outside the file, which has \
                         {total} lines",
                        op.line
                    ));
                }
                let lines = if op.action == Action::Replace {
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
            Action::InsertBefore | Action::InsertAfter => {
                let gap = if op.action == Action::InsertBefore {
                    op.line.saturating_sub(1)
                } else {
                    op.line
                };
                let valid = if op.action == Action::InsertBefore {
                    op.line <= total + 1
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

    /// Each newline at the end of the text adds a blank line, so that a block can be inserted with the blank lines that set it apart.
    #[tokio::test]
    async fn newlines_at_the_end_add_blank_lines() {
        let (tool, dir) = tool("edit-trailing-newlines");
        let t = put(&dir, "f.txt", "a\nb\n");
        tool.call(call(
            "f.txt",
            &t,
            json!([{ "op": "insert_after", "line": 1, "text": "x\n\n" }]),
        ))
        .await
        .unwrap();
        assert_eq!(get(&dir, "f.txt"), "a\nx\n\n\nb\n");
    }

    /// An insert at line 0 puts the text at the top of the file, before or after, and the schema lets line 0 through.
    #[tokio::test]
    async fn inserts_at_line_zero_go_to_the_top() {
        let (tool, dir) = tool("edit-insert-zero");
        for op in ["insert_before", "insert_after"] {
            let t = put(&dir, "f.txt", "a\nb\n");
            let args = call("f.txt", &t, json!([{ "op": op, "line": 0, "text": "top" }]));
            assert!(crate::schema::violations(&tool.schema(), &args, "args").is_empty());
            tool.call(args).await.unwrap();
            assert_eq!(get(&dir, "f.txt"), "top\na\nb\n");
        }
    }

    /// A section of inserts at line 0 needs no tag, also for a file that was never read, while any other operation without a tag is refused and writes nothing.
    #[tokio::test]
    async fn inserts_at_the_top_need_no_tag() {
        let (tool, dir) = tool("edit-top-untagged");
        put(&dir, "a.txt", "a\n");
        put(&dir, "b.txt", "b\n");
        let args = json!({ "files": [
            { "path": "a.txt", "ops": [{ "op": "insert_before", "line": 0, "text": "top" }] },
            { "path": "b.txt", "ops": [{ "op": "insert_after", "line": 0, "text": "top" }] }
        ]});
        assert!(crate::schema::violations(&tool.schema(), &args, "args").is_empty());
        tool.call(args).await.unwrap();
        assert_eq!(get(&dir, "a.txt"), "top\na\n");
        assert_eq!(get(&dir, "b.txt"), "top\nb\n");
        let refused = tool
            .call(json!({ "files": [
                { "path": "a.txt", "ops": [{ "op": "insert_after", "line": 1, "text": "x" }] }
            ]}))
            .await
            .unwrap_err();
        assert!(refused.to_string().contains("needs the tag"));
        assert_eq!(get(&dir, "a.txt"), "top\na\n");
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

    /// With the tag of an outline, lines the outline showed can be edited and
    /// folded lines cannot until a read shows them.
    #[tokio::test]
    async fn outline_tags_keep_edits_off_folded_lines() {
        let (tool, dir) = tool("edit-outline");
        let mut code = String::from("use std::fmt;\n\n");
        for index in 0..20 {
            code.push_str(&format!("pub fn f{index}(x: u32) -> u32 {{\n"));
            code.push_str(&"    let y = x + 1;\n".repeat(10));
            code.push_str("    y\n}\n\n");
        }
        put(&dir, "a.rs", &code);
        let read = crate::tools::Read::new(Sandbox::new(&dir).unwrap());
        let outline = read.call(json!({ "path": "a.rs" })).await.unwrap().text;
        assert!(outline.contains("… 11 lines folded (4-14)"), "{outline}");
        let outline_tag = outline[5..10].to_owned();
        assert_eq!(outline_tag, super::outline_tag(code.as_bytes()));
        let folded = tool
            .call(call(
                "a.rs",
                &outline_tag,
                json!([{ "op": "replace", "line": 10, "text": "    let z = 0;" }]),
            ))
            .await;
        assert!(
            matches!(&folded, Err(ToolError::Failed(m)) if m.contains("lines 4-14") && m.contains("read a.rs:4-14")),
            "{folded:?}"
        );
        let straddling = tool
            .call(call(
                "a.rs",
                &outline_tag,
                json!([{ "op": "delete", "line": 3, "end": 15 }]),
            ))
            .await;
        assert!(straddling.is_err());
        assert_eq!(get(&dir, "a.rs"), code);
        tool.call(call(
            "a.rs",
            &outline_tag,
            json!([{ "op": "replace", "line": 3, "text": "pub fn first(x: u32) -> u32 {" }, { "op": "insert_before", "line": 15, "text": "    .min(9)" }]),
        ))
        .await
        .unwrap();
        let edited = get(&dir, "a.rs");
        assert!(
            edited.contains("pub fn first(x: u32) -> u32 {\n    let y"),
            "{edited}"
        );
        assert!(edited.contains("    y\n    .min(9)\n}\n"), "{edited}");
    }

    /// An edit made with an outline's tag returns the outline tag of the new
    /// contents, so a chained edit still cannot touch folded lines; a read
    /// of the folded range unlocks them.
    #[tokio::test]
    async fn chained_edits_keep_folds_locked() {
        let (tool, dir) = tool("edit-outline-chain");
        let mut code = String::from("use std::fmt;\n\n");
        for index in 0..20 {
            code.push_str(&format!("pub fn f{index}(x: u32) -> u32 {{\n"));
            code.push_str(&"    let y = x + 1;\n".repeat(10));
            code.push_str("    y\n}\n\n");
        }
        put(&dir, "a.rs", &code);
        let read = crate::tools::Read::new(Sandbox::new(&dir).unwrap());
        let outline = read.call(json!({ "path": "a.rs" })).await.unwrap().text;
        let first = outline[5..10].to_owned();
        let out = tool
            .call(call(
                "a.rs",
                &first,
                json!([{ "op": "replace", "line": 1, "text": "use std::fmt::Debug;" }]),
            ))
            .await
            .unwrap()
            .text;
        let edited = get(&dir, "a.rs");
        let next = super::outline_tag(edited.as_bytes());
        assert!(
            out.starts_with(&format!("a.rs: tag {next} (an outline tag")),
            "{out}"
        );
        let folded = tool
            .call(call(
                "a.rs",
                &next,
                json!([{ "op": "replace", "line": 8, "text": "    let v3 = 0;" }]),
            ))
            .await;
        assert!(
            matches!(&folded, Err(ToolError::Failed(m)) if m.contains("folded away")),
            "{folded:?}"
        );
        let ranged = read
            .call(json!({ "path": "a.rs:4-14" }))
            .await
            .unwrap()
            .text;
        let plain = ranged[5..9].to_owned();
        assert_eq!(plain, tag(edited.as_bytes()));
        assert!(
            tool.call(call(
                "a.rs",
                &plain,
                json!([{ "op": "replace", "line": 8, "text": "    let v3 = 0;" }]),
            ))
            .await
            .is_ok()
        );
    }

    /// A stale tag is reported as such, before the operations are placed,
    /// rather than as a problem with the conflict a resolve names.
    #[tokio::test]
    async fn stale_tags_are_reported_before_operations() {
        let (tool, dir) = tool("edit-stale-resolve");
        put(&dir, "f.txt", "a\nb\n");
        let error = tool
            .call(call(
                "f.txt",
                "0000",
                json!([{ "op": "resolve", "conflict": 1, "take": "ours" }]),
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("has changed since it was read"), "{error}");
        let outline = tool
            .call(call(
                "f.txt",
                "O0000",
                json!([{ "op": "replace", "line": 1, "text": "x" }]),
            ))
            .await
            .unwrap_err()
            .to_string();
        assert!(outline.contains("it is now O"), "{outline}");
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

    /// A conflict is resolved by number with a side, with both sides or with
    /// new text, replacing the whole block; the other conflict keeps its lines.
    #[tokio::test]
    async fn resolves_conflicts_by_number() {
        use super::super::conflict::tests::TWO;
        let (tool, dir) = tool("edit-resolve");
        for (take, block) in [
            (json!({ "take": "ours" }), "ours 1\n"),
            (json!({ "take": "theirs" }), "theirs 1\ntheirs 2\n"),
            (json!({ "take": "both" }), "ours 1\ntheirs 1\ntheirs 2\n"),
            (json!({ "text": "merged" }), "merged\n"),
            (json!({ "text": "" }), ""),
        ] {
            let t = put(&dir, "f.txt", TWO);
            let mut op = json!({ "op": "resolve", "conflict": 1 });
            op.as_object_mut()
                .unwrap()
                .extend(take.as_object().unwrap().clone());
            tool.call(call("f.txt", &t, json!([op]))).await.unwrap();
            let rest = TWO.split_once(">>>>>>> feature\n").unwrap().1;
            assert_eq!(get(&dir, "f.txt"), format!("top\n{block}{rest}"));
        }
        let t = put(&dir, "f.txt", TWO);
        tool.call(call(
            "f.txt",
            &t,
            json!([
                { "op": "resolve", "conflict": 2, "take": "base" },
                { "op": "replace", "line": 1, "text": "TOP" }
            ]),
        ))
        .await
        .unwrap();
        assert!(get(&dir, "f.txt").starts_with("TOP\n<<<<<<< HEAD\n"));
        assert!(get(&dir, "f.txt").ends_with("middle\nancestor\nend\n"));
    }

    /// Resolving a conflict that does not exist, a side the block lacks, or
    /// with both or neither of take and text fails without a write, as does a
    /// stale tag.
    #[tokio::test]
    async fn rejects_bad_resolutions() {
        use super::super::conflict::tests::TWO;
        let (tool, dir) = tool("edit-resolve-bad");
        let t = put(&dir, "f.txt", TWO);
        for (op, expected) in [
            (
                json!({ "op": "resolve", "conflict": 3, "take": "ours" }),
                "no conflict 3",
            ),
            (
                json!({ "op": "resolve", "conflict": 1, "take": "base" }),
                "no base side",
            ),
            (
                json!({ "op": "resolve", "conflict": 1 }),
                "either take or text",
            ),
            (
                json!({ "op": "resolve", "conflict": 1, "take": "ours", "text": "x" }),
                "either take or text",
            ),
            (json!({ "op": "resolve", "take": "ours" }), "needs conflict"),
            (json!({ "op": "delete" }), "needs line"),
        ] {
            let result = tool.call(call("f.txt", &t, json!([op]))).await;
            assert!(
                matches!(&result, Err(ToolError::Failed(m)) if m.contains(expected)),
                "{result:?}"
            );
        }
        let stale = tool
            .call(call(
                "f.txt",
                "0000",
                json!([{ "op": "resolve", "conflict": 1, "take": "ours" }]),
            ))
            .await;
        assert!(stale.is_err());
        assert_eq!(get(&dir, "f.txt"), TWO);
    }

    /// The approval prompt of a resolution shows the block it removes and the side it keeps.
    #[test]
    fn describes_resolutions() {
        use super::super::conflict::tests::TWO;
        let (tool, dir) = tool("edit-resolve-describe");
        put(&dir, "f.txt", TWO);
        let shown = tool.describe(&call(
            "f.txt",
            "0000",
            json!([{ "op": "resolve", "conflict": 1, "take": "theirs" }]),
        ));
        assert!(
            shown.contains("resolve conflict 1 at lines 2-7, keeping theirs"),
            "{shown}"
        );
        assert!(shown.contains("- <<<<<<< HEAD"), "{shown}");
        assert!(shown.contains("+ theirs 2"), "{shown}");
    }

    /// A generated file is refused, naming the evidence, unless the section allows it.
    #[tokio::test]
    async fn refuses_generated_files_unless_allowed() {
        let (tool, dir) = tool("edit-generated");
        let code = "// Code generated by protoc-gen-go. DO NOT EDIT.\npackage a\n";
        let t = put(&dir, "a.go", code);
        let ops = json!([{ "op": "replace", "line": 2, "text": "package b" }]);
        let refused = tool.call(call("a.go", &t, ops.clone())).await;
        assert!(
            matches!(&refused, Err(ToolError::Failed(m)) if m.contains("a.go is a generated file") && m.contains("line 1:") && m.contains("\"generated\": true")),
            "{refused:?}"
        );
        assert_eq!(get(&dir, "a.go"), code);
        tool.call(
            json!({ "files": [{ "path": "a.go", "tag": t, "ops": ops, "generated": true }] }),
        )
        .await
        .unwrap();
        assert!(get(&dir, "a.go").ends_with("package b\n"));
    }

    /// A missing file is an error naming the file whose path ends with it, never an edit of that file.
    #[tokio::test]
    async fn names_lookalikes_of_missing_files() {
        let (tool, dir) = tool("edit-lookalike");
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        let t = put(&dir, "src/deep/f.txt", "a\n");
        let result = tool
            .call(call(
                "deep/f.txt",
                &t,
                json!([{ "op": "delete", "line": 1 }]),
            ))
            .await;
        assert!(
            matches!(&result, Err(ToolError::Failed(m)) if m.contains("deep/f.txt does not exist; the only file whose path ends with it is src/deep/f.txt")),
            "{result:?}"
        );
        assert_eq!(get(&dir, "src/deep/f.txt"), "a\n");
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
            outlined: false,
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
