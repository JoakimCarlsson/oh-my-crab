//! Where hooks are defined and how their files are read.
//!
//! Every file has the shape of the `hooks` key of Claude Code's `settings.json`: each event names a list of groups, each group a `matcher` and the `hooks` to run, each hook a `type` of `command`, the `command` and an optional `timeout` in seconds.

use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_context::{Locations, plugin_folders};
use serde_json::Value;

/// The variable that names the file the user's own hooks for crab are read from, instead of `hooks.json` in crab's config folder.
pub const HOOKS_FILE_VARIABLE: &str = "OMC_HOOKS_FILE";

/// The point in a conversation a hook runs at.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum HookEvent {
    /// Before a tool call is asked about or run.
    PreToolUse,
    /// After a tool call succeeded.
    PostToolUse,
    /// When the reader's prompt is about to start a turn.
    UserPromptSubmit,
    /// When the main agent is about to end its turn.
    Stop,
    /// When a subagent is about to end its work.
    SubagentStop,
}

impl HookEvent {
    /// Every event, in the order they are listed.
    pub const ALL: [Self; 5] = [
        Self::PreToolUse,
        Self::PostToolUse,
        Self::UserPromptSubmit,
        Self::Stop,
        Self::SubagentStop,
    ];

    /// The event's name, as files spell it and hooks are told it.
    pub fn name(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::Stop => "Stop",
            Self::SubagentStop => "SubagentStop",
        }
    }

    /// The event named `name`, when crab runs hooks for it.
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.name() == name)
    }

    /// How long a hook of this event may run when its definition gives no timeout.
    pub fn default_timeout(self) -> Duration {
        match self {
            Self::UserPromptSubmit => Duration::from_secs(30),
            _ => Duration::from_secs(60),
        }
    }

    /// Whether the event's matcher names tools, rather than kinds of subagent or nothing.
    pub fn matches_tools(self) -> bool {
        matches!(self, Self::PreToolUse | Self::PostToolUse)
    }
}

/// Where a hook comes from, which decides whether it needs the project to be trusted.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum HookOrigin {
    /// The user's own config, for every project; always trusted.
    User,
    /// A Claude Code plugin the user installed; trusted as the user's, unless only the project's settings switched it on.
    Plugin,
    /// The project folder; used only once the project is trusted.
    Project,
}

impl HookOrigin {
    /// The origin's name, as `crab hooks` prints it.
    pub fn name(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Plugin => "plugin",
            Self::Project => "project",
        }
    }
}

/// One command to run at one event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Hook {
    /// When it runs.
    pub event: HookEvent,
    /// Which tools, or kinds of subagent, it runs for, as written; empty for all.
    pub matcher: String,
    /// The shell command line.
    pub command: String,
    /// How long it may run, when its definition says.
    pub timeout: Option<Duration>,
    /// The file it is defined in.
    pub file: PathBuf,
    /// Where it comes from.
    pub origin: HookOrigin,
    /// Whether it is defined in a file of Claude Code's, so it is told Claude Code's names for tools.
    pub claude: bool,
    /// The folder of the plugin that brings it, when a plugin does.
    pub plugin: Option<PathBuf>,
}

impl Hook {
    /// How long it may run.
    pub fn limit(&self) -> Duration {
        self.timeout.unwrap_or_else(|| self.event.default_timeout())
    }

    /// The hook in a few words, for a reader: its event, matcher and command.
    pub fn label(&self) -> String {
        match self.matcher.trim() {
            "" | "*" => format!("{}: {}", self.event.name(), self.command),
            matcher => format!("{} {matcher}: {}", self.event.name(), self.command),
        }
    }
}

/// The hooks found, and what could not be read.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Loaded {
    /// The hooks, the user's first, then the plugins', then the project's.
    pub hooks: Vec<Hook>,
    /// One line for each file or entry that could not be read.
    pub problems: Vec<String>,
}

/// The file the user's own hooks for crab are read from: the one [`HOOKS_FILE_VARIABLE`] names, or `hooks.json` in crab's config folder; nothing when there is no config folder.
pub fn own_file() -> Option<PathBuf> {
    match std::env::var_os(HOOKS_FILE_VARIABLE) {
        Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
        _ => dirs::config_dir().map(|base| base.join("oh-my-crab").join("hooks.json")),
    }
}

/// The files of the project folder hooks are read from, under each of its folders from the working folder up to the repository root, and whether each is Claude Code's.
pub const PROJECT_FILES: [(&str, bool); 3] = [
    (".oh-my-crab/hooks.json", false),
    (".claude/settings.json", true),
    (".claude/settings.local.json", true),
];

/// Reads the hooks for work in `locations`: the user's own file `own` for crab, Claude Code's user `settings.json`, the `hooks/hooks.json` of each installed plugin that applies, and, when the project's own folders are read, the project's files of [`PROJECT_FILES`].
///
/// A file that does not exist is skipped; one that cannot be parsed is reported in [`Loaded::problems`] and skipped. When a file that is read sets `disableAllHooks` to `true`, as Claude Code's settings can, no hook runs at all.
pub fn load(locations: &Locations, own: Option<&Path>) -> Loaded {
    let mut sources: Vec<(PathBuf, HookOrigin, bool, Option<PathBuf>)> = Vec::new();
    if let Some(own) = own {
        sources.push((own.to_path_buf(), HookOrigin::User, false, None));
    }
    if let Some(claude) = &locations.claude {
        sources.push((claude.join("settings.json"), HookOrigin::User, true, None));
    }
    for (_, folder) in plugin_folders(locations) {
        sources.push((
            folder.join("hooks/hooks.json"),
            HookOrigin::Plugin,
            true,
            Some(folder),
        ));
    }
    if locations.project {
        for folder in locations.ancestors() {
            for (file, claude) in PROJECT_FILES {
                sources.push((folder.join(file), HookOrigin::Project, claude, None));
            }
        }
    }
    let mut loaded = Loaded::default();
    let mut disabled = false;
    for (file, origin, claude, plugin) in sources {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        let parsed: Value = match serde_json::from_str(&text) {
            Ok(parsed) => parsed,
            Err(error) => {
                loaded
                    .problems
                    .push(format!("{}: not JSON: {error}", file.display()));
                continue;
            }
        };
        disabled |= parsed["disableAllHooks"] == Value::Bool(true);
        let found = parse(&parsed, &file, origin, claude, plugin.as_deref());
        loaded.hooks.extend(found.hooks);
        loaded.problems.extend(found.problems);
    }
    if disabled {
        loaded.hooks.clear();
    }
    loaded
}

/// The hooks `settings`, the JSON of the file `file`, defines under its `hooks` key, each from `origin`, told Claude Code's tool names when `claude` is set, and brought by the plugin in `plugin` when there is one.
///
/// Events crab does not run hooks for, and hooks of another type than `command`, are skipped quietly, so a file written for Claude Code can be shared; entries of the wrong shape are reported.
pub fn parse(
    settings: &Value,
    file: &Path,
    origin: HookOrigin,
    claude: bool,
    plugin: Option<&Path>,
) -> Loaded {
    let mut loaded = Loaded::default();
    let Some(events) = settings.get("hooks") else {
        return loaded;
    };
    let Some(events) = events.as_object() else {
        loaded
            .problems
            .push(format!("{}: `hooks` is not an object", file.display()));
        return loaded;
    };
    for (name, groups) in events {
        let Some(event) = HookEvent::parse(name) else {
            continue;
        };
        let Some(groups) = groups.as_array() else {
            loaded
                .problems
                .push(format!("{}: `hooks.{name}` is not a list", file.display()));
            continue;
        };
        for group in groups {
            let matcher = group["matcher"].as_str().unwrap_or_default().to_owned();
            let Some(commands) = group["hooks"].as_array() else {
                loaded.problems.push(format!(
                    "{}: a `{name}` entry has no `hooks` list",
                    file.display()
                ));
                continue;
            };
            for command in commands {
                if command["type"].as_str().unwrap_or("command") != "command" {
                    continue;
                }
                let Some(line) = command["command"]
                    .as_str()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                else {
                    loaded.problems.push(format!(
                        "{}: a `{name}` hook has no `command`",
                        file.display()
                    ));
                    continue;
                };
                let timeout = command["timeout"]
                    .as_f64()
                    .filter(|seconds| *seconds > 0.0 && seconds.is_finite())
                    .map(|seconds| Duration::from_secs_f64(seconds.min(86_400.0)));
                loaded.hooks.push(Hook {
                    event,
                    matcher: matcher.clone(),
                    command: line.to_owned(),
                    timeout,
                    file: file.to_path_buf(),
                    origin,
                    claude,
                    plugin: plugin.map(Path::to_path_buf),
                });
            }
        }
    }
    loaded
}

#[cfg(test)]
/// Tests for reading hook definitions.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Claude Code's format is read: events, matchers, commands and timeouts, with other events and hook types skipped and bad entries reported.
    #[test]
    fn reads_the_claude_code_format() {
        let settings: Value = serde_json::from_str(
            r#"{
                "permissions": {"allow": ["Bash(ls:*)"]},
                "hooks": {
                    "PreToolUse": [
                        {"matcher": "Bash", "hooks": [
                            {"type": "command", "command": "./guard.sh", "timeout": 5},
                            {"type": "prompt", "prompt": "is this safe?"}
                        ]},
                        {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "  fmt  "}]}
                    ],
                    "PostToolUse": [{"hooks": [{"type": "command", "command": "echo done"}]}],
                    "Notification": [{"hooks": [{"type": "command", "command": "notify-send hi"}]}],
                    "Stop": [{"matcher": "", "hooks": [{"type": "command"}]}],
                    "UserPromptSubmit": "nonsense"
                }
            }"#,
        )
        .unwrap();
        let file = Path::new("/p/.claude/settings.json");
        let loaded = parse(&settings, file, HookOrigin::Project, true, None);
        let mut found: Vec<(HookEvent, &str, &str, Option<Duration>)> = loaded
            .hooks
            .iter()
            .map(|hook| {
                (
                    hook.event,
                    hook.matcher.as_str(),
                    hook.command.as_str(),
                    hook.timeout,
                )
            })
            .collect();
        found.sort();
        assert_eq!(
            found,
            vec![
                (
                    HookEvent::PreToolUse,
                    "Bash",
                    "./guard.sh",
                    Some(Duration::from_secs(5))
                ),
                (HookEvent::PreToolUse, "Edit|Write", "fmt", None),
                (HookEvent::PostToolUse, "", "echo done", None),
            ]
        );
        assert!(
            loaded
                .hooks
                .iter()
                .all(|hook| hook.claude && hook.file == file)
        );
        assert_eq!(loaded.problems.len(), 2, "{:?}", loaded.problems);
        let by = |command: &str| {
            loaded
                .hooks
                .iter()
                .find(|hook| hook.command == command)
                .unwrap()
        };
        assert_eq!(by("./guard.sh").label(), "PreToolUse Bash: ./guard.sh");
        assert_eq!(by("./guard.sh").limit(), Duration::from_secs(5));
        assert_eq!(by("echo done").label(), "PostToolUse: echo done");
        assert_eq!(by("echo done").limit(), Duration::from_secs(60));
    }

    /// The user's file, Claude Code's user settings, an installed plugin's hooks and the project's files are all read, in that order, and the project's only while its folders are read.
    #[test]
    fn loads_every_place_and_gates_the_project() {
        let base = scratch("config-places");
        let hook = |command: &str| {
            format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{command}"}}]}}]}}}}"#
            )
        };
        put(&base, "config/hooks.json", &hook("own"));
        put(&base, "home/.claude/settings.json", &hook("claude"));
        put(
            &base,
            "home/.claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"kit@m":[{{"scope":"user","installPath":{}}}]}}}}"#,
                serde_json::to_string(&base.join("kit")).unwrap()
            ),
        );
        put(&base, "kit/hooks/hooks.json", &hook("plugin"));
        put(&base, "repo/.claude/settings.json", &hook("shared"));
        put(&base, "repo/.claude/settings.local.json", &hook("local"));
        put(&base, "repo/.oh-my-crab/hooks.json", &hook("crab"));
        put(&base, "repo/.claude/settings.json.bak", "not read");
        let mut locations = Locations {
            directory: base.join("repo"),
            root: None,
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: None,
            project: true,
        };
        let own = base.join("config/hooks.json");
        let commands = |locations: &Locations| -> Vec<(String, HookOrigin)> {
            load(locations, Some(&own))
                .hooks
                .into_iter()
                .map(|hook| (hook.command, hook.origin))
                .collect()
        };
        assert_eq!(
            commands(&locations),
            vec![
                ("own".to_owned(), HookOrigin::User),
                ("claude".to_owned(), HookOrigin::User),
                ("plugin".to_owned(), HookOrigin::Plugin),
                ("crab".to_owned(), HookOrigin::Project),
                ("shared".to_owned(), HookOrigin::Project),
                ("local".to_owned(), HookOrigin::Project),
            ]
        );
        let plugin = load(&locations, Some(&own)).hooks[2].clone();
        assert_eq!(plugin.plugin, Some(base.join("kit")));
        locations.project = false;
        assert_eq!(commands(&locations).len(), 3);
    }

    /// `disableAllHooks` in a file that is read turns every hook off; a file that is not JSON is reported and skipped.
    #[test]
    fn hooks_can_be_switched_off() {
        let base = scratch("config-disabled");
        put(
            &base,
            "home/.claude/settings.json",
            r#"{"disableAllHooks":true}"#,
        );
        put(
            &base,
            "own.json",
            r#"{"hooks":{"Stop":[{"hooks":[{"type":"command","command":"x"}]}]}}"#,
        );
        put(&base, "broken.json", "{");
        let locations = Locations {
            directory: base.join("repo"),
            root: None,
            home: None,
            claude: Some(base.join("home/.claude")),
            codex: None,
            project: false,
        };
        assert!(
            load(&locations, Some(&base.join("own.json")))
                .hooks
                .is_empty()
        );
        let broken = load(&locations, Some(&base.join("broken.json")));
        assert_eq!(broken.problems.len(), 1);
    }
}
