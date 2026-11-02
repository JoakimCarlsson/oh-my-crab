//! Atomic file replacement shared by the mutating tools.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::ToolError;

/// Distinguishes temporary files created by concurrent writes in one process.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Writes `content` to `path` by writing a sibling temporary file and renaming
/// it over the target, so readers never see a partial file.
///
/// An existing file keeps its permissions. The temporary file is removed when
/// any step fails.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the directory is missing or not writable, or
/// when writing or renaming fails.
pub fn write_atomic(path: &Path, content: &[u8]) -> Result<(), ToolError> {
    let dir = path
        .parent()
        .ok_or_else(|| ToolError::Failed(format!("{} has no parent directory", path.display())))?;
    let name = path
        .file_name()
        .ok_or_else(|| ToolError::Failed(format!("{} has no file name", path.display())))?
        .to_string_lossy();
    let id = COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{name}.omc-{}-{id}.tmp", std::process::id()));
    let result = write_and_rename(&temp, path, content);
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// Writes `content` to `temp`, copies the permissions of `target` and renames
/// `temp` over `target`.
///
/// # Errors
///
/// Returns [`ToolError::Io`] on any filesystem failure.
fn write_and_rename(temp: &Path, target: &Path, content: &[u8]) -> Result<(), ToolError> {
    let mut file = std::fs::File::options()
        .write(true)
        .create_new(true)
        .open(temp)?;
    file.write_all(content)?;
    file.sync_all()?;
    if let Ok(meta) = std::fs::metadata(target) {
        file.set_permissions(meta.permissions())?;
    }
    drop(file);
    std::fs::rename(temp, target)?;
    Ok(())
}
