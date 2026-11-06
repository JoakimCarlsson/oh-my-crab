//! The instruction files a project and its reader keep for the agent.

use std::path::{Path, PathBuf};

use crate::Locations;
use crate::place::{INSTRUCTIONS, Place, Root, Shape, files_under};

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
        &[Root::Claude, Root::Codex]
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
    let shown = path.display().to_string();
    if disabled
        .iter()
        .any(|entry| *entry == shown || *entry == canonical.display().to_string())
    {
        return;
    }
    if found.iter().any(|held| {
        held.path
            .canonicalize()
            .unwrap_or_else(|_| held.path.clone())
            == canonical
    }) {
        return;
    }
    let Ok(mut content) = std::fs::read_to_string(&path) else {
        return;
    };
    if content.trim().is_empty() {
        return;
    }
    if content.len() > LIMIT {
        let mut end = LIMIT;
        while !content.is_char_boundary(end) {
            end -= 1;
        }
        content.truncate(end);
    }
    found.push(Instruction {
        path,
        content,
        user,
    });
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
}
