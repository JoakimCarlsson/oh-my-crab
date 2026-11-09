//! The instruction files a project and its reader keep for the agent.

use std::path::{Path, PathBuf};

use crate::place::{INSTRUCTIONS, Place, Root, Shape, files_under};
use crate::scope::PROJECT_FOLDER;
use crate::{Locations, Origin};

/// The name of the file whose text replaces crab's built-in instructions at the start of the main agent's system prompt.
pub const SYSTEM_FILE: &str = "SYSTEM.md";

/// The name of the file whose text is added to the end of the system prompt of the main agent and of every subagent.
pub const APPEND_SYSTEM_FILE: &str = "APPEND_SYSTEM.md";

/// The system prompt files of a scope, or those in force for a conversation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemFiles {
    /// The [`SYSTEM_FILE`].
    pub replace: Option<PathBuf>,
    /// The [`APPEND_SYSTEM_FILE`].
    pub append: Option<PathBuf>,
}

/// The most of one instruction file that goes into the prompt, in bytes.
const LIMIT: usize = 64 * 1024;

/// One instruction file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Instruction {
    /// Where the file is.
    pub path: PathBuf,
    /// What it says.
    pub content: String,
    /// Whether it is the reader's own, rather than the project's.
    pub user: bool,
}

/// The instruction files the agent reads for work in `locations`, the project's first and nearest first, then the reader's; the paths in `disabled` are skipped.
pub(crate) fn find(locations: &Locations, disabled: &[String]) -> Vec<Instruction> {
    let mut found: Vec<Instruction> = Vec::new();
    for user in [false, true] {
        for folder in folders(locations, user) {
            for place in INSTRUCTIONS
                .iter()
                .filter(|place| (place.root != Root::Project) == user)
                .filter(|place| place.root == folder.0)
            {
                for path in paths(&folder.1, place) {
                    admit(&mut found, path, user, disabled);
                }
            }
        }
    }
    found
}

/// The folders to look in for the project's places, or for the reader's, with the root each stands for.
fn folders(locations: &Locations, user: bool) -> Vec<(Root, PathBuf)> {
    let roots: &[Root] = if user {
        &[Root::Crab, Root::Claude, Root::Codex]
    } else {
        &[Root::Project]
    };
    roots
        .iter()
        .flat_map(|root| {
            locations
                .folders(*root)
                .into_iter()
                .map(|folder| (*root, folder))
        })
        .collect()
}

/// The files `place` names under `folder`.
fn paths(folder: &Path, place: &Place) -> Vec<PathBuf> {
    let path = folder.join(place.path);
    match place.shape {
        Shape::File => path.is_file().then_some(path).into_iter().collect(),
        Shape::Files(suffix) => files_under(&path, suffix),
        Shape::Skills | Shape::Toml => Vec::new(),
    }
}

/// Reads `path` into `found`, unless it is disabled, empty, unreadable or already there.
fn admit(found: &mut Vec<Instruction>, path: PathBuf, user: bool, disabled: &[String]) {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    if found.iter().any(|held| {
        held.path
            .canonicalize()
            .unwrap_or_else(|_| held.path.clone())
            == canonical
    }) {
        return;
    }
    if let Some(instruction) = read(path, user, disabled) {
        found.push(instruction);
    }
}

/// The file at `path`, cut to [`LIMIT`] bytes, marked as the reader's own when `user` is set; nothing when `disabled` names it or it is unreadable or blank.
fn read(path: PathBuf, user: bool, disabled: &[String]) -> Option<Instruction> {
    let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
    let shown = path.display().to_string();
    if disabled
        .iter()
        .any(|entry| *entry == shown || *entry == canonical.display().to_string())
    {
        return None;
    }
    let mut content = std::fs::read_to_string(&path).ok()?;
    if content.trim().is_empty() {
        return None;
    }
    if content.len() > LIMIT {
        let mut end = LIMIT;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
    }
    Some(Instruction {
        path,
        content,
        user,
    })
}

/// The system prompt files in force for work in `locations`: for each of [`SYSTEM_FILE`] and [`APPEND_SYSTEM_FILE`], the first that is readable and not blank in the project's `.oh-my-crab` folders, nearest first and only while the project's own folders are read, then in crab's own config folder.
pub fn system_files(locations: &Locations) -> SystemFiles {
    let (replace, append) = system_texts(locations, &[]);
    SystemFiles {
        replace: replace.map(|file| file.path),
        append: append.map(|file| file.path),
    }
}

/// Where the system prompt files of `scope` are kept, and where a new one is written: in the folder [`Locations::native_folder`] gives, whether or not the files are there; nothing when that folder is not known.
pub fn native_system_files(locations: &Locations, scope: Origin) -> SystemFiles {
    let folder = locations.native_folder(scope);
    SystemFiles {
        replace: folder.as_ref().map(|folder| folder.join(SYSTEM_FILE)),
        append: folder.map(|folder| folder.join(APPEND_SYSTEM_FILE)),
    }
}

/// The [`SYSTEM_FILE`] and the [`APPEND_SYSTEM_FILE`] in force for work in `locations`, as [`system_files`] finds them, read; files `disabled` names are passed over.
pub(crate) fn system_texts(
    locations: &Locations,
    disabled: &[String],
) -> (Option<Instruction>, Option<Instruction>) {
    let folders: Vec<(PathBuf, bool)> = locations
        .folders(Root::Project)
        .into_iter()
        .map(|folder| (folder.join(PROJECT_FOLDER), false))
        .chain(
            locations
                .folders(Root::Crab)
                .into_iter()
                .map(|folder| (folder, true)),
        )
        .collect();
    let first = |name: &str| {
        folders
            .iter()
            .find_map(|(folder, user)| read(folder.join(name), *user, disabled))
    };
    (first(SYSTEM_FILE), first(APPEND_SYSTEM_FILE))
}

#[cfg(test)]
/// Tests for instruction discovery.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Builds locations for `base`, with the working folder `base/repo/pkg`.
    fn locations(base: &Path) -> Locations {
        Locations {
            directory: base.join("repo/pkg"),
            root: Some(base.join("repo")),
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: Some(base.join("home/.codex")),
            crab: Some(base.join("config/oh-my-crab")),
            project: true,
        }
    }

    /// Project files come nearest first, then the reader's, and every table location is read.
    #[test]
    fn walks_up_and_reads_user_files() {
        let base = scratch("instructions-walk");
        put(&base, "repo/AGENTS.md", "root agents");
        put(&base, "repo/CLAUDE.md", "root claude");
        put(&base, "repo/.claude/rules/style.md", "rule");
        put(&base, "repo/pkg/CLAUDE.md", "nested claude");
        put(&base, "repo/pkg/CLAUDE.local.md", "local");
        put(&base, "repo/pkg/AGENTS.override.md", "override");
        put(&base, "home/.claude/CLAUDE.md", "user claude");
        put(&base, "home/.codex/AGENTS.md", "user codex");
        put(&base, "elsewhere/AGENTS.md", "outside");
        let found = find(&locations(&base), &[]);
        let said: Vec<&str> = found.iter().map(|i| i.content.as_str()).collect();
        assert_eq!(
            said,
            vec![
                "nested claude",
                "local",
                "override",
                "root claude",
                "rule",
                "root agents",
                "user claude",
                "user codex"
            ]
        );
        assert!(found[6].user && !found[0].user);
    }

    /// Disabled paths and empty files are skipped.
    #[test]
    fn skips_disabled_and_empty() {
        let base = scratch("instructions-disabled");
        put(&base, "repo/CLAUDE.md", "keep");
        put(&base, "repo/AGENTS.md", "drop");
        put(&base, "repo/pkg/AGENTS.md", "  \n");
        let off = base.join("repo/AGENTS.md").display().to_string();
        let found = find(&locations(&base), &[off]);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].content, "keep");
    }

    /// Crab's own instruction files come before Claude Code's and Codex's in each scope, and the project's `.oh-my-crab` ones are skipped while the project is not read.
    #[test]
    fn reads_native_files_first() {
        let base = scratch("instructions-native");
        put(&base, "repo/CLAUDE.md", "project claude");
        put(&base, "repo/.oh-my-crab/AGENTS.md", "project crab");
        put(
            &base,
            "repo/.oh-my-crab/rules/style.md",
            "project crab rule",
        );
        put(&base, "home/.claude/CLAUDE.md", "user claude");
        put(&base, "config/oh-my-crab/AGENTS.md", "user crab");
        put(&base, "config/oh-my-crab/rules/a/b.md", "user crab rule");
        let mut locations = locations(&base);
        let said = |locations: &Locations| -> Vec<String> {
            find(locations, &[])
                .into_iter()
                .map(|file| file.content)
                .collect()
        };
        assert_eq!(
            said(&locations),
            vec![
                "project crab",
                "project crab rule",
                "project claude",
                "user crab",
                "user crab rule",
                "user claude"
            ]
        );
        locations.project = false;
        assert_eq!(
            said(&locations),
            vec!["user crab", "user crab rule", "user claude"]
        );
    }

    /// The nearest project system prompt file wins over farther ones and the user's; blank and disabled ones count as absent, and the project's are passed over while it is not read.
    #[test]
    fn finds_system_files() {
        let base = scratch("instructions-system");
        put(&base, "repo/.oh-my-crab/SYSTEM.md", "repo system");
        put(&base, "repo/pkg/.oh-my-crab/SYSTEM.md", "pkg system");
        put(&base, "repo/pkg/.oh-my-crab/APPEND_SYSTEM.md", " \n");
        put(&base, "config/oh-my-crab/SYSTEM.md", "user system");
        put(&base, "config/oh-my-crab/APPEND_SYSTEM.md", "user append");
        let mut locations = locations(&base);
        assert_eq!(
            system_files(&locations),
            SystemFiles {
                replace: Some(base.join("repo/pkg/.oh-my-crab/SYSTEM.md")),
                append: Some(base.join("config/oh-my-crab/APPEND_SYSTEM.md")),
            }
        );
        let off = base
            .join("repo/pkg/.oh-my-crab/SYSTEM.md")
            .display()
            .to_string();
        let (replace, append) = system_texts(&locations, &[off]);
        assert_eq!(replace.unwrap().content, "repo system");
        assert!(append.unwrap().user);
        locations.project = false;
        assert_eq!(
            system_files(&locations).replace,
            Some(base.join("config/oh-my-crab/SYSTEM.md"))
        );
        locations.crab = None;
        assert_eq!(system_files(&locations), SystemFiles::default());
    }

    /// The user's system prompt files are written in crab's own folder and the project's in its `.oh-my-crab` folder.
    #[test]
    fn native_system_files_by_scope() {
        let base = scratch("instructions-native-system");
        let locations = locations(&base);
        assert_eq!(
            native_system_files(&locations, Origin::User),
            SystemFiles {
                replace: Some(base.join("config/oh-my-crab/SYSTEM.md")),
                append: Some(base.join("config/oh-my-crab/APPEND_SYSTEM.md")),
            }
        );
        assert_eq!(
            native_system_files(&locations, Origin::Project).replace,
            Some(base.join("repo/pkg/.oh-my-crab/SYSTEM.md"))
        );
        assert_eq!(
            native_system_files(&locations, Origin::Plugin),
            SystemFiles::default()
        );
    }
}
