//! Which project a folder belongs to, so that every conversation about the same code shares one memory.

use std::path::{Path, PathBuf};

/// The project a folder belongs to.
///
/// In a git repository the project is the repository itself, told apart by the folder git keeps its shared data in, so that the main checkout and every linked worktree of it, such as the ones an editor makes for each agent session, are one project. A submodule is a project of its own. Outside a repository the project is the folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Project {
    /// The folder the project is shown as: the main checkout of a repository, the worktree a bare repository was reached through, or the folder outside a repository.
    pub root: PathBuf,
    /// What tells the project apart: git's common folder of a repository, or the folder outside one.
    pub key: PathBuf,
}

impl Project {
    /// The project `directory` belongs to; a folder that cannot be resolved stands for itself.
    pub fn of(directory: &Path) -> Self {
        Self::below(directory, None)
    }

    /// The project `directory` belongs to, looking for a repository no higher up than `ceiling` when it is given.
    fn below(directory: &Path, ceiling: Option<&Path>) -> Self {
        let directory = directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf());
        for folder in directory.ancestors() {
            if ceiling.is_some_and(|ceiling| !folder.starts_with(ceiling)) {
                break;
            }
            let dot_git = folder.join(".git");
            if dot_git.is_dir() {
                return Self {
                    root: folder.to_path_buf(),
                    key: dot_git.canonicalize().unwrap_or(dot_git),
                };
            }
            if dot_git.is_file()
                && let Some(common) = common_folder(folder, &dot_git)
            {
                let root = match common.file_name() {
                    Some(name) if name == ".git" => common
                        .parent()
                        .map_or_else(|| folder.to_path_buf(), Path::to_path_buf),
                    _ => folder.to_path_buf(),
                };
                return Self { root, key: common };
            }
        }
        Self {
            root: directory.clone(),
            key: directory,
        }
    }

    /// The short name of the project: the name of its folder, or of a bare repository without its `.git` ending.
    pub fn name(&self) -> String {
        let named = if self.key.file_name().is_some_and(|name| name == ".git") {
            &self.root
        } else {
            &self.key
        };
        let name = named
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        match name.strip_suffix(".git") {
            Some(stem) if !stem.is_empty() => stem.to_owned(),
            _ if name.is_empty() => "root".to_owned(),
            _ => name,
        }
    }

    /// The name of the folder the project's memories are kept in: its name, cut to letters, digits, `-`, `_` and `.`, then a hash of its key, so that two projects of the same name never share one.
    pub fn folder_name(&self) -> String {
        let name: String = self
            .name()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .take(40)
            .collect();
        let name = name.trim_start_matches('.');
        format!(
            "{}-{:016x}",
            if name.is_empty() { "project" } else { name },
            fnv(self.key.to_string_lossy().as_bytes())
        )
    }
}

/// The folder git keeps the shared data of the repository in, for the worktree at `folder` whose `.git` file is `dot_git`: what its `commondir` names for a linked worktree, or the folder the file points at for a submodule; nothing when the file cannot be read.
fn common_folder(folder: &Path, dot_git: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(dot_git).ok()?;
    let target = pointer.trim().strip_prefix("gitdir:")?.trim();
    let git_dir = folder.join(target);
    let git_dir = git_dir.canonicalize().unwrap_or(git_dir);
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(common) => git_dir.join(common.trim()),
        Err(_) => git_dir,
    };
    Some(common.canonicalize().unwrap_or(common))
}

/// The 64-bit FNV-1a hash of `bytes`, which stays the same from one build to the next.
pub(crate) fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
/// Tests for telling projects apart.
mod tests {
    use super::*;
    use crate::testutil::{git, scratch};

    /// Makes a repository with one commit at `folder`.
    fn repository(folder: &Path) {
        std::fs::create_dir_all(folder).unwrap();
        git(folder, &["init", "-q", "-b", "main"]);
        git(
            folder,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                "first",
            ],
        );
    }

    /// The main checkout, a subfolder of it and two linked worktrees are one project shown as the main checkout; another repository is another project, even with the same name.
    #[test]
    fn linked_worktrees_share_a_project() {
        let base = scratch("project-worktrees");
        let main = base.join("one/app");
        repository(&main);
        std::fs::create_dir_all(main.join("src")).unwrap();
        git(&main, &["worktree", "add", "-q", "../wt-a", "-b", "a"]);
        git(&main, &["worktree", "add", "-q", "../wt-b", "-b", "b"]);
        let project = Project::of(&main);
        assert_eq!(project.root, main);
        assert_eq!(project.name(), "app");
        assert_eq!(Project::of(&main.join("src")), project);
        assert_eq!(Project::of(&base.join("one/wt-a")), project);
        assert_eq!(Project::of(&base.join("one/wt-b")), project);
        let other = base.join("two/app");
        repository(&other);
        let elsewhere = Project::of(&other);
        assert_ne!(elsewhere, project);
        assert_ne!(elsewhere.folder_name(), project.folder_name());
        assert!(project.folder_name().starts_with("app-"));
    }

    /// A folder outside any repository is a project of its own, named after itself.
    #[test]
    fn a_plain_folder_is_its_own_project() {
        let base = scratch("project-plain");
        let folder = base.join("notes here");
        std::fs::create_dir_all(&folder).unwrap();
        let project = Project::below(&folder, Some(&base));
        assert_eq!(
            (project.root.clone(), project.key.clone()),
            (folder.clone(), folder)
        );
        assert!(project.folder_name().starts_with("notes_here-"));
    }

    /// The hash is the published FNV-1a.
    #[test]
    fn fnv_is_stable() {
        assert_eq!(fnv(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv(b"a"), 0xaf63_dc4c_8601_ec8c);
    }
}
