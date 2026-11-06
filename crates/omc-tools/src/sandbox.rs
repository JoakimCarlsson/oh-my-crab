//! Confinement of tool paths to the working directory.

use std::path::{Component, Path, PathBuf};

use crate::{Spill, ToolError};

/// The directory tools may touch, with the logic to keep them inside it, and the folder long output is saved in, which tools may read as well.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// The canonical working directory.
    root: PathBuf,
    /// Where output too long to show is saved.
    spill: Spill,
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
            spill: Spill::default(),
        })
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

    /// Resolves `path` against the root and checks that it stays inside.
    ///
    /// Relative paths are joined to the root, `.` and `..` are folded away, and
    /// symlinks are followed for the part of the path that exists, so a link
    /// pointing outside is rejected. The path itself need not exist.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::OutsidePath`] when the resolved path is not inside
    /// the root.
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
        if resolved.starts_with(&self.root) {
            Ok(resolved)
        } else {
            Err(outside())
        }
    }

    /// Describes where `path` really points, for the approval prompt.
    ///
    /// Shows the resolved path relative to the root, or flags a path that
    /// would be rejected.
    pub fn describe(&self, path: &str) -> String {
        match self.resolve(path) {
            Ok(resolved) => self.display(&resolved),
            Err(_) => format!("{path} (OUTSIDE the working directory; will be rejected)"),
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
        let sandbox = Sandbox::new(&dir).unwrap();
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
        let sandbox = Sandbox::new(&dir).unwrap();
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
        let sandbox = Sandbox::new(&dir).unwrap().with_spill(Spill::new(&saved));
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
        let sandbox = Sandbox::new(&dir).unwrap();
        assert!(matches!(
            sandbox.resolve("link"),
            Err(ToolError::OutsidePath(_))
        ));
    }
}
