//! Where instructions, skills and agent definitions are read from: the union of what Claude Code and Codex read.

/// The folder a place is found under.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Root {
    /// A folder of the project, from the working folder up to the repository root.
    Project,
    /// Claude Code's own config folder.
    Claude,
    /// Codex's own config folder.
    Codex,
    /// The reader's home folder.
    Home,
}

/// How the files of a place are laid out.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shape {
    /// The one file at the path.
    File,
    /// The files ending in this, in the folder and the folders inside it.
    Files(&'static str),
    /// Folders that each hold a `SKILL.md`.
    Skills,
    /// Agent definitions written as `.toml` files.
    Toml,
}

/// One place a kind of customization is read from.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Place {
    /// The folder its path is under.
    pub(crate) root: Root,
    /// Its path under that folder.
    pub(crate) path: &'static str,
    /// How its files are laid out.
    pub(crate) shape: Shape,
}

/// A place at `path` under `root`, laid out as `shape`.
const fn at(root: Root, path: &'static str, shape: Shape) -> Place {
    Place { root, path, shape }
}

/// Where instruction files are read from.
pub(crate) const INSTRUCTIONS: &[Place] = &[
    at(Root::Claude, "CLAUDE.md", Shape::File),
    at(Root::Claude, "rules", Shape::Files(".md")),
    at(Root::Codex, "AGENTS.md", Shape::File),
    at(Root::Codex, "AGENTS.override.md", Shape::File),
    at(Root::Project, "CLAUDE.md", Shape::File),
    at(Root::Project, ".claude/CLAUDE.md", Shape::File),
    at(Root::Project, "CLAUDE.local.md", Shape::File),
    at(Root::Project, ".claude/rules", Shape::Files(".md")),
    at(Root::Project, "AGENTS.md", Shape::File),
    at(Root::Project, "AGENTS.override.md", Shape::File),
];

/// Where skills are read from.
pub(crate) const SKILLS: &[Place] = &[
    at(Root::Project, ".claude/skills", Shape::Skills),
    at(Root::Project, ".codex/skills", Shape::Skills),
    at(Root::Project, ".agents/skills", Shape::Skills),
    at(Root::Claude, "skills", Shape::Skills),
    at(Root::Codex, "skills", Shape::Skills),
    at(Root::Home, ".agents/skills", Shape::Skills),
];

/// Where agent definitions are read from.
pub(crate) const AGENTS: &[Place] = &[
    at(Root::Project, ".claude/agents", Shape::Files(".md")),
    at(Root::Project, ".codex/agents", Shape::Toml),
    at(Root::Claude, "agents", Shape::Files(".md")),
    at(Root::Codex, "agents", Shape::Toml),
];

/// Where instruction files are read from, written for a reader: a path in the project, or one under `~`.
pub fn instruction_places() -> Vec<String> {
    described(INSTRUCTIONS)
}

/// Where skills are read from, written for a reader, installed Claude Code plugins last.
pub fn skill_places() -> Vec<String> {
    let mut places = described(SKILLS);
    places.push(PLUGINS.to_owned());
    places
}

/// Where agent definitions are read from, written for a reader, installed Claude Code plugins last.
pub fn agent_places() -> Vec<String> {
    let mut places = described(AGENTS);
    places.push(PLUGINS.to_owned());
    places
}

/// How a reader is told that installed Claude Code plugins are read too.
const PLUGINS: &str = "installed Claude Code plugins";

/// `places` written for a reader: a path in the project, or one under `~`.
fn described(places: &[Place]) -> Vec<String> {
    places
        .iter()
        .map(|place| {
            let folder = match place.root {
                Root::Project => "",
                Root::Claude => "~/.claude/",
                Root::Codex => "~/.codex/",
                Root::Home => "~/",
            };
            match place.shape {
                Shape::File => format!("{folder}{}", place.path),
                Shape::Files(suffix) => format!("{folder}{}/<name>{suffix}", place.path),
                Shape::Skills => format!("{folder}{}/<name>/SKILL.md", place.path),
                Shape::Toml => format!("{folder}{}/<name>.toml", place.path),
            }
        })
        .collect()
}

impl crate::Locations {
    /// The folders `root` stands for, nearest first; none for the project while its own folders are not read.
    pub(crate) fn folders(&self, root: Root) -> Vec<std::path::PathBuf> {
        match root {
            Root::Project if self.project => self.ancestors(),
            Root::Project => Vec::new(),
            Root::Claude => self.claude.iter().cloned().collect(),
            Root::Codex => self.codex.iter().cloned().collect(),
            Root::Home => self.home.iter().cloned().collect(),
        }
    }
}

/// The files under `folder` whose names end in `suffix`, folders inside it included, in name order.
pub(crate) fn files_under(folder: &std::path::Path, suffix: &str) -> Vec<std::path::PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let mut found = Vec::new();
    for path in paths {
        if path.is_dir() {
            found.extend(files_under(&path, suffix));
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(suffix))
        {
            found.push(path);
        }
    }
    found
}
