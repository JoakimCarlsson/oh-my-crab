//! Changes taken file by file, for subagents that work in a clone of a folder outside any git repository.
//!
//! The folder is cloned once, untouched, as the state every subagent of a call starts from, and each subagent works in a clone of that. When one ends, every file of its clone is compared with the untouched clone: one whose size and modification time match is taken to be unchanged, any other is read and compared byte for byte. Files a `.gitignore` names are left out on both sides, as git would leave them out.
//!
//! The changes are applied to the main agent's folder only when every file they touch there is still as it was when the subagent started, or already as the subagent left it; otherwise none of them are applied, and they are saved as a patch for the main agent to merge by hand.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use omc_tools::Spill;

use crate::worktree::Merge;

/// What a path holds, as far as handing changes back cares.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Content {
    /// Nothing, or something other than a file or a symbolic link.
    Missing,
    /// A regular file with these bytes, and whether it may be run.
    File {
        /// What it holds.
        bytes: Vec<u8>,
        /// Whether any of its execute bits are set.
        executable: bool,
    },
    /// A symbolic link pointing at this.
    Link(PathBuf),
}

/// One file a subagent changed, added or removed.
#[derive(Clone, Debug)]
pub(crate) struct Change {
    /// Where it is, from the root of the folder, with `/` between its parts.
    pub(crate) path: String,
    /// What it holds now.
    pub(crate) after: Content,
}

/// What `path` holds.
///
/// # Errors
///
/// Fails when it exists but cannot be read.
fn content(path: &Path) -> std::io::Result<Content> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Content::Missing),
        Err(error) => return Err(error),
    };
    if meta.file_type().is_symlink() {
        return Ok(Content::Link(std::fs::read_link(path)?));
    }
    if !meta.is_file() {
        return Ok(Content::Missing);
    }
    Ok(Content::File {
        bytes: std::fs::read(path)?,
        executable: executable(&meta),
    })
}

/// Whether the file `meta` describes may be run.
#[cfg(unix)]
fn executable(meta: &std::fs::Metadata) -> bool {
    std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0
}

/// Whether the file `meta` describes may be run: never said on this platform.
#[cfg(not(unix))]
fn executable(_meta: &std::fs::Metadata) -> bool {
    false
}

/// The files and symbolic links under `root`, from it, with `/` between their parts; what a `.gitignore` names is left out.
fn listing(root: &Path) -> BTreeSet<String> {
    ignore::WalkBuilder::new(root)
        .standard_filters(false)
        .git_ignore(true)
        .require_git(false)
        .follow_links(false)
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| !kind.is_dir()))
        .filter_map(|entry| {
            let relative = entry.path().strip_prefix(root).ok()?;
            Some(
                relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        })
        .collect()
}

/// The size and modification time of the regular file at `path`, which a clone keeps from what it was made from, so that matching ones need not be read.
fn stamp(path: &Path) -> Option<(u64, SystemTime)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    meta.is_file()
        .then(|| meta.modified().ok().map(|modified| (meta.len(), modified)))
        .flatten()
}

/// Every file that differs between the folder `before` and the folder `after`, by its path, with what it holds in `after`.
///
/// # Errors
///
/// Fails, saying which, when a file of either folder cannot be read.
pub(crate) fn changes(before: &Path, after: &Path) -> Result<Vec<Change>, String> {
    let mut paths = listing(before);
    paths.extend(listing(after));
    let mut found = Vec::new();
    for path in paths {
        let (old, new) = (before.join(&path), after.join(&path));
        if stamp(&old).is_some_and(|stamp| Some(stamp) == self::stamp(&new)) {
            continue;
        }
        let read = |at: &Path| content(at).map_err(|error| format!("{}: {error}", at.display()));
        let now = read(&new)?;
        if read(&old)? != now {
            found.push(Change { path, after: now });
        }
    }
    Ok(found)
}

/// Applies `changes` to the folder `top` when `finished`, the subagent having ended well, and every file they touch there is as it is in `base`, the untouched clone they were taken against, or already as they leave it; says what became of them, saving them in `patches` as a patch when they are not applied.
pub(crate) fn settle(
    top: &Path,
    base: &Path,
    changes: &[Change],
    finished: bool,
    patches: &Spill,
) -> Merge {
    if changes.is_empty() {
        return Merge::Unchanged;
    }
    let patch = patch(base, changes);
    if !finished {
        return crate::worktree::held("the subagent did not finish", &patch, patches);
    }
    let mut conflicting = Vec::new();
    let mut pending = Vec::new();
    for change in changes {
        let target = top.join(&change.path);
        let now = content(&target).unwrap_or(Content::Missing);
        if now == change.after {
            continue;
        }
        if now != content(&base.join(&change.path)).unwrap_or(Content::Missing) {
            conflicting.push(change.path.clone());
            continue;
        }
        pending.push((target, change));
    }
    if !conflicting.is_empty() {
        return match patches.save("worktree-patch", &patch) {
            Ok(saved) => Merge::Conflicted {
                patch: saved,
                files: conflicting,
            },
            Err(error) => Merge::Held {
                reason: format!(
                    "it does not apply cleanly to {}, and the patch could not be saved: {error}",
                    conflicting.join(", ")
                ),
                patch: None,
            },
        };
    }
    for (target, change) in pending {
        if let Err(error) = write(&target, &change.after) {
            return crate::worktree::held(
                &format!(
                    "{} could not be written ({error}), so the changes after it were not applied",
                    change.path
                ),
                &patch,
                patches,
            );
        }
    }
    Merge::Applied(changes.iter().map(|change| change.path.clone()).collect())
}

/// Makes `target` hold `content`, making the folders it needs, or removes it when `content` is [`Content::Missing`].
///
/// # Errors
///
/// Fails with the error of the first call that fails.
fn write(target: &Path, content: &Content) -> std::io::Result<()> {
    if std::fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink()) {
        std::fs::remove_file(target)?;
    }
    match content {
        Content::Missing => match std::fs::remove_file(target) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
        Content::File { bytes, executable } => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, bytes)?;
            set_executable(target, *executable)
        }
        Content::Link(pointed) => {
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if target.exists() {
                std::fs::remove_file(target)?;
            }
            link(pointed, target)
        }
    }
}

/// Sets or clears the execute bits of `target`, where the reader of the file may read it.
///
/// # Errors
///
/// Fails when its permissions cannot be read or changed.
#[cfg(unix)]
fn set_executable(target: &Path, executable: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt as _;
    let mut permissions = std::fs::metadata(target)?.permissions();
    let mode = permissions.mode();
    let wanted = if executable {
        mode | ((mode & 0o444) >> 2)
    } else {
        mode & !0o111
    };
    if wanted != mode {
        permissions.set_mode(wanted);
        std::fs::set_permissions(target, permissions)?;
    }
    Ok(())
}

/// Leaves the permissions of `_target` alone, on a platform without execute bits.
///
/// # Errors
///
/// Never fails.
#[cfg(not(unix))]
fn set_executable(_target: &Path, _executable: bool) -> std::io::Result<()> {
    Ok(())
}

/// Makes `target` a symbolic link to `pointed`.
///
/// # Errors
///
/// Fails when the link cannot be made.
#[cfg(unix)]
fn link(pointed: &Path, target: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(pointed, target)
}

/// Makes `target` a symbolic link to `pointed`, as a link to a file.
///
/// # Errors
///
/// Fails when the link cannot be made, which needs a privilege on Windows.
#[cfg(windows)]
fn link(pointed: &Path, target: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(pointed, target)
}

/// Fails, on a platform without symbolic links.
///
/// # Errors
///
/// Always fails.
#[cfg(not(any(unix, windows)))]
fn link(_pointed: &Path, _target: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symbolic links cannot be made here",
    ))
}

/// The number of unchanged lines shown around each change of a patch.
const CONTEXT: usize = 3;

/// `changes`, taken against the folder `base`, as a unified diff in git's form, which `git apply` and `patch -p1` read: one hunk per file, around the lines between the first and the last that differ, and a note for a file that is not text.
pub(crate) fn patch(base: &Path, changes: &[Change]) -> Vec<u8> {
    let mut out = String::new();
    for change in changes {
        let before = content(&base.join(&change.path)).unwrap_or(Content::Missing);
        file_patch(&mut out, &change.path, &before, &change.after);
    }
    out.into_bytes()
}

/// The text `content` stands for in a patch, and its git mode; nothing for a missing file, and no text for one that is not text.
fn shown(content: &Content) -> Option<(Option<String>, &'static str)> {
    match content {
        Content::Missing => None,
        Content::Link(pointed) => Some((Some(pointed.to_string_lossy().into_owned()), "120000")),
        Content::File { bytes, executable } => {
            let text = (!bytes.contains(&0))
                .then(|| String::from_utf8(bytes.clone()).ok())
                .flatten();
            Some((text, if *executable { "100755" } else { "100644" }))
        }
    }
}

/// Writes to `out` the part of a patch that turns `before` into `after` at `path`.
fn file_patch(out: &mut String, path: &str, before: &Content, after: &Content) {
    let (old, new) = (shown(before), shown(after));
    let _ = writeln!(out, "diff --git a/{path} b/{path}");
    match (&old, &new) {
        (None, Some((_, mode))) => {
            let _ = writeln!(out, "new file mode {mode}");
        }
        (Some((_, mode)), None) => {
            let _ = writeln!(out, "deleted file mode {mode}");
        }
        (Some((_, was)), Some((_, now))) if was != now => {
            let _ = writeln!(out, "old mode {was}\nnew mode {now}");
        }
        _ => {}
    }
    let old_text = match &old {
        None => Some(String::new()),
        Some((text, _)) => text.clone(),
    };
    let new_text = match &new {
        None => Some(String::new()),
        Some((text, _)) => text.clone(),
    };
    let (Some(old_text), Some(new_text)) = (old_text, new_text) else {
        let _ = writeln!(out, "Binary files a/{path} and b/{path} differ");
        return;
    };
    if old_text == new_text {
        return;
    }
    let from = if old.is_some() {
        format!("a/{path}")
    } else {
        "/dev/null".to_owned()
    };
    let to = if new.is_some() {
        format!("b/{path}")
    } else {
        "/dev/null".to_owned()
    };
    let _ = writeln!(out, "--- {from}\n+++ {to}");
    hunk(out, &old_text, &new_text);
}

/// The lines of `text`, each with its line ending, the last without one when the text does not end with one.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// Writes to `out` one hunk turning `old` into `new`: the lines between the first and the last that differ, removed and added, with up to [`CONTEXT`] unchanged lines on each side.
fn hunk(out: &mut String, old: &str, new: &str) {
    let (old, new) = (lines(old), lines(new));
    let same_start = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let same_end = old[same_start..]
        .iter()
        .rev()
        .zip(new[same_start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = same_start.saturating_sub(CONTEXT);
    let tail = same_end.min(CONTEXT);
    let old_end = old.len() - same_end + tail;
    let new_end = new.len() - same_end + tail;
    let range = |from: usize, to: usize| {
        let count = to - from;
        let first = if count == 0 { from } else { from + 1 };
        format!("{first},{count}")
    };
    let _ = writeln!(
        out,
        "@@ -{} +{} @@",
        range(start, old_end),
        range(start, new_end)
    );
    let mut line = |mark: char, text: &str| {
        out.push(mark);
        out.push_str(text);
        if !text.ends_with('\n') {
            out.push_str("\n\\ No newline at end of file\n");
        }
    };
    for text in &old[start..same_start] {
        line(' ', text);
    }
    for text in &old[same_start..old.len() - same_end] {
        line('-', text);
    }
    for text in &new[same_start..new.len() - same_end] {
        line('+', text);
    }
    for text in &old[old.len() - same_end..old_end] {
        line(' ', text);
    }
}

#[cfg(test)]
/// Tests of taking, applying and showing changes file by file.
mod tests {
    use super::*;

    /// Dates every file under `folder` back to 2001, as a clone keeps the times of what it was made from, so that a file written afterwards differs in time even where the file system's clock is coarse.
    fn age(folder: &Path) {
        let then = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        for entry in std::fs::read_dir(folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                age(&path);
            } else {
                let file = std::fs::File::options().write(true).open(&path).unwrap();
                file.set_modified(then).unwrap();
            }
        }
    }

    /// Changed, added and removed files are found, ignored and untouched ones are not, and they apply to a folder that is still as it was.
    #[test]
    fn changes_are_found_and_applied() {
        let root = crate::testkit::scratch("files-apply");
        let (base, clone, top) = (root.join("base"), root.join("clone"), root.join("top"));
        for folder in [&base, &clone, &top] {
            std::fs::create_dir_all(folder.join("sub")).unwrap();
            std::fs::write(folder.join("keep.txt"), "same\n").unwrap();
            std::fs::write(folder.join("a.txt"), "one\ntwo\n").unwrap();
            std::fs::write(folder.join("sub/gone.txt"), "bye\n").unwrap();
            std::fs::write(folder.join(".gitignore"), "*.log\n").unwrap();
            age(folder);
        }
        std::fs::write(clone.join("a.txt"), "one\nTWO\n").unwrap();
        std::fs::write(clone.join("new.txt"), "made").unwrap();
        std::fs::write(clone.join("noise.log"), "ignored\n").unwrap();
        std::fs::remove_file(clone.join("sub/gone.txt")).unwrap();
        let found = changes(&base, &clone).unwrap();
        let paths: Vec<&str> = found.iter().map(|change| change.path.as_str()).collect();
        assert_eq!(paths, ["a.txt", "new.txt", "sub/gone.txt"]);
        let shown = String::from_utf8(patch(&base, &found)).unwrap();
        assert!(shown.contains("-two\n+TWO\n"), "{shown}");
        assert!(shown.contains("--- /dev/null\n+++ b/new.txt\n@@ -0,0 +1,1 @@\n+made\n\\ No newline at end of file\n"), "{shown}");
        assert!(shown.contains("deleted file mode 100644"), "{shown}");
        let merge = settle(&top, &base, &found, true, &Spill::new(root.join("out")));
        assert_eq!(
            merge,
            Merge::Applied(vec![
                "a.txt".into(),
                "new.txt".into(),
                "sub/gone.txt".into()
            ])
        );
        assert_eq!(
            std::fs::read_to_string(top.join("a.txt")).unwrap(),
            "one\nTWO\n"
        );
        assert_eq!(
            std::fs::read_to_string(top.join("new.txt")).unwrap(),
            "made"
        );
        assert!(!top.join("sub/gone.txt").exists() && !top.join("noise.log").exists());
    }

    /// A file changed in the folder since the subagent started stops every change from being applied, and the patch is saved naming it.
    #[test]
    fn a_conflict_applies_nothing() {
        let root = crate::testkit::scratch("files-conflict");
        let (base, clone, top) = (root.join("base"), root.join("clone"), root.join("top"));
        for folder in [&base, &clone, &top] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("a.txt"), "one\n").unwrap();
            std::fs::write(folder.join("b.txt"), "bee\n").unwrap();
            age(folder);
        }
        std::fs::write(clone.join("a.txt"), "sub\n").unwrap();
        std::fs::write(clone.join("b.txt"), "sub bee\n").unwrap();
        std::fs::write(top.join("a.txt"), "main\n").unwrap();
        let found = changes(&base, &clone).unwrap();
        let merge = settle(&top, &base, &found, true, &Spill::new(root.join("out")));
        let Merge::Conflicted { patch, files } = merge else {
            panic!("no conflict: {merge:?}");
        };
        assert_eq!(files, ["a.txt"]);
        assert!(std::fs::read_to_string(patch).unwrap().contains("+sub bee"));
        assert_eq!(std::fs::read_to_string(top.join("b.txt")).unwrap(), "bee\n");
    }
}
