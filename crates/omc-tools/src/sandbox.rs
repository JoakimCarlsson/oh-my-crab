//! Confinement of tool paths to the working directory and the user's scratch directory.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use crate::{Spill, ToolError};

/// The directories tools may touch, the working directory and the user's scratch directory, with the logic to keep them inside those, and the folder long output is saved in, which tools may read as well.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// The canonical working directory.
    root: PathBuf,
    /// The scratch directory tools may read and write besides the working directory, with symlinks followed as far as it exists; none when the user has no cache directory.
    scratch: Option<PathBuf>,
    /// Where output too long to show is saved.
    spill: Spill,
    /// The whole-file tags the model was shown for each file, oldest first, shared by every copy of the sandbox, so that an edit that leaves the tag out of a file the model has seen as it now is, or cites a tag that was never this file's, can use the latest.
    shown: Arc<Mutex<HashMap<PathBuf, Vec<String>>>>,
}

/// The user's scratch directory: `scratch` under the user's cache directory, which is `$XDG_CACHE_HOME` or `~/.cache` on Linux; none when the user has no cache directory.
fn scratch_dir() -> Option<PathBuf> {
    dirs::cache_dir().map(|cache| cache.join("scratch"))
}

impl Sandbox {
    /// Creates a sandbox rooted at `root`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Io`] when `root` cannot be canonicalized.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, ToolError> {
        Ok(Self {
            root: root.as_ref().canonicalize()?,
            scratch: scratch_dir().and_then(|dir| canonicalize_existing(&dir)),
            spill: Spill::default(),
            shown: Arc::default(),
        })
    }

    /// Notes that the model was shown `tag`, the whole-file tag of `path` as it was then.
    pub(crate) fn note_shown(&self, path: &Path, tag: &str) {
        let mut shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        let tags = shown.entry(path.to_path_buf()).or_default();
        tags.retain(|seen| seen != tag);
        tags.push(tag.to_owned());
    }

    /// The tag an edit of `path` that cites `cited`, possibly nothing, is taken to mean, given `current`, the whole-file tag of the file now: the latest tag the model was shown for the file when that is the file as it now is, and `cited` names no earlier state of the file the model saw, as with a tag left out or copied from another file; otherwise `cited` as it is.
    pub(crate) fn meant_tag<'a>(&self, path: &Path, cited: &'a str, current: &'a str) -> &'a str {
        let shown = self.shown.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(tags) = shown.get(path) else {
            return cited;
        };
        let latest_is_current = tags.last().is_some_and(|latest| latest == current);
        let names_earlier = tags.iter().any(|seen| seen.eq_ignore_ascii_case(cited));
        if latest_is_current && !names_earlier {
            current
        } else {
            cited
        }
    }

    /// The sandbox letting tools reach `scratch` besides the working directory, instead of the user's scratch directory; none lets them reach only the working directory.
    pub fn with_scratch(mut self, scratch: Option<&Path>) -> Self {
        self.scratch = scratch.and_then(canonicalize_existing);
        self
    }

    /// The scratch directory tools may reach besides the working directory, when there is one.
    pub fn scratch(&self) -> Option<&Path> {
        self.scratch.as_deref()
    }

    /// The sandbox saving output too long to show in `spill` instead of the process's own folder.
    pub fn with_spill(mut self, spill: Spill) -> Self {
        self.spill = spill;
        self
    }

    /// The canonical working directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Where output too long to show is saved.
    pub fn spill(&self) -> &Spill {
        &self.spill
    }

    /// Resolves `path` for reading: like [`Sandbox::resolve`], and also letting through a file the sandbox saved long output in, so the model can read the rest of it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] when the resolved path is neither
    /// inside the root nor a saved output file.
    pub fn resolve_readable(&self, path: &str) -> Result<PathBuf, ToolError> {
        match self.resolve(path) {
            Err(ToolError::OutsidePath(_))
                if let Some(saved) = self.spill.holds(Path::new(path)) =>
            {
                Ok(saved)
            }
            resolved => resolved,
        }
    }

    /// Resolves `path` against the root and checks that it stays inside the root or the scratch directory.
    ///
    /// Relative paths are joined to the root, `.` and `..` are folded away, and
    /// symlinks are followed for the part of the path that exists, so a link
    /// pointing outside both is rejected. The path itself need not exist.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] when the resolved path is inside
    /// neither the root nor the scratch directory.
    pub fn resolve(&self, path: &str) -> Result<PathBuf, ToolError> {
        let outside = || ToolError::OutsidePath(path.to_string());
        let joined = self.root.join(path);
        let mut folded = PathBuf::new();
        for component in joined.components() {
            match component {
                Component::ParentDir => {
                    if !folded.pop() {
                        return Err(outside());
                    }
                }
                Component::CurDir => {}
                other => folded.push(other),
            }
        }
        let resolved = canonicalize_existing(&folded).ok_or_else(outside)?;
        if resolved.starts_with(&self.root)
            || self
                .scratch
                .as_ref()
                .is_some_and(|scratch| resolved.starts_with(scratch))
        {
            Ok(resolved)
        } else {
            Err(outside())
        }
    }

    /// Where `path` points, for permission rules to match: as [`Sandbox::resolve`] resolves it, or, when it leaves the root, joined to the root with `.` and `..` folded away and symlinks followed as far as they exist, so that a rule about a folder outside still sees it.
    pub fn locate(&self, path: &str) -> PathBuf {
        if let Ok(resolved) = self.resolve(path) {
            return resolved;
        }
        let mut folded = PathBuf::new();
        for component in self.root.join(path).components() {
            match component {
                Component::ParentDir => {
                    folded.pop();
                }
                Component::CurDir => {}
                other => folded.push(other),
            }
        }
        canonicalize_existing(&folded).unwrap_or(folded)
    }

    /// Describes where `path` really points, for the approval prompt.
    ///
    /// Shows the resolved path relative to the root, or flags a path that
    /// would be rejected.
    pub fn describe(&self, path: &str) -> String {
        match self.resolve(path) {
            Ok(resolved) => self.display(&resolved),
            Err(_) => format!(
                "{path} (OUTSIDE the working directory and the scratch directory; will be rejected)"
            ),
        }
    }

    /// Returns `path` relative to the root for display, or unchanged when it is
    /// not under the root, with `/` between the parts on every system.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "/")
    }
}

/// Canonicalizes the longest existing prefix of `path` and re-appends the rest.
///
/// Returns `None` when a component exists on disk but cannot be canonicalized,
/// which is what a dangling symlink looks like: its target is unknown, so it
/// could point anywhere and a write through it would escape.
fn canonicalize_existing(path: &Path) -> Option<PathBuf> {
    let mut existing = path.to_path_buf();
    let mut tail = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return Some(tail.iter().rev().fold(real, |acc, name| acc.join(name)));
        }
        if existing.symlink_metadata().is_ok() {
            return None;
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return Some(path.to_path_buf()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Paths inside the root resolve, whether or not they exist yet.
    #[test]
    fn resolves_paths_inside_the_root() {
        let dir = scratch("sandbox-inside");
        let sandbox = Sandbox::new(&dir).unwrap();
        assert!(
            sandbox
                .resolve("a/b.txt")
                .unwrap()
                .starts_with(sandbox.root())
        );
        assert!(sandbox.resolve("a/../c.txt").is_ok());
    }

    /// Parent traversal and absolute paths outside the root are rejected.
    #[test]
    fn rejects_paths_outside_the_root() {
        let dir = scratch("sandbox-outside");
        let sandbox = Sandbox::new(&dir).unwrap().with_scratch(None);
        assert!(matches!(
            sandbox.resolve("../escape.txt"),
            Err(ToolError::OutsidePath(_))
        ));
        assert!(matches!(
            sandbox.resolve("/etc/passwd"),
            Err(ToolError::OutsidePath(_))
        ));
    }

    /// A symlink pointing out of the root is rejected.
    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_root() {
        let dir = scratch("sandbox-symlink");
        let outside = scratch("sandbox-symlink-target");
        std::os::unix::fs::symlink(&outside, dir.join("link")).unwrap();
        let sandbox = Sandbox::new(&dir).unwrap().with_scratch(None);
        assert!(matches!(
            sandbox.resolve("link/file.txt"),
            Err(ToolError::OutsidePath(_))
        ));
    }

    /// Saved output outside the root may be read but not written, and nothing else outside the root may be read.
    #[test]
    fn reads_saved_output_outside_the_root() {
        let dir = scratch("sandbox-spill");
        let saved = scratch("sandbox-spill-saved");
        let sandbox = Sandbox::new(&dir)
            .unwrap()
            .with_scratch(None)
            .with_spill(Spill::new(&saved));
        let path = sandbox.spill().save("bash", b"out").unwrap();
        let text = path.display().to_string();
        assert_eq!(sandbox.resolve_readable(&text).unwrap(), path);
        assert!(sandbox.resolve(&text).is_err());
        let other = saved.parent().unwrap().join("sandbox-spill-other.txt");
        std::fs::write(&other, "x").unwrap();
        assert!(matches!(
            sandbox.resolve_readable(&other.display().to_string()),
            Err(ToolError::OutsidePath(_))
        ));
    }

    /// A dangling symlink is rejected even though its target does not exist.
    #[cfg(unix)]
    #[test]
    fn rejects_dangling_symlinks() {
        let dir = scratch("sandbox-dangling");
        let outside = scratch("sandbox-dangling-target");
        std::os::unix::fs::symlink(outside.join("missing.txt"), dir.join("link")).unwrap();
        let sandbox = Sandbox::new(&dir).unwrap().with_scratch(None);
        assert!(matches!(
            sandbox.resolve("link"),
            Err(ToolError::OutsidePath(_))
        ));
    }

    /// Paths in the scratch directory resolve for reading and writing, whether or not they exist yet, while its siblings and traversal out of it are rejected.
    #[test]
    fn reaches_the_scratch_directory() {
        let dir = scratch("sandbox-scratch-root");
        let cache = scratch("sandbox-scratch-cache");
        let pad = cache.join("scratch");
        std::fs::create_dir_all(&pad).unwrap();
        let sandbox = Sandbox::new(&dir).unwrap().with_scratch(Some(&pad));
        let inside = pad.join("project/notes.txt").display().to_string();
        assert!(
            sandbox
                .resolve(&inside)
                .unwrap()
                .starts_with(sandbox.scratch().unwrap())
        );
        assert!(sandbox.resolve_readable(&inside).is_ok());
        let sibling = cache.join("other.txt").display().to_string();
        assert!(matches!(
            sandbox.resolve(&sibling),
            Err(ToolError::OutsidePath(_))
        ));
        let traversal = pad.join("project/../../other.txt").display().to_string();
        assert!(matches!(
            sandbox.resolve(&traversal),
            Err(ToolError::OutsidePath(_))
        ));
        assert!(matches!(
            sandbox.resolve("../escape.txt"),
            Err(ToolError::OutsidePath(_))
        ));
    }

    /// A symlink in the scratch directory pointing out of both roots is rejected.
    #[cfg(unix)]
    #[test]
    fn rejects_symlinks_that_leave_the_scratch_directory() {
        let dir = scratch("sandbox-scratch-link-root");
        let pad = scratch("sandbox-scratch-link-pad");
        let outside = scratch("sandbox-scratch-link-target");
        std::os::unix::fs::symlink(&outside, pad.join("link")).unwrap();
        let sandbox = Sandbox::new(&dir).unwrap().with_scratch(Some(&pad));
        let through = pad.join("link/file.txt").display().to_string();
        assert!(matches!(
            sandbox.resolve(&through),
            Err(ToolError::OutsidePath(_))
        ));
    }
}
