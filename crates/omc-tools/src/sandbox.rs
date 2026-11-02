//! Confinement of tool paths to the working directory.

use std::path::{Component, Path, PathBuf};

use crate::ToolError;

/// The directory tools may touch, with the logic to keep them inside it.
#[derive(Debug, Clone)]
pub struct Sandbox {
    /// The canonical working directory.
    root: PathBuf,
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
        })
    }

    /// The canonical working directory.
    pub fn root(&self) -> &Path {
        &self.root
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
        let resolved = canonicalize_existing(&folded);
        if resolved.starts_with(&self.root) {
            Ok(resolved)
        } else {
            Err(outside())
        }
    }

    /// Returns `path` relative to the root for display, or unchanged when it is
    /// not under the root.
    pub fn display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }
}

/// Canonicalizes the longest existing prefix of `path` and re-appends the rest.
fn canonicalize_existing(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail = Vec::new();
    loop {
        if let Ok(real) = existing.canonicalize() {
            return tail.iter().rev().fold(real, |acc, name| acc.join(name));
        }
        match (existing.file_name(), existing.parent()) {
            (Some(name), Some(parent)) => {
                tail.push(name.to_os_string());
                existing = parent.to_path_buf();
            }
            _ => return path.to_path_buf(),
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
}
