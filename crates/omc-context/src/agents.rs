//! Agent definitions: the kinds of subagent the `task` tool can start.

use std::path::PathBuf;

use crate::frontmatter::parse;
use crate::place::{AGENTS, Shape, files_under};
use crate::plugins::installed;
use crate::{Locations, Origin, toml};

/// The tools that only look and never change anything.
pub const READ_ONLY_TOOLS: &[&str] = &["read", "grep", "glob", "skill", "ask", "todo"];

/// One kind of subagent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentDefinition {
    /// What the kind is called, which is how the `task` tool names it.
    pub name: String,
    /// What the kind is for.
    pub description: String,
    /// The tools it may use, by name, or nothing for all that the main agent has.
    pub tools: Option<Vec<String>>,
    /// The instructions it is given as its role.
    pub prompt: String,
    /// Where the definition is, or nothing for a built-in one.
    pub path: Option<PathBuf>,
    /// Where the definition came from.
    pub origin: Origin,
}

impl AgentDefinition {
    /// Whether the definition lists tools and all of them only look.
    pub fn read_only(&self) -> bool {
        self.tools.as_ref().is_some_and(|tools| {
            !tools.is_empty()
                && tools
                    .iter()
                    .all(|tool| READ_ONLY_TOOLS.contains(&tool.as_str()))
        })
    }
}

/// The agent kinds that come with the harness.
pub fn builtin_agents() -> Vec<AgentDefinition> {
    [
        include_str!("prompts/agent_task.md"),
        include_str!("prompts/agent_scout.md"),
        include_str!("prompts/agent_reviewer.md"),
    ]
    .into_iter()
    .filter_map(|text| from_markdown(text, "builtin", None, Origin::BuiltIn))
    .collect()
}

/// The agent kinds available for work in `locations`: the project's, then the reader's, then those Claude Code plugins bring, then the built-in ones; a name found twice keeps its first, and names in `disabled` are skipped.
pub(crate) fn find(locations: &Locations, disabled: &[String]) -> Vec<AgentDefinition> {
    let mut found: Vec<AgentDefinition> = Vec::new();
    for place in AGENTS {
        for folder in locations.folders(place.root) {
            let folder = folder.join(place.path);
            let suffix = match place.shape {
                Shape::Toml => ".toml",
                _ => ".md",
            };
            for path in files_under(&folder, suffix) {
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let stem = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or_default()
                    .trim_end_matches(".agent")
                    .to_owned();
                let agent = if suffix == ".toml" {
                    from_toml(&text, &stem, &path, place.root.into())
                } else {
                    from_markdown(&text, &stem, Some(path), place.root.into())
                };
                keep(&mut found, agent, disabled);
            }
        }
    }
    for plugin in installed(locations) {
        for path in files_under(&plugin.folder.join("agents"), ".md") {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default()
                .to_owned();
            let agent = from_markdown(&text, &stem, Some(path), Origin::Plugin);
            keep(&mut found, agent, disabled);
        }
    }
    for agent in builtin_agents() {
        keep(&mut found, Some(agent), disabled);
    }
    found
}

/// Adds `agent` to `found` unless it is missing, disabled or its name is taken.
fn keep(found: &mut Vec<AgentDefinition>, agent: Option<AgentDefinition>, disabled: &[String]) {
    if let Some(agent) = agent
        && !disabled.contains(&agent.name)
        && !found.iter().any(|held| held.name == agent.name)
    {
        found.push(agent);
    }
}

/// The definition `text` holds as markdown with front matter, named `stem` unless it names itself, coming from `origin`.
fn from_markdown(
    text: &str,
    stem: &str,
    path: Option<PathBuf>,
    origin: Origin,
) -> Option<AgentDefinition> {
    let parsed = parse(text);
    let name = parsed
        .get("name")
        .filter(|name| !name.is_empty())
        .unwrap_or(stem)
        .to_owned();
    let tools = parsed
        .list("tools")
        .map(|tools| tools.iter().map(|tool| tool.to_lowercase()).collect());
    let prompt = parsed.body.trim().to_owned();
    (!prompt.is_empty() || parsed.get("description").is_some()).then(|| AgentDefinition {
        name,
        description: parsed.get("description").unwrap_or_default().to_owned(),
        tools,
        prompt,
        path,
        origin,
    })
}

/// The definition `text` holds as TOML, named `stem` unless it names itself, coming from `origin`.
fn from_toml(
    text: &str,
    stem: &str,
    path: &std::path::Path,
    origin: Origin,
) -> Option<AgentDefinition> {
    let fields = toml::strings(text);
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let prompt = get("developer_instructions").or_else(|| get("instructions"))?;
    Some(AgentDefinition {
        name: get("name")
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| stem.to_owned()),
        description: get("description").unwrap_or_default(),
        tools: None,
        prompt,
        path: Some(path.to_path_buf()),
        origin,
    })
}

#[cfg(test)]
/// Tests for agent definitions.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// The built-in kinds exist and only the scout is read-only among the first two.
    #[test]
    fn builtins() {
        let agents = builtin_agents();
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["task", "scout", "reviewer"]);
        assert!(!agents[0].read_only());
        assert!(agents[1].read_only());
        assert!(!agents[2].read_only());
    }

    /// Markdown and TOML definitions are found, and a project one shadows a built-in.
    #[test]
    fn finds_definitions() {
        let base = scratch("agents-find");
        put(
            &base,
            "repo/.claude/agents/scout.md",
            "---\nname: scout\ndescription: mine\ntools: Read, Grep\n---\nLook.",
        );
        put(
            &base,
            "repo/.codex/agents/tester.toml",
            "name = \"tester\"\ndescription = \"runs tests\"\ndeveloper_instructions = \"\"\"\nRun them.\n\"\"\"\n",
        );
        put(
            &base,
            "home/.claude/agents/doc.md",
            "---\ndescription: docs\n---\nWrite docs.",
        );
        let locations = Locations {
            directory: base.join("repo"),
            root: Some(base.join("repo")),
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: Some(base.join("home/.codex")),
        };
        put(
            &base,
            "home/.claude/plugins/installed_plugins.json",
            &format!(
                r#"{{"plugins":{{"kit@market":[{{"installPath":"{}/kit"}}]}}}}"#,
                base.display()
            ),
        );
        put(
            &base,
            "kit/agents/auditor.md",
            "---\ndescription: audits\n---\nAudit.",
        );
        let found = find(&locations, &["reviewer".to_owned()]);
        let names: Vec<&str> = found.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["scout", "tester", "doc", "auditor", "task"]);
        assert_eq!(found[0].description, "mine");
        assert_eq!(found[0].tools, Some(vec!["read".into(), "grep".into()]));
        assert_eq!(found[1].prompt, "Run them.");
        assert_eq!(found[2].origin, Origin::User);
        assert_eq!(found[3].origin, Origin::Plugin);
        assert_eq!(found[4].origin, Origin::BuiltIn);
    }
}
