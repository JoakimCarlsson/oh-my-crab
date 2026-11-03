//! Skills: folders that each hold a `SKILL.md` the agent can load when a task matches.

use std::path::{Path, PathBuf};

use crate::Locations;
use crate::frontmatter::parse;
use crate::place::{SKILLS, Shape};

/// One skill found on disk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Skill {
    /// What the skill is called; the name of its folder unless its front matter says otherwise.
    pub name: String,
    /// What the skill is for, which is how the agent knows when to load it.
    pub description: String,
    /// The `SKILL.md` file.
    pub path: PathBuf,
    /// The skill's folder, which may hold more files it refers to.
    pub base: PathBuf,
    /// Whether the skill is kept out of the prompt's list and loaded only when the reader asks for it.
    pub hidden: bool,
}

impl Skill {
    /// The skill's instructions: `SKILL.md` without its front matter.
    ///
    /// # Errors
    ///
    /// Fails when `SKILL.md` cannot be read.
    pub fn body(&self) -> std::io::Result<String> {
        Ok(parse(&std::fs::read_to_string(&self.path)?).body)
    }
}

/// The skills available for work in `locations`, the nearest first; a name found twice keeps its first, and names in `disabled` are skipped.
pub(crate) fn find(locations: &Locations, disabled: &[String]) -> Vec<Skill> {
    let mut found: Vec<Skill> = Vec::new();
    for place in SKILLS.iter().filter(|place| place.shape == Shape::Skills) {
        for folder in locations.folders(place.root) {
            for skill in in_folder(&folder.join(place.path)) {
                let taken = found.iter().any(|held| held.name == skill.name);
                if !taken && !disabled.contains(&skill.name) {
                    found.push(skill);
                }
            }
        }
    }
    found
}

/// The skills in the folders of `skills`, by folder name.
fn in_folder(skills: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(skills) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    folders.iter().filter_map(|base| read(base)).collect()
}

/// The skill in `base`, when it holds a readable `SKILL.md`.
fn read(base: &Path) -> Option<Skill> {
    let path = base.join("SKILL.md");
    let parsed = parse(&std::fs::read_to_string(&path).ok()?);
    let folder_name = base.file_name()?.to_str()?.to_owned();
    let name = parsed
        .get("name")
        .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
        .map_or(folder_name, str::to_owned);
    let description = parsed.get("description").unwrap_or_default().to_owned();
    let hidden = parsed.get("disable-model-invocation") == Some("true")
        || parsed.get("hide") == Some("true");
    Some(Skill {
        name,
        description,
        path,
        base: base.to_path_buf(),
        hidden,
    })
}

#[cfg(test)]
/// Tests for skill discovery.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Builds locations for `base`, with the working folder `base/repo`.
    fn locations(base: &Path) -> Locations {
        Locations {
            directory: base.join("repo"),
            root: Some(base.join("repo")),
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: Some(base.join("home/.codex")),
        }
    }

    /// Skills are found in every location, the project's first, and a name is kept once.
    #[test]
    fn finds_skills_in_every_place() {
        let base = scratch("skills-find");
        put(
            &base,
            "repo/.claude/skills/pdf/SKILL.md",
            "---\nname: pdf\ndescription: Reads PDFs\n---\nUse pdftotext.",
        );
        put(
            &base,
            "repo/.agents/skills/lint/SKILL.md",
            "---\ndescription: Lints\n---\nRun it.",
        );
        put(
            &base,
            "repo/.codex/skills/other/SKILL.md",
            "---\nname: other\ndescription: o\n---\nx",
        );
        put(
            &base,
            "home/.claude/skills/pdf/SKILL.md",
            "---\nname: pdf\ndescription: user copy\n---\nx",
        );
        put(
            &base,
            "home/.codex/skills/mine/SKILL.md",
            "---\ndescription: m\n---\nx",
        );
        put(
            &base,
            "home/.agents/skills/shared/SKILL.md",
            "---\ndescription: s\n---\nx",
        );
        put(&base, "home/.claude/skills/nofile/README.md", "x");
        let found = find(&locations(&base), &[]);
        let names: Vec<&str> = found.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["pdf", "other", "lint", "mine", "shared"]);
        assert_eq!(found[0].description, "Reads PDFs");
        assert_eq!(found[0].body().unwrap(), "Use pdftotext.");
    }

    /// Disabled names are skipped and hidden skills are marked.
    #[test]
    fn skips_disabled_and_marks_hidden() {
        let base = scratch("skills-disabled");
        put(
            &base,
            "repo/.claude/skills/a/SKILL.md",
            "---\ndescription: a\n---\nx",
        );
        put(
            &base,
            "repo/.claude/skills/b/SKILL.md",
            "---\ndescription: b\ndisable-model-invocation: true\n---\nx",
        );
        let found = find(&locations(&base), &["a".to_owned()]);
        assert_eq!(found.len(), 1);
        assert!(found[0].hidden);
    }
}
