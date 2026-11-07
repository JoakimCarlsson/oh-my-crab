//! Where permission rules come from and where "always allow" answers are kept: crab's own permissions file, Claude Code's user settings and the project's Claude Code settings, and what `crab permissions` prints and changes.
//!
//! Crab's file, `permissions.json` in its config folder or the file
//! [`FILE_VARIABLE`] names, holds the user's rules for every project under
//! `permissions` and each project's under `projects`, keyed like trust so that
//! every worktree of a repository shares them. Claude Code's
//! `~/.claude/settings.json` is the user's too. A project's
//! `.claude/settings.json` and `.claude/settings.local.json` come from the
//! repository, so their allow rules are used only while the project's hooks
//! would run; their deny and ask rules only take away and are always used.
//! Crab writes its own file alone, whole, through a temporary file beside it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use omc_agent::permissions::{Effect, Places, Rule, RuleBook};
use omc_agent::{Keeper, Place};
use omc_context::Locations;
use omc_hooks::{Assessment, TrustStore, assess, own_file};
use omc_memory::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// The variable that names crab's permissions file, instead of `permissions.json` under `oh-my-crab` in the user's config folder.
pub const FILE_VARIABLE: &str = "OMC_PERMISSIONS_FILE";

/// The project files rules are read from, in each folder from the working folder up to the repository root.
const PROJECT_FILES: [&str; 2] = [".claude/settings.json", ".claude/settings.local.json"];

/// The three lists of rules a `permissions` object holds, with whatever else it holds kept as it is.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Lists {
    /// Rules that let calls run without asking.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allow: Vec<String>,
    /// Rules that make calls ask.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ask: Vec<String>,
    /// Rules that stop calls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    deny: Vec<String>,
    /// Whatever else the object holds.
    #[serde(flatten)]
    other: Map<String, Value>,
}

impl Lists {
    /// The list of `effect`.
    fn of(&self, effect: Effect) -> &Vec<String> {
        match effect {
            Effect::Allow => &self.allow,
            Effect::Ask => &self.ask,
            Effect::Deny => &self.deny,
        }
    }

    /// The list of `effect`, to change.
    fn of_mut(&mut self, effect: Effect) -> &mut Vec<String> {
        match effect {
            Effect::Allow => &mut self.allow,
            Effect::Ask => &mut self.ask,
            Effect::Deny => &mut self.deny,
        }
    }
}

/// One project's rules in crab's file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ProjectRules {
    /// The folder the project is shown as.
    #[serde(default)]
    root: PathBuf,
    /// Its rules.
    #[serde(default)]
    permissions: Lists,
}

/// Crab's permissions file as it is stored.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Stored {
    /// The user's rules, for every project.
    #[serde(default)]
    permissions: Lists,
    /// Each project's rules, by what tells the project apart.
    #[serde(default)]
    projects: BTreeMap<String, ProjectRules>,
    /// Whatever else the file holds.
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// Crab's own permissions file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PermissionsFile {
    /// Where it is.
    path: PathBuf,
}

impl PermissionsFile {
    /// The file at `path`.
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The file [`FILE_VARIABLE`] names, or `oh-my-crab/permissions.json` in the user's config folder; nothing when there is no config folder.
    pub(crate) fn locate() -> Option<Self> {
        match std::env::var_os(FILE_VARIABLE) {
            Some(path) if !path.is_empty() => Some(Self::new(path)),
            _ => dirs::config_dir()
                .map(|config| Self::new(config.join("oh-my-crab").join("permissions.json"))),
        }
    }

    /// Where the file is.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// What the file holds; an empty file when it does not exist.
    ///
    /// # Errors
    ///
    /// Fails with the reason when it exists but cannot be read or parsed.
    fn read(&self) -> Result<Stored, String> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) if text.trim().is_empty() => Ok(Stored::default()),
            Ok(text) => serde_json::from_str(&text)
                .map_err(|error| format!("{}: {error}", self.path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Stored::default()),
            Err(error) => Err(format!("{}: {error}", self.path.display())),
        }
    }

    /// Reads the file, changes it with `change`, and writes it back whole through a temporary file beside it, so a reader never sees half of it.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the file cannot be read or parsed, which leaves it untouched, or cannot be written.
    fn change(&self, change: impl FnOnce(&mut Stored)) -> Result<(), String> {
        let mut stored = self.read()?;
        change(&mut stored);
        if let Some(folder) = self.path.parent() {
            std::fs::create_dir_all(folder)
                .map_err(|error| format!("{}: {error}", folder.display()))?;
        }
        let text = serde_json::to_string_pretty(&stored).map_err(|error| error.to_string())?;
        let temporary = self
            .path
            .with_extension(format!("json.{}", std::process::id()));
        std::fs::write(&temporary, format!("{text}\n"))
            .and_then(|()| std::fs::rename(&temporary, &self.path))
            .map_err(|error| {
                let _ = std::fs::remove_file(&temporary);
                format!("{}: {error}", self.path.display())
            })
    }

    /// Adds `rules` to the list of `effect`, for `project` or, when there is none, for every project; a rule already listed is not listed twice.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the file cannot be read, parsed or written.
    pub(crate) fn add(
        &self,
        effect: Effect,
        rules: &[String],
        project: Option<&Project>,
    ) -> Result<(), String> {
        self.change(|stored| {
            let lists = match project {
                Some(project) => {
                    let entry = stored.projects.entry(key(project)).or_default();
                    entry.root = project.root.clone();
                    &mut entry.permissions
                }
                None => &mut stored.permissions,
            };
            let list = lists.of_mut(effect);
            for rule in rules {
                if !list.contains(rule) {
                    list.push(rule.clone());
                }
            }
        })
    }

    /// Takes `rule` out of every list, the user's and `project`'s, and says how many times it was listed.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the file cannot be read, parsed or written.
    pub(crate) fn remove(&self, rule: &str, project: &Project) -> Result<usize, String> {
        let wanted = Rule::parse(rule)
            .map(|rule| rule.to_string())
            .unwrap_or_else(|_| rule.trim().to_owned());
        let mut removed = 0;
        self.change(|stored| {
            let mut lists = vec![&mut stored.permissions];
            if let Some(entry) = stored.projects.get_mut(&key(project)) {
                lists.push(&mut entry.permissions);
            }
            for lists in lists {
                for effect in [Effect::Allow, Effect::Ask, Effect::Deny] {
                    let list = lists.of_mut(effect);
                    let before = list.len();
                    list.retain(|listed| {
                        Rule::parse(listed).map_or_else(
                            |_| listed.trim() != wanted,
                            |listed| listed.to_string() != wanted,
                        )
                    });
                    removed += before - list.len();
                }
            }
            stored.projects.retain(|_, entry| {
                !(entry.permissions.allow.is_empty()
                    && entry.permissions.ask.is_empty()
                    && entry.permissions.deny.is_empty()
                    && entry.permissions.other.is_empty())
            });
        })?;
        Ok(removed)
    }
}

/// What tells `project` apart in crab's file, as trust keys it.
fn key(project: &Project) -> String {
    project.key.display().to_string()
}

/// Where a rule was read from.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Origin {
    /// Crab's file, for every project.
    User,
    /// Crab's file, for this project.
    Project,
    /// Claude Code's user settings.
    ClaudeUser,
    /// A project's Claude Code settings, from the repository.
    ClaudeProject,
}

impl Origin {
    /// The origin's name, as `crab permissions` prints it.
    fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
            Self::ClaudeUser => "claude-user",
            Self::ClaudeProject => "claude-project",
        }
    }
}

/// One rule found, where it was found and whether it is used.
#[derive(Clone, Debug)]
pub(crate) struct Found {
    /// The rule, with the folder its paths are read from.
    rule: Rule,
    /// The rule as it was written.
    text: String,
    /// What it does.
    effect: Effect,
    /// Where it was found.
    origin: Origin,
    /// The file it was found in.
    file: PathBuf,
    /// Whether it is in force: an allow rule from a project's Claude Code settings is not while the project is not trusted.
    used: bool,
}

/// Every rule found for a conversation, and what could not be read.
#[derive(Clone, Debug, Default)]
pub(crate) struct Loaded {
    /// The rules in force, with the folders they are read against.
    pub(crate) book: RuleBook,
    /// Every rule found, used or not.
    found: Vec<Found>,
    /// What could not be read or parsed.
    pub(crate) problems: Vec<String>,
}

/// The rules for a conversation in `locations`, whose project and trust are `assessment`'s, read from crab's `file`, Claude Code's user settings and the project's Claude Code settings.
pub(crate) fn load(
    locations: &Locations,
    assessment: &Assessment,
    file: Option<&PermissionsFile>,
) -> Loaded {
    let project = &assessment.project;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root = canonical(&project.root);
    let mut loaded = Loaded {
        book: RuleBook {
            places: Places {
                directory: canonical(&locations.directory),
                root: root.clone(),
                home: locations.home.as_deref().map(canonical),
            },
            ..RuleBook::default()
        },
        ..Loaded::default()
    };
    if let Some(file) = file {
        match file.read() {
            Ok(stored) => {
                loaded.take(&stored.permissions, Origin::User, file.path(), &root, true);
                if let Some(entry) = stored.projects.get(&key(project)) {
                    loaded.take(
                        &entry.permissions,
                        Origin::Project,
                        file.path(),
                        &root,
                        true,
                    );
                }
            }
            Err(problem) => loaded.problems.push(problem),
        }
    }
    if let Some(claude) = &locations.claude {
        let path = claude.join("settings.json");
        if let Some(lists) = loaded.settings(&path) {
            loaded.take(&lists, Origin::ClaudeUser, &path, &root, true);
        }
    }
    let trusted = assessment.standing.runs_project_hooks();
    let home = locations.home.as_deref().map(canonical);
    for folder in locations.ancestors() {
        if home.as_deref() == Some(canonical(&folder).as_path()) {
            continue;
        }
        for name in PROJECT_FILES {
            let path = folder.join(name);
            if let Some(lists) = loaded.settings(&path) {
                loaded.take(
                    &lists,
                    Origin::ClaudeProject,
                    &path,
                    &canonical(&folder),
                    trusted,
                );
            }
        }
    }
    loaded
}

impl Loaded {
    /// The `permissions` object of the Claude Code settings file at `path`; nothing when the file does not exist, and nothing, with the problem noted, when it cannot be parsed.
    fn settings(&mut self, path: &Path) -> Option<Lists> {
        let text = std::fs::read_to_string(path).ok()?;
        match serde_json::from_str::<Value>(&text) {
            Ok(value) => {
                let permissions = value.get("permissions")?.clone();
                match serde_json::from_value(permissions) {
                    Ok(lists) => Some(lists),
                    Err(error) => {
                        self.problems
                            .push(format!("{}: permissions: {error}", path.display()));
                        None
                    }
                }
            }
            Err(error) => {
                self.problems.push(format!("{}: {error}", path.display()));
                None
            }
        }
    }

    /// Takes in the rules of `lists`, found in `file` at `origin`, with `/path` read from `base`; allow rules are put in force only when `allowed` is set, deny and ask rules always. A rule that does not parse is noted as a problem.
    fn take(&mut self, lists: &Lists, origin: Origin, file: &Path, base: &Path, allowed: bool) {
        for effect in [Effect::Deny, Effect::Ask, Effect::Allow] {
            for text in lists.of(effect) {
                let rule = match Rule::parse(text) {
                    Ok(rule) => rule.based(base),
                    Err(problem) => {
                        self.problems.push(format!("{}: {problem}", file.display()));
                        continue;
                    }
                };
                let used = effect != Effect::Allow || allowed;
                if used {
                    let list = match effect {
                        Effect::Allow => &mut self.book.allow,
                        Effect::Ask => &mut self.book.ask,
                        Effect::Deny => &mut self.book.deny,
                    };
                    list.push(rule.clone());
                }
                self.found.push(Found {
                    rule,
                    text: text.clone(),
                    effect,
                    origin,
                    file: file.to_path_buf(),
                    used,
                });
            }
        }
    }
}

/// A [`Keeper`] that adds the rules of "always allow" answers to crab's permissions file, for the project or for every project, and reports what fails through `report`.
pub(crate) struct FileKeeper {
    /// The file.
    file: PermissionsFile,
    /// The project of the conversation.
    project: Project,
    /// Where failures are told.
    report: Box<dyn Fn(String) + Send + Sync>,
}

impl FileKeeper {
    /// A keeper adding to `file` for `project`, telling failures to `report`.
    pub(crate) fn new(
        file: PermissionsFile,
        project: Project,
        report: Box<dyn Fn(String) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            file,
            project,
            report,
        })
    }
}

impl Keeper for FileKeeper {
    /// Adds `rules` to the allow list of the project's section or of the user's, as `place` says.
    fn keep(&self, rules: &[String], place: Place) {
        let project = match place {
            Place::Project => Some(&self.project),
            Place::User => None,
        };
        if let Err(problem) = self.file.add(Effect::Allow, rules, project) {
            (self.report)(format!("cannot keep the permission rules: {problem}"));
        }
    }
}

/// What `crab permissions` prints for `folder`, or the current folder: the project and its trust, crab's file, and every rule found with its effect, origin, file and whether it is used, and what could not be read.
///
/// # Errors
///
/// Fails with the reason when no folder is given and the current one cannot be known.
pub fn permissions(folder: Option<PathBuf>) -> Result<Value, String> {
    let directory = folder_or_current(folder)?;
    let locations = Locations::detect(&directory);
    let store = TrustStore::locate();
    let assessment = assess(
        &locations,
        own_file().as_deref(),
        store.as_ref(),
        false,
        &[],
    );
    let file = PermissionsFile::locate();
    let loaded = load(&locations, &assessment, file.as_ref());
    let rules: Vec<Value> = loaded
        .found
        .iter()
        .map(|found| {
            json!({
                "rule": found.text,
                "parsed": found.rule.to_string(),
                "effect": found.effect.name(),
                "origin": found.origin.name(),
                "file": found.file,
                "used": found.used,
            })
        })
        .collect();
    Ok(json!({
        "project": {
            "root": assessment.project.root,
            "key": assessment.project.key,
        },
        "trust": assessment.standing.name(),
        "file": file.as_ref().map(PermissionsFile::path),
        "rules": rules,
        "problems": loaded.problems,
    }))
}

/// Adds `rule` to the `effect` list of crab's file, for the project `folder`, or the current folder, belongs to when `project` is set and for every project otherwise; then prints the rules as [`permissions`] does.
///
/// # Errors
///
/// Fails with the reason when the effect or the rule cannot be read, the folder cannot be known, there is no config folder, or the file cannot be read, parsed or written.
pub fn add_permission(
    effect: &str,
    rule: &str,
    project: bool,
    folder: Option<PathBuf>,
) -> Result<Value, String> {
    let effect = match effect {
        "allow" => Effect::Allow,
        "ask" => Effect::Ask,
        "deny" => Effect::Deny,
        other => return Err(format!("a rule is allow, ask or deny, not {other}")),
    };
    let rule = Rule::parse(rule)?.to_string();
    let directory = folder_or_current(folder)?;
    let file =
        PermissionsFile::locate().ok_or_else(|| "no config folder to keep rules in".to_owned())?;
    let owner = project.then(|| Project::of(&directory));
    file.add(effect, &[rule], owner.as_ref())?;
    permissions(Some(directory))
}

/// Takes `rule` out of crab's file, from the user's lists and those of the project `folder`, or the current folder, belongs to; then prints the rules as [`permissions`] does, with how many were removed.
///
/// # Errors
///
/// Fails with the reason when the folder cannot be known, there is no config folder, or the file cannot be read, parsed or written.
pub fn remove_permission(rule: &str, folder: Option<PathBuf>) -> Result<Value, String> {
    let directory = folder_or_current(folder)?;
    let file =
        PermissionsFile::locate().ok_or_else(|| "no config folder to keep rules in".to_owned())?;
    let removed = file.remove(rule, &Project::of(&directory))?;
    let mut listed = permissions(Some(directory))?;
    listed["removed"] = json!(removed);
    Ok(listed)
}

/// `folder`, or the current folder when there is none.
///
/// # Errors
///
/// Fails with the reason when the current folder cannot be known.
fn folder_or_current(folder: Option<PathBuf>) -> Result<PathBuf, String> {
    match folder {
        Some(folder) => Ok(folder),
        None => std::env::current_dir().map_err(|error| error.to_string()),
    }
}

#[cfg(test)]
#[path = "permissions_tests.rs"]
mod tests;
