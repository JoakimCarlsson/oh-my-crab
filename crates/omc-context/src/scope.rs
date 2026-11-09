//! The folders customizations are read from.

use std::path::{Path, PathBuf};

use crate::Origin;

/// The environment variable that moves crab's own folder for instructions, skills, agent definitions and system prompt files; an empty value counts as unset.
pub const CONFIG_DIR_VARIABLE: &str = "OMC_CONFIG_DIR";

/// The folder under a project folder that holds crab's own customizations for that project.
pub const PROJECT_FOLDER: &str = ".oh-my-crab";

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
    /// Crab's own config folder, read for the user's instructions, skills, agent definitions and system prompt files, when known.
    pub crab: Option<PathBuf>,
    /// Whether the project's own folders are read: off while the project is not trusted, which leaves out its instruction files, skills and agent definitions, and the plugins its settings switch on or off.
    pub project: bool,
}

impl Locations {
    /// The locations for work in `directory`, with the user folders taken from the environment and the project's own folders read.
    ///
    /// `CLAUDE_CONFIG_DIR` and `CODEX_HOME` move the products' own folders; otherwise they are `~/.claude` and `~/.codex`. Crab's own folder is the one [`crab_folder`] gives.
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
            crab: crab_folder(),
            home,
            project: true,
        }
    }

    /// The folder crab's own customizations of `scope` are written to: [`Locations::crab`] for the user's, `.oh-my-crab` in the working folder for the project's, whether the project is read or not; nothing for a plugin's or a built-in one, or when crab's own folder is not known.
    ///
    /// Instruction files go in it as `AGENTS.md` or `rules/<name>.md`, skills as `skills/<name>/SKILL.md`, agent definitions as `agents/<name>.md`, and system prompt files as `SYSTEM.md` and `APPEND_SYSTEM.md`.
    pub fn native_folder(&self, scope: Origin) -> Option<PathBuf> {
        match scope {
            Origin::User => self.crab.clone(),
            Origin::Project => Some(self.directory.join(PROJECT_FOLDER)),
            Origin::Plugin | Origin::BuiltIn => None,
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

/// Crab's own config folder: the one [`CONFIG_DIR_VARIABLE`] names, else `oh-my-crab` in the user's config folder; nothing when neither is known.
pub fn crab_folder() -> Option<PathBuf> {
    chosen_folder(std::env::var_os(CONFIG_DIR_VARIABLE), dirs::config_dir())
}

/// Crab's own config folder when [`CONFIG_DIR_VARIABLE`] holds `named` and the user's config folder is `config`: `named` unless it is missing or empty, else `oh-my-crab` in `config`.
fn chosen_folder(named: Option<std::ffi::OsString>, config: Option<PathBuf>) -> Option<PathBuf> {
    match named {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => config.map(|base| base.join("oh-my-crab")),
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
            crab: None,
            project: true,
        };
        assert_eq!(locations.ancestors(), vec![base]);
    }

    /// The variable names crab's own folder unless it is empty; otherwise it is `oh-my-crab` in the user's config folder.
    #[test]
    fn crab_folder_follows_the_variable() {
        let config = Some(PathBuf::from("/c"));
        assert_eq!(
            chosen_folder(Some("/mine".into()), config.clone()),
            Some(PathBuf::from("/mine"))
        );
        assert_eq!(
            chosen_folder(Some("".into()), config.clone()),
            Some(PathBuf::from("/c/oh-my-crab"))
        );
        assert_eq!(
            chosen_folder(None, config),
            Some(PathBuf::from("/c/oh-my-crab"))
        );
        assert_eq!(chosen_folder(None, None), None);
    }

    /// The user's native folder is crab's own and the project's is `.oh-my-crab` in the working folder, even while the project is not read; plugins and built-ins have none.
    #[test]
    fn native_folders_by_scope() {
        let base = scratch("scope-native");
        let locations = Locations {
            directory: base.join("repo"),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: Some(base.join("config/oh-my-crab")),
            project: false,
        };
        assert_eq!(
            locations.native_folder(Origin::User),
            Some(base.join("config/oh-my-crab"))
        );
        assert_eq!(
            locations.native_folder(Origin::Project),
            Some(base.join("repo/.oh-my-crab"))
        );
        assert_eq!(locations.native_folder(Origin::Plugin), None);
        assert_eq!(locations.native_folder(Origin::BuiltIn), None);
    }
}
