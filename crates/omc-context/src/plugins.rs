//! Claude Code plugins: installed packages that bring skills and agent definitions.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::Locations;

/// One install of a plugin for Claude Code that applies to the working folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Plugin {
    /// What the plugin is called, without its marketplace; what its skills are named after when their own names are taken.
    pub(crate) name: String,
    /// The folder it is installed in.
    pub(crate) folder: PathBuf,
}

impl Plugin {
    /// The folders its skills are in, each either a skill itself or a folder of skills.
    ///
    /// Those its manifest, `.claude-plugin/plugin.json`, names under `skills`, then those the entry for it in a marketplace kept in its own folder names, each once and only inside the plugin. `skills` comes first, unless such a marketplace entry exists and some folder is named; when none is, `skills` alone.
    pub(crate) fn skill_folders(&self) -> Vec<PathBuf> {
        let fallback = self.folder.join("skills");
        let catalogued = self.catalogued_skills();
        let named: Vec<String> = json(&self.folder.join(".claude-plugin/plugin.json"))
            .map(|manifest| strings(&manifest["skills"]))
            .unwrap_or_default()
            .into_iter()
            .chain(catalogued.clone().unwrap_or_default())
            .collect();
        if named.is_empty() {
            return vec![fallback];
        }
        let mut folders = if catalogued.is_none() {
            vec![fallback]
        } else {
            Vec::new()
        };
        for entry in named {
            let folder = normalized(&self.folder.join(entry));
            if folder.starts_with(&self.folder) && !folders.contains(&folder) {
                folders.push(folder);
            }
        }
        folders
    }

    /// The skill folders named by the entry for this plugin in a marketplace kept in its own folder, `marketplace.json` or `.claude-plugin/marketplace.json`, that installs it from that folder; nothing when there is no such entry.
    fn catalogued_skills(&self) -> Option<Vec<String>> {
        ["marketplace.json", ".claude-plugin/marketplace.json"]
            .into_iter()
            .filter_map(|file| json(&self.folder.join(file)))
            .find_map(|catalog| {
                let entry = catalog["plugins"].as_array()?.iter().find(|entry| {
                    entry["name"].as_str() == Some(self.name.as_str())
                        && entry["source"].as_str() == Some("./")
                })?;
                Some(strings(&entry["skills"]))
            })
    }
}

/// The plugin installs in Claude's config folder of `locations` that apply to its working folder, those bound to a project first.
///
/// An install is left out when its record or the settings switch it off. One bound to a project applies only when that project is the working folder, unless the settings switch the plugin on.
pub(crate) fn installed(locations: &Locations) -> Vec<Plugin> {
    let Some(claude) = &locations.claude else {
        return Vec::new();
    };
    let Some(record) = json(&claude.join("plugins/installed_plugins.json")) else {
        return Vec::new();
    };
    let Some(plugins) = record["plugins"].as_object() else {
        return Vec::new();
    };
    let switched = switched(claude, &locations.directory);
    let here = locations.directory.canonicalize().ok();
    let mut bound = Vec::new();
    let mut shared = Vec::new();
    for (id, installs) in plugins {
        let Some((name, _)) = id.rsplit_once('@') else {
            continue;
        };
        let switch = switched.get(id).copied();
        if switch == Some(false) {
            continue;
        }
        for install in installs.as_array().into_iter().flatten() {
            let Some(folder) = install["installPath"].as_str() else {
                continue;
            };
            if install["enabled"] == Value::Bool(false) {
                continue;
            }
            let project_bound = matches!(install["scope"].as_str(), Some("local" | "project"));
            if project_bound && switch != Some(true) && !installed_here(install, here.as_deref()) {
                continue;
            }
            let plugin = Plugin {
                name: name.to_owned(),
                folder: PathBuf::from(folder),
            };
            if project_bound {
                bound.push(plugin);
            } else {
                shared.push(plugin);
            }
        }
    }
    bound.extend(shared);
    bound
}

/// Whether `install`, bound to a project, was installed for `here`, the working folder.
fn installed_here(install: &Value, here: Option<&Path>) -> bool {
    let project = install["projectPath"]
        .as_str()
        .and_then(|path| Path::new(path).canonicalize().ok());
    here.is_some() && project.as_deref() == here
}

/// The plugins switched on or off by `enabledPlugins` in Claude's `settings.json` under `claude`, then the working folder's `.claude/settings.json` and `.claude/settings.local.json`, a later file winning.
fn switched(claude: &Path, directory: &Path) -> HashMap<String, bool> {
    let files = [
        claude.join("settings.json"),
        directory.join(".claude/settings.json"),
        directory.join(".claude/settings.local.json"),
    ];
    let mut switched = HashMap::new();
    for settings in files.iter().filter_map(|file| json(file)) {
        let Some(enabled) = settings["enabledPlugins"].as_object() else {
            continue;
        };
        for (id, on) in enabled {
            if let Some(on) = on.as_bool() {
                switched.insert(id.clone(), on);
            }
        }
    }
    switched
}

/// The non-empty strings `value` holds: itself when it is one, its entries when it is a list.
fn strings(value: &Value) -> Vec<String> {
    let values = match value {
        Value::Array(entries) => entries.iter().collect(),
        other => vec![other],
    };
    values
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// `path` with its `.` and `..` parts worked out, without the disk being asked.
fn normalized(path: &Path) -> PathBuf {
    let mut parts = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    parts
}

/// The JSON in the file at `path`, when it can be read and parsed.
fn json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

#[cfg(test)]
/// Tests for finding installed plugins.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// Locations for work in `base/repo`, with Claude's config folder at `base/home/.claude`.
    fn locations(base: &Path) -> Locations {
        Locations {
            directory: base.join("repo"),
            root: None,
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: None,
        }
    }

    /// Writes the plugin record under `base` with `plugins` as its `plugins` object, `{base}` standing for `base`.
    fn record(base: &Path, plugins: &str) {
        put(
            base,
            "home/.claude/plugins/installed_plugins.json",
            &format!(r#"{{"version":2,"plugins":{plugins}}}"#)
                .replace("{base}", &base.display().to_string()),
        );
    }

    /// The names and folders, under `base`, of what `installed` finds.
    fn found(base: &Path) -> Vec<(String, String)> {
        installed(&locations(base))
            .into_iter()
            .map(|plugin| {
                let folder = plugin.folder.strip_prefix(base).unwrap();
                (plugin.name, folder.display().to_string())
            })
            .collect()
    }

    /// A plugin switched off in the user's settings, or in its own record, is left out, and the project's local settings win over the user's.
    #[test]
    fn honours_every_switch() {
        let base = scratch("plugins-switches");
        std::fs::create_dir_all(base.join("repo")).unwrap();
        record(
            &base,
            r#"{
                "tools@market":[{"scope":"user","installPath":"{base}/tools"}],
                "off@market":[{"scope":"user","installPath":"{base}/off"}],
                "dead@market":[{"scope":"user","installPath":"{base}/dead","enabled":false}],
                "back@market":[{"scope":"user","installPath":"{base}/back"}]
            }"#,
        );
        put(
            &base,
            "home/.claude/settings.json",
            r#"{"enabledPlugins":{"off@market":false,"back@market":false}}"#,
        );
        put(
            &base,
            "repo/.claude/settings.local.json",
            r#"{"enabledPlugins":{"back@market":true}}"#,
        );
        assert_eq!(
            found(&base),
            vec![
                ("back".to_owned(), "back".to_owned()),
                ("tools".to_owned(), "tools".to_owned()),
            ]
        );
    }

    /// An install bound to a project applies only in that project, unless the settings switch it on, and comes before the user's install of the same plugin.
    #[test]
    fn project_installs_apply_to_their_project() {
        let base = scratch("plugins-projects");
        std::fs::create_dir_all(base.join("repo")).unwrap();
        std::fs::create_dir_all(base.join("elsewhere")).unwrap();
        record(
            &base,
            r#"{
                "kit@market":[
                    {"scope":"user","installPath":"{base}/kit-user"},
                    {"scope":"project","projectPath":"{base}/repo","installPath":"{base}/kit-here"}
                ],
                "other@market":[{"scope":"local","projectPath":"{base}/elsewhere","installPath":"{base}/other"}],
                "forced@market":[{"scope":"project","projectPath":"{base}/elsewhere","installPath":"{base}/forced"}]
            }"#,
        );
        put(
            &base,
            "repo/.claude/settings.json",
            r#"{"enabledPlugins":{"forced@market":true}}"#,
        );
        assert_eq!(
            found(&base),
            vec![
                ("forced".to_owned(), "forced".to_owned()),
                ("kit".to_owned(), "kit-here".to_owned()),
                ("kit".to_owned(), "kit-user".to_owned()),
            ]
        );
    }

    /// The manifest can name more skill folders, one or a list, and those outside the plugin are ignored.
    #[test]
    fn manifest_names_skill_folders() {
        let base = scratch("plugins-manifest");
        let plugin = Plugin {
            name: "kit".to_owned(),
            folder: base.join("kit"),
        };
        put(
            &base,
            "kit/.claude-plugin/plugin.json",
            r#"{"skills":["./extra", "skills", "../outside", " "]}"#,
        );
        assert_eq!(
            plugin.skill_folders(),
            vec![base.join("kit/skills"), base.join("kit/extra")]
        );
    }

    /// A plugin that is its own marketplace takes only the folders named, without `skills`, and `skills` alone when none is.
    #[test]
    fn own_marketplace_names_the_only_folders() {
        let base = scratch("plugins-marketplace");
        let plugin = Plugin {
            name: "kit".to_owned(),
            folder: base.join("kit"),
        };
        put(
            &base,
            "kit/.claude-plugin/marketplace.json",
            r#"{"plugins":[{"name":"kit","source":"./"}]}"#,
        );
        assert_eq!(plugin.skill_folders(), vec![base.join("kit/skills")]);
        put(
            &base,
            "kit/.claude-plugin/plugin.json",
            r#"{"skills":["./skills/commit","./skills/pr"]}"#,
        );
        assert_eq!(
            plugin.skill_folders(),
            vec![base.join("kit/skills/commit"), base.join("kit/skills/pr")]
        );
    }
}
