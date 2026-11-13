//! The instruction files a project and its reader keep for the agent.

use std::path::{Path, PathBuf};

use ignore::overrides::OverrideBuilder;

use crate::frontmatter::{parse, unquote};
use crate::place::{INSTRUCTIONS, Place, Root, Shape, files_under};
use crate::scope::PROJECT_FOLDER;
use crate::template::render;
use crate::tree::SKIPPED;
use crate::{Discovered, Locations, Origin};

/// What a tool result ends with when instruction files start to apply to the files its call touched.
const PATH_RULES: &str = include_str!("prompts/path_rules.md");

/// The line that opens the block of instruction files a tool result ends with.
const PATH_RULES_OPEN: &str = "<path-rules>\n";

/// How the line that opens one instruction file in a block starts.
const FILE_OPEN: &str = "<file path=\"";

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

/// An instruction file that applies only to some of the project's files: a rule whose front matter names paths, or a file kept in a folder below the folders the system prompt is built from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scoped {
    /// The file, holding what follows its front matter.
    pub file: Instruction,
    /// The folder it applies below, which its patterns are written from.
    pub base: PathBuf,
    /// The paths it applies to, in the syntax of a `.gitignore` line; none when it applies to every file below `base`.
    pub patterns: Vec<String>,
}

impl Scoped {
    /// Whether the file applies to `file`, an absolute path with its links resolved: `file` is below [`Scoped::base`] and, when there are patterns, one of them takes it and no later `!` pattern leaves it out.
    pub fn covers(&self, file: &Path) -> bool {
        let Ok(inside) = file.strip_prefix(&self.base) else {
            return false;
        };
        if self.patterns.is_empty() {
            return true;
        }
        let mut builder = OverrideBuilder::new(&self.base);
        for pattern in &self.patterns {
            let _ = builder.add(pattern);
        }
        builder
            .build()
            .is_ok_and(|globs| globs.matched(inside, false).is_whitelist())
    }
}

/// The instruction files found, told apart by whether they always apply.
#[derive(Debug, Default)]
pub(crate) struct Found {
    /// The files that go into the system prompt.
    pub(crate) always: Vec<Instruction>,
    /// The files that apply only to some of the project's files.
    pub(crate) scoped: Vec<Scoped>,
}

/// The instruction files the agent reads for work in `locations`, the project's first and nearest first, then the reader's; the paths in `disabled` are skipped.
///
/// A rule file whose front matter names paths under `paths` or `globs`, and does not say `alwaysApply: true`, is scoped: a project's to the folder its rules folder is kept in, the reader's to the repository root, or to the working folder outside a repository.
pub(crate) fn find(locations: &Locations, disabled: &[String]) -> Found {
    let mut found = Found::default();
    let project = locations
        .root
        .clone()
        .unwrap_or_else(|| locations.directory.clone());
    for user in [false, true] {
        for folder in folders(locations, user) {
            let base = if user { &project } else { &folder.1 };
            for place in INSTRUCTIONS
                .iter()
                .filter(|place| (place.root != Root::Project) == user)
                .filter(|place| place.root == folder.0)
            {
                for path in paths(&folder.1, place) {
                    admit(&mut found, path, user, disabled, place.shape, base);
                }
            }
        }
    }
    found
}

/// The instruction files that start to apply once the agent touches `file`, nearest first: those kept in the folders between `file` and the folders the system prompt is built from, then the scoped rules of `discovered` that cover it. The files `disabled` names, the files `discovered` already holds as always applying, and `file` itself are left out.
///
/// Folders are looked in only below the repository root, or below the working folder outside a repository, only while the project's own folders are read, and never inside the folders of version control, builds and vendored packages.
pub fn instructions_for(
    locations: &Locations,
    discovered: &Discovered,
    disabled: &[String],
    file: &Path,
) -> Vec<Instruction> {
    let file = settled(file);
    let mut nested = Found::default();
    for folder in below(locations, &file) {
        for place in INSTRUCTIONS
            .iter()
            .filter(|place| place.root == Root::Project)
        {
            for path in paths(&folder, place) {
                admit(&mut nested, path, false, disabled, place.shape, &folder);
            }
        }
    }
    let mut held: Vec<PathBuf> = discovered
        .instructions
        .iter()
        .map(|known| canonical(&known.path))
        .collect();
    held.push(file.clone());
    let scoped = nested
        .scoped
        .into_iter()
        .chain(discovered.scoped.iter().cloned())
        .filter(|rule| rule.covers(&file))
        .map(|rule| rule.file);
    let mut applying = Vec::new();
    for found in nested.always.into_iter().chain(scoped) {
        let path = canonical(&found.path);
        if !held.contains(&path) {
            held.push(path);
            applying.push(found);
        }
    }
    applying
}

/// The folders from the one `file` is in up to, and leaving out, the folders the system prompt is built from, nearest first; none when `file` is outside the project, the project's own folders are not read, or a folder on the way is one of version control, builds or vendored packages.
fn below(locations: &Locations, file: &Path) -> Vec<PathBuf> {
    if !locations.project {
        return Vec::new();
    }
    let top = canonical(locations.root.as_deref().unwrap_or(&locations.directory));
    let known: Vec<PathBuf> = locations
        .ancestors()
        .iter()
        .map(|folder| canonical(folder))
        .collect();
    let mut folders = Vec::new();
    for folder in file.ancestors().skip(1) {
        if !folder.starts_with(&top) || known.iter().any(|known| known == folder) {
            break;
        }
        folders.push(folder.to_path_buf());
    }
    let vendored = file.strip_prefix(&top).is_ok_and(|inside| {
        inside.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| SKIPPED.contains(&name))
        })
    });
    if vendored {
        return Vec::new();
    }
    folders
}

/// `path` with its links resolved, or as it is when it cannot be.
fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// `file` with the links of its folder resolved, whether or not the file exists yet.
fn settled(file: &Path) -> PathBuf {
    match (file.parent(), file.file_name()) {
        (Some(folder), Some(name)) => canonical(folder).join(name),
        _ => file.to_path_buf(),
    }
}

/// `files` as the blocks a prompt shows them in, one `<file path="...">` block each.
pub(crate) fn blocks<'a>(files: impl IntoIterator<Item = &'a Instruction>) -> String {
    files
        .into_iter()
        .map(|file| {
            format!(
                "{FILE_OPEN}{}\">\n{}\n</file>",
                file.path.display(),
                file.content.trim_end()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The block a tool result ends with to hand the agent `files`, instruction files that apply to what its call touched.
pub fn path_rules(files: &[Instruction]) -> String {
    let rules = blocks(files);
    render(PATH_RULES, &[("rules", rules.as_str())])
}

/// The instruction files the block [`path_rules`] made at the end of `text`, a tool result, hands over, by path; none when `text` ends with no such block.
pub fn path_rules_in(text: &str) -> Vec<PathBuf> {
    let Some(start) = text.rfind(PATH_RULES_OPEN) else {
        return Vec::new();
    };
    text[start..]
        .lines()
        .filter_map(|line| line.strip_prefix(FILE_OPEN)?.strip_suffix("\">"))
        .map(PathBuf::from)
        .collect()
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

/// Reads `path` into `found`, unless it is disabled, empty, unreadable or already there, marked as the reader's own when `user` is set. A file of a place shaped as `shape` whose front matter scopes it is kept as a rule scoped to `base`, with what follows the front matter as its text, and skipped when nothing follows.
fn admit(
    found: &mut Found,
    path: PathBuf,
    user: bool,
    disabled: &[String],
    shape: Shape,
    base: &Path,
) {
    let resolved = canonical(&path);
    if found
        .always
        .iter()
        .chain(found.scoped.iter().map(|rule| &rule.file))
        .any(|held| canonical(&held.path) == resolved)
    {
        return;
    }
    let Some(instruction) = read(path, user, disabled) else {
        return;
    };
    let scope = match shape {
        Shape::Files(_) => scope(&instruction.content),
        Shape::File | Shape::Skills | Shape::Toml => None,
    };
    match scope {
        None => found.always.push(instruction),
        Some((_, body)) if body.trim().is_empty() => {}
        Some((patterns, body)) => found.scoped.push(Scoped {
            file: Instruction {
                content: body,
                ..instruction
            },
            base: canonical(base),
            patterns,
        }),
    }
}

/// The patterns the front matter of the rule file `content` names under `paths` or `globs`, with what follows the front matter; nothing when it names none or says `alwaysApply: true`.
fn scope(content: &str) -> Option<(Vec<String>, String)> {
    let parsed = parse(content);
    if parsed
        .get("alwaysapply")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
    {
        return None;
    }
    let named: Vec<String> = ["paths", "globs"]
        .iter()
        .filter_map(|key| parsed.get(key))
        .flat_map(patterns)
        .collect();
    (!named.is_empty()).then_some((named, parsed.body))
}

/// The patterns in `value`, a front matter value: one to a line or separated by commas, with list dashes, brackets and quotes taken off; a comma inside braces stays part of its pattern.
fn patterns(value: &str) -> Vec<String> {
    let value = value.trim();
    let value = value
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(value);
    let mut items = vec![String::new()];
    let mut depth = 0usize;
    for character in value.chars() {
        match character {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            _ => {}
        }
        if character == '\n' || (character == ',' && depth == 0) {
            items.push(String::new());
        } else if let Some(item) = items.last_mut() {
            item.push(character);
        }
    }
    items
        .iter()
        .map(|item| unquote(item.trim().trim_start_matches("- ")))
        .filter(|item| !item.is_empty())
        .collect()
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
        let found = find(&locations(&base), &[]).always;
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
        let found = find(&locations(&base), &[off]).always;
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
                .always
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

    /// A rule whose front matter names paths is kept out of the always-on files, scoped to the folder its rules folder is in or, for the reader's, to the repository root; `alwaysApply: true` keeps a rule always on.
    #[test]
    fn rules_with_paths_are_scoped() {
        let base = scratch("instructions-scoped");
        put(&base, "repo/AGENTS.md", "root agents");
        put(
            &base,
            "repo/.claude/rules/ts.md",
            "---\npaths:\n  - \"src/**/*.{ts,tsx}\"\n  - \"!src/gen/**\"\n---\nUse strict mode.\n",
        );
        put(
            &base,
            "repo/.oh-my-crab/rules/always.md",
            "---\nglobs: \"*.rs\"\nalwaysApply: true\n---\nAlways.",
        );
        put(
            &base,
            "repo/.oh-my-crab/rules/empty.md",
            "---\nglobs: \"*.rs\"\n---\n",
        );
        put(
            &base,
            "home/.claude/rules/md.md",
            "---\nglobs: *.md, docs/**\n---\nWrap at 80.",
        );
        let found = find(&locations(&base), &[]);
        let always: Vec<&str> = found
            .always
            .iter()
            .map(|file| file.content.as_str())
            .collect();
        assert_eq!(
            always,
            vec![
                "---\nglobs: \"*.rs\"\nalwaysApply: true\n---\nAlways.",
                "root agents"
            ]
        );
        assert_eq!(found.scoped.len(), 2);
        let ts = &found.scoped[0];
        assert_eq!(ts.file.content, "Use strict mode.");
        assert_eq!(ts.base, base.join("repo"));
        assert_eq!(ts.patterns, vec!["src/**/*.{ts,tsx}", "!src/gen/**"]);
        assert!(ts.covers(&base.join("repo/src/a/b.tsx")));
        assert!(!ts.covers(&base.join("repo/src/gen/b.ts")));
        assert!(!ts.covers(&base.join("repo/lib/b.ts")));
        assert!(!ts.covers(&base.join("elsewhere/src/b.ts")));
        let md = &found.scoped[1];
        assert!(md.file.user);
        assert_eq!(md.patterns, vec!["*.md", "docs/**"]);
        assert!(md.covers(&base.join("repo/pkg/deep/README.md")));
        assert!(md.covers(&base.join("repo/docs/a.txt")));
        assert!(!md.covers(&base.join("repo/pkg/docs/a.txt")));
    }

    /// Touching a file brings the instruction files of the folders between it and the folders already read, nearest first, then the scoped rules that cover it; files already in the prompt, disabled ones, the touched file itself, vendored folders and an unread project bring nothing.
    #[test]
    fn touching_a_file_brings_its_folders_files() {
        let base = scratch("instructions-nested");
        put(&base, "repo/AGENTS.md", "root agents");
        put(&base, "repo/pkg/AGENTS.md", "pkg agents");
        put(&base, "repo/pkg/app/AGENTS.md", "app agents");
        put(&base, "repo/pkg/app/ui/CLAUDE.md", "ui claude");
        put(
            &base,
            "repo/pkg/app/.claude/rules/css.md",
            "---\npaths: \"*.css\"\n---\nNo ids.",
        );
        put(&base, "repo/other/AGENTS.md", "other agents");
        put(&base, "repo/other/node_modules/dep/AGENTS.md", "dep agents");
        put(
            &base,
            "repo/.claude/rules/rs.md",
            "---\npaths: \"*.rs\"\n---\nNo unwrap.",
        );
        put(&base, "elsewhere/AGENTS.md", "outside");
        let mut locations = locations(&base);
        let discovered = crate::discover(&locations, &[]);
        let said = |locations: &Locations, disabled: &[String], file: &str| -> Vec<String> {
            instructions_for(locations, &discovered, disabled, &base.join(file))
                .into_iter()
                .map(|file| file.content)
                .collect()
        };
        assert_eq!(
            said(&locations, &[], "repo/pkg/app/ui/main.rs"),
            vec!["ui claude", "app agents", "No unwrap."]
        );
        assert_eq!(
            said(&locations, &[], "repo/pkg/app/ui/new/style.css"),
            vec!["ui claude", "app agents", "No ids."]
        );
        assert_eq!(said(&locations, &[], "repo/pkg/lib.rs"), vec!["No unwrap."]);
        assert_eq!(
            said(&locations, &[], "repo/other/x.txt"),
            vec!["other agents"]
        );
        assert!(said(&locations, &[], "repo/other/AGENTS.md").is_empty());
        assert!(said(&locations, &[], "repo/other/node_modules/dep/x.js").is_empty());
        assert!(said(&locations, &[], "elsewhere/x.txt").is_empty());
        let off = base.join("repo/pkg/app/AGENTS.md").display().to_string();
        assert_eq!(
            said(&locations, &[off], "repo/pkg/app/x.txt"),
            Vec::<String>::new()
        );
        locations.project = false;
        assert!(said(&locations, &[], "repo/pkg/app/ui/x.txt").is_empty());
    }

    /// The block a tool result ends with names its files, and they are read back from the end of the result alone.
    #[test]
    fn path_rules_are_read_back() {
        let file = |path: &str, content: &str| Instruction {
            path: PathBuf::from(path),
            content: content.to_owned(),
            user: false,
        };
        let block = path_rules(&[file("/w/a/AGENTS.md", "A {{x}}\n"), file("/w/r.md", "R")]);
        assert!(block.starts_with("<path-rules>\n"));
        assert!(block.contains("<file path=\"/w/a/AGENTS.md\">\nA {{x}}\n</file>"));
        assert!(block.trim_end().ends_with("</path-rules>"));
        let result = format!("1\tfn main() {{}}\n\n{block}");
        assert_eq!(
            path_rules_in(&result),
            vec![PathBuf::from("/w/a/AGENTS.md"), PathBuf::from("/w/r.md")]
        );
        assert!(path_rules_in("<file path=\"/w/a/AGENTS.md\">\nA\n</file>").is_empty());
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
