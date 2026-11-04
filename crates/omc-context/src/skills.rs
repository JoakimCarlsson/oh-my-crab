//! Skills: folders that each hold a `SKILL.md` the agent can load when a task matches.

use std::path::{Path, PathBuf};

use crate::frontmatter::{Parsed, parse};
use crate::place::{SKILLS, Shape};
use crate::plugins::installed;
use crate::{Locations, Origin};

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
    /// Where the skill came from.
    pub origin: Origin,
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

/// The skills available for work in `locations`: the project's nearest first, then the reader's, then those installed Claude Code plugins bring; names in `disabled` are left out.
///
/// The first skill found under a name keeps it. A later one with the same front matter and body is dropped; a later one that differs is named `<namespace>/<name>`, after its plugin or the folder its skills are in, with `~2`, `~3` and on added when that is taken too. Names are settled before `disabled` is applied, so turning one skill off never renames another.
pub(crate) fn find(locations: &Locations, disabled: &[String]) -> Vec<Skill> {
    let mut found = Admitted::default();
    for place in SKILLS.iter().filter(|place| place.shape == Shape::Skills) {
        for folder in locations.folders(place.root) {
            let skills = folder.join(place.path);
            let namespace = folder_namespace(&skills);
            for skill in in_folder(&skills, place.root.into(), false) {
                found.admit(skill, &namespace);
            }
        }
    }
    for plugin in installed(locations) {
        let namespace = sanitized(&plugin.name);
        for folder in plugin.skill_folders() {
            for skill in in_folder(&folder, Origin::Plugin, true) {
                found.admit(skill, &namespace);
            }
        }
    }
    found
        .skills
        .into_iter()
        .filter(|skill| !disabled.contains(&skill.name))
        .collect()
}

/// The skills taken in so far, each with the name it was found under and what its file holds.
#[derive(Default)]
struct Admitted {
    /// The skills, under the names they are offered by.
    skills: Vec<Skill>,
    /// For each skill, the name its file gave it and the file read.
    held: Vec<(String, Parsed)>,
}

impl Admitted {
    /// Takes in `skill`, unless a skill of its name with the same file was taken already; when its name is taken by a different one it is named after `namespace` instead.
    fn admit(&mut self, mut skill: Skill, namespace: &str) {
        let parsed = std::fs::read_to_string(&skill.path)
            .map(|text| parse(&text))
            .unwrap_or_default();
        let raw = skill.name.clone();
        let mut same = self.held.iter().filter(|(name, _)| *name == raw).peekable();
        if same.peek().is_some() {
            if same.any(|(_, held)| *held == parsed) {
                return;
            }
            skill.name = self.free_name(namespace, &raw);
        }
        self.held.push((raw, parsed));
        self.skills.push(skill);
    }

    /// `<namespace>/<name>`, or the first of it with `~2`, `~3` and on added that no skill is offered by.
    fn free_name(&self, namespace: &str, name: &str) -> String {
        let taken = |candidate: &str| self.skills.iter().any(|skill| skill.name == candidate);
        let base = format!("{namespace}/{name}");
        if !taken(&base) {
            return base;
        }
        (2..)
            .map(|number| format!("{base}~{number}"))
            .find(|candidate| !taken(candidate))
            .unwrap_or(base)
    }
}

/// The namespace of skills found in the folder `skills`: the name of the folder that holds it, without a leading dot.
fn folder_namespace(skills: &Path) -> String {
    let owner = skills
        .parent()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    sanitized(owner.trim_start_matches('.'))
}

/// `name` as one token: each run of characters other than letters, digits, `_` and `-` made a `-`, with none at either end; `skills` when nothing is left.
fn sanitized(name: &str) -> String {
    let mut token = String::new();
    for character in name.chars() {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            token.push(character);
        } else if !token.ends_with('-') {
            token.push('-');
        }
    }
    let token = token.trim_matches('-');
    if token.is_empty() {
        "skills".to_owned()
    } else {
        token.to_owned()
    }
}

/// The skills in `folder`: the folder itself when `include_self` is set and it holds a `SKILL.md`, then each folder inside it that does, by name, hidden folders left out; each is marked as coming from `origin`.
fn in_folder(folder: &Path, origin: Origin, include_self: bool) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    if include_self {
        folders.insert(0, folder.to_path_buf());
    }
    folders
        .iter()
        .filter_map(|base| read(base, origin))
        .collect()
}

/// The skill in `base`, coming from `origin`, when it holds a readable `SKILL.md` that is not switched off with `enabled: false` and whose name holds no path separator.
fn read(base: &Path, origin: Origin) -> Option<Skill> {
    let path = base.join("SKILL.md");
    let parsed = parse(&std::fs::read_to_string(&path).ok()?);
    if parsed.get("enabled") == Some("false") {
        return None;
    }
    let folder_name = base.file_name()?.to_str()?.to_owned();
    let name = parsed
        .get("name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map_or(folder_name, str::to_owned);
    if name.contains(['/', '\\']) {
        return None;
    }
    let description = parsed.get("description").unwrap_or_default().to_owned();
    let hidden = parsed.get("disable-model-invocation") == Some("true")
        || parsed.get("hide") == Some("true");
    Some(Skill {
        name,
        description,
        path,
        base: base.to_path_buf(),
        hidden,
        origin,
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

    /// Skills are found in every location, the project's first, and a later skill of a taken name is named after the folder its skills are in.
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
        assert_eq!(
            names,
            vec!["pdf", "other", "lint", "claude/pdf", "mine", "shared"]
        );
        assert_eq!(found[0].description, "Reads PDFs");
        assert_eq!(found[0].origin, Origin::Project);
        assert_eq!(found[3].origin, Origin::User);
        assert_eq!(found[3].description, "user copy");
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

    /// Plugin skills keep their own names; one whose name is taken by a different skill is named after its plugin, one identical to it is dropped, and turning a skill off renames no other.
    #[test]
    fn finds_plugin_skills() {
        let base = scratch("skills-plugins");
        put(
            &base,
            "home/.claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"kit@market":[{{"installPath":"{}/kit"}}]}}}}"#,
                base.display()
            ),
        );
        let same = "---\ndescription: Shared\n---\nSame.";
        put(
            &base,
            "kit/skills/commit/SKILL.md",
            "---\nname: commit\ndescription: Commits\n---\nCommit it.",
        );
        put(
            &base,
            "kit/skills/review/SKILL.md",
            "---\ndescription: Reviews\n---\nx",
        );
        put(&base, "kit/skills/shared/SKILL.md", same);
        put(
            &base,
            "kit/skills/off/SKILL.md",
            "---\nenabled: false\n---\nx",
        );
        put(
            &base,
            "kit/skills/.hidden/SKILL.md",
            "---\ndescription: h\n---\nx",
        );
        put(&base, "kit/skills/bad/SKILL.md", "---\nname: a/b\n---\nx");
        put(
            &base,
            "home/.claude/skills/commit/SKILL.md",
            "---\ndescription: mine\n---\nx",
        );
        put(&base, "home/.claude/skills/shared/SKILL.md", same);
        let names = |disabled: &[String]| -> Vec<String> {
            find(&locations(&base), disabled)
                .into_iter()
                .map(|skill| skill.name)
                .collect()
        };
        assert_eq!(names(&[]), vec!["commit", "shared", "kit/commit", "review"]);
        assert_eq!(
            names(&["commit".to_owned()]),
            vec!["shared", "kit/commit", "review"]
        );
        let found = find(&locations(&base), &[]);
        assert_eq!(found[2].description, "Commits");
        assert_eq!(found[2].origin, Origin::Plugin);
        assert_eq!(found[2].body().unwrap(), "Commit it.");
    }

    /// A taken namespaced name gets a number, and folders are named after the folder their skills are in.
    #[test]
    fn numbers_taken_names() {
        let mut admitted = Admitted::default();
        let base = scratch("skills-numbers");
        for (folder, body) in [("a", "one"), ("b", "two"), ("c", "three")] {
            put(&base, &format!("{folder}/x/SKILL.md"), body);
            admitted.admit(
                read(&base.join(folder).join("x"), Origin::User).unwrap(),
                "kit",
            );
        }
        let names: Vec<&str> = admitted.skills.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["x", "kit/x", "kit/x~2"]);
        assert_eq!(folder_namespace(Path::new("/r/.claude/skills")), "claude");
        assert_eq!(sanitized("my plugin!"), "my-plugin");
    }
}
