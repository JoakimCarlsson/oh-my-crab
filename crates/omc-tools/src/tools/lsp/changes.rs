//! Writing the files a language server's answer changes: checking that they stay inside the working directory, showing them for a dry run, writing every file or none, then formatting them and reporting what is wrong with them the way an `edit` does.

use std::path::{Path, PathBuf};

use super::render;
use crate::language::{Languages, Written, settle};
use crate::tools::atomic::write_atomic;
use crate::tools::snapshot::tag;
use crate::{FileChange, Renamed, Sandbox, ToolError, ToolOutput};

/// The most changed lines a dry run shows for each file.
const PREVIEW_LINES: usize = 20;

/// One file about to be changed.
pub(super) struct Pending {
    /// Where it is written.
    pub(super) path: PathBuf,
    /// What it holds now, or nothing when it does not exist yet, as the file a move creates.
    pub(super) old: Option<String>,
    /// What it will hold.
    pub(super) new: String,
    /// How many places in it change.
    pub(super) edits: usize,
}

/// A file a move takes away, which is removed once the files changed are written.
pub(super) struct Moved {
    /// Where it was.
    pub(super) from: PathBuf,
    /// What it held.
    pub(super) text: String,
}

/// The files `changed` holds that differ from their text on disk, each resolved inside the sandbox.
///
/// # Errors
///
/// Returns [`ToolError::Failed`], naming them, when any of them is outside the working directory, saying that `what` changed nothing, and [`ToolError::Io`] when one cannot be read.
pub(super) async fn pending(
    sandbox: &Sandbox,
    changed: Vec<Renamed>,
    what: &str,
) -> Result<Vec<Pending>, ToolError> {
    let mut outside = Vec::new();
    let mut inside = Vec::new();
    for file in changed {
        match sandbox.resolve(&file.path.to_string_lossy()) {
            Ok(path) => inside.push((path, file)),
            Err(_) => outside.push(file.path.display().to_string()),
        }
    }
    if !outside.is_empty() {
        return Err(ToolError::Failed(format!(
            "{what} would change files outside the working directory, so nothing was changed: {}",
            outside.join(", ")
        )));
    }
    let mut pending = Vec::with_capacity(inside.len());
    for (path, file) in inside {
        let old = tokio::fs::read_to_string(&path).await?;
        if old != file.text {
            pending.push(Pending {
                path,
                old: Some(old),
                new: file.text,
                edits: file.edits,
            });
        }
    }
    Ok(pending)
}

/// The block a dry run of `pending` shows: each file with the lines that change in it.
pub(super) fn preview(sandbox: &Sandbox, pending: &[Pending]) -> String {
    let mut text = String::new();
    for file in pending {
        let old = file.old.as_deref().unwrap_or_default();
        let created = if file.old.is_none() { ", created" } else { "" };
        text.push_str(&format!(
            "\n\n{}: {} edit(s){created}{}",
            sandbox.display(&file.path),
            file.edits,
            render::changed_lines(old, &file.new, PREVIEW_LINES)
        ));
    }
    text
}

/// Writes every file of `pending` and removes the file `moved` takes away, or, when one write fails, puts back what was already done; then has the written files formatted and checked, and returns `headline` followed by each file's edits and new snapshot tag and what is wrong with them, with the changes made.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when a file cannot be written or the moved file removed, after putting back what was done.
pub(super) async fn commit(
    sandbox: &Sandbox,
    languages: &Languages,
    headline: String,
    pending: Vec<Pending>,
    moved: Option<Moved>,
) -> Result<ToolOutput, ToolError> {
    write_all(&pending, moved.as_ref())?;
    let mut written: Vec<Written> = pending
        .iter()
        .map(|file| Written {
            path: file.path.clone(),
            shown: sandbox.display(&file.path),
            text: file.new.clone(),
            formatted: false,
        })
        .collect();
    let diagnostics = settle(languages, &mut written).await;
    let mut text = headline;
    if let Some(moved) = &moved {
        text.push_str(&format!("\n{}: moved away", sandbox.display(&moved.from)));
    }
    for (file, done) in pending.iter().zip(&written) {
        let formatted = if done.formatted { ", formatted" } else { "" };
        let created = if file.old.is_none() { ", created" } else { "" };
        text.push_str(&format!(
            "\n{}: {} edit(s){created}{formatted}, tag {}",
            done.shown,
            file.edits,
            tag(done.text.as_bytes())
        ));
    }
    if !diagnostics.is_empty() {
        text.push_str("\n\n");
        text.push_str(&diagnostics);
    }
    let mut changes: Vec<FileChange> = moved
        .into_iter()
        .map(|moved| FileChange {
            path: sandbox.display(&moved.from),
            old: moved.text,
            new: String::new(),
        })
        .collect();
    changes.extend(
        pending
            .into_iter()
            .zip(written)
            .map(|(file, done)| FileChange {
                path: done.shown,
                old: file.old.unwrap_or_default(),
                new: done.text,
            }),
    );
    Ok(ToolOutput::new(text).with_changes(changes))
}

/// Writes the new text of every file in `pending`, creating the folders of a file that does not exist yet, then removes the file `moved` takes away; when one step fails, puts back what was already done.
///
/// # Errors
///
/// Returns the error of the step that failed.
fn write_all(pending: &[Pending], moved: Option<&Moved>) -> Result<(), ToolError> {
    for (done, file) in pending.iter().enumerate() {
        if let Err(error) = write_one(&file.path, &file.new) {
            undo(&pending[..done]);
            return Err(error);
        }
    }
    if let Some(moved) = moved
        && let Err(error) = std::fs::remove_file(&moved.from)
    {
        undo(pending);
        return Err(error.into());
    }
    Ok(())
}

/// Writes `text` to the file at `path`, creating the folders it is in when they do not exist.
///
/// # Errors
///
/// Returns why the folder or the file cannot be written.
fn write_one(path: &Path, text: &str) -> Result<(), ToolError> {
    if let Some(folder) = path.parent()
        && !folder.exists()
    {
        std::fs::create_dir_all(folder)?;
    }
    write_atomic(path, text.as_bytes())
}

/// Puts the files of `done` back as they were: their old text written again, and those that did not exist removed.
fn undo(done: &[Pending]) {
    for file in done {
        match &file.old {
            Some(old) => {
                let _ = write_atomic(&file.path, old.as_bytes());
            }
            None => {
                let _ = std::fs::remove_file(&file.path);
            }
        }
    }
}
