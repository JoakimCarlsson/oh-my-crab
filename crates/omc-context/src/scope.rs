//! The folders customizations are read from.

use std::path::{Path, PathBuf};

/// The folders a conversation's instructions, skills and agents are read from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Locations {
    /// The folder the agent works in.
    pub directory: PathBuf,
    /// The root of the repository the folder is in, when it is in one.
    pub root: Option<PathBuf>,
    /// The reader's home folder, when known.
    pub home: Option<PathBuf>,
    /// Claude Code's own config folder, when known.
    pub claude: Option<PathBuf>,
    /// Codex's own config folder, when known.
    pub codex: Option<PathBuf>,
    /// Whether the project's own folders are read: off while the project is not trusted, which leaves out its instruction files, skills and agent definitions, and the plugins its settings switch on or off.
    pub project: bool,
}

impl Locations {
    /// The locations for work in `directory`, with the user folders taken from the environment and the project's own folders read.
    ///
    /// `CLAUDE_CONFIG_DIR` and `CODEX_HOME` move the products' own folders; otherwise they are `~/.claude` and `~/.codex`.
    pub fn detect(directory: &Path) -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let own = |variable: &str, folder: &str| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .or_else(|| home.as_ref().map(|home| home.join(folder)))
        };
        Self {
            directory: directory.to_path_buf(),
            root: git_root(directory),
            claude: own("CLAUDE_CONFIG_DIR", ".claude"),
            codex: own("CODEX_HOME", ".codex"),
            home,
            project: true,
        }
    }

    /// The folders from the working folder up to the repository root, nearest first; just the working folder when it is in no repository.
    pub fn ancestors(&self) -> Vec<PathBuf> {
        let mut folders = Vec::new();
        for folder in self.directory.ancestors() {
            folders.push(folder.to_path_buf());
            if Some(folder) == self.root.as_deref() {
                break;
            }
        }
        if self.root.is_none() {
            folders.truncate(1);
        }
        folders
    }
}

/// The nearest folder at or above `directory` that holds a `.git` entry.
pub(crate) fn git_root(directory: &Path) -> Option<PathBuf> {
    directory
        .ancestors()
        .find(|folder| folder.join(".git").exists())
        .map(Path::to_path_buf)
}

#[cfg(test)]
/// Tests for the locations.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// The ancestors stop at the repository root.
    #[test]
    fn ancestors_stop_at_the_root() {
        let base = scratch("scope-ancestors");
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        std::fs::create_dir_all(base.join("repo/a/b")).unwrap();
        let locations = Locations::detect(&base.join("repo/a/b"));
        assert_eq!(locations.root, Some(base.join("repo")));
        assert_eq!(
            locations.ancestors(),
            vec![
                base.join("repo/a/b"),
                base.join("repo/a"),
                base.join("repo")
            ]
        );
    }

    /// Outside a repository only the working folder counts.
    #[test]
    fn no_repository_means_one_folder() {
        let base = scratch("scope-norepo");
        let locations = Locations {
            directory: base.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            project: true,
        };
        assert_eq!(locations.ancestors(), vec![base]);
    }
}
