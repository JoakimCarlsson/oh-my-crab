//! What the agent is told in a message, rather than in the system prompt, so that the system prompt stays the same for the whole conversation and the provider's prompt cache keeps it: the date and branch, plan mode turning on and off, what changed on disk, and what earlier conversations remembered.

use crate::Discovered;
use crate::Environment;
use crate::system::{inventory, plan_rules};
use crate::template::render;

/// The date and branch the agent is told.
const ENVIRONMENT: &str = include_str!("prompts/environment.md");

/// What the agent is told when plan mode turns on.
const PLAN_ON: &str = include_str!("prompts/plan_mode_on.md");

/// What the agent is told when plan mode turns off.
const PLAN_OFF: &str = include_str!("prompts/plan_mode_off.md");

/// What the agent is told when what was found on disk changed.
const CHANGES: &str = include_str!("prompts/context_changes.md");

/// The memories of earlier conversations the agent is told.
const MEMORY: &str = include_str!("prompts/memory.md");

/// The kinds of notice, each a block of its own tag that a later one of the same kind replaces.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Notice {
    /// The date and the branch.
    Environment,
    /// Plan mode turning on or off.
    PlanMode,
    /// Skills, agents and instruction files that changed on disk.
    ContextChanges,
    /// What earlier conversations remembered about the project and the user.
    Memory,
}

impl Notice {
    /// The tag the notice is written in.
    pub fn tag(self) -> &'static str {
        match self {
            Notice::Environment => "environment",
            Notice::PlanMode => "plan-mode",
            Notice::ContextChanges => "context-changes",
            Notice::Memory => "memory",
        }
    }

    /// The last notice of this kind in `text`, from its opening tag to its closing one; nothing when `text` holds none.
    pub fn find_last(self, text: &str) -> Option<&str> {
        let open = format!("<{}>", self.tag());
        let close = format!("</{}>", self.tag());
        let start = text.rfind(&open)?;
        let length = text[start..].find(&close)? + close.len();
        Some(&text[start..start + length])
    }
}

/// The notice that tells the agent today's date and the branch of `environment`.
pub fn environment_reminder(environment: &Environment) -> String {
    render(
        ENVIRONMENT,
        &[
            ("date", environment.date.as_str()),
            ("branch", environment.branch.as_deref().unwrap_or_default()),
        ],
    )
}

/// The notice that hands the agent `memories`, the digest of what earlier conversations saved with one memory to a line, or says that nothing is saved yet when it is empty; it points at `memory_edit` only when `tools` names it.
pub fn memory_notice(memories: &str, tools: &[String]) -> String {
    let edit = if tools.iter().any(|tool| tool == "memory_edit") {
        "1"
    } else {
        ""
    };
    render(MEMORY, &[("memories", memories), ("has_memory_edit", edit)])
}

/// The notice that plan mode is now `on`, giving the rules and `tools`, the tools it allows, when it is, and saying every tool may be used again when it is not.
pub fn plan_mode_notice(on: bool, tools: &[String]) -> String {
    if !on {
        return render(PLAN_OFF, &[]);
    }
    let listed = inventory(tools);
    let rules = plan_rules(tools);
    render(
        PLAN_ON,
        &[("tools", listed.as_str()), ("plan", rules.as_str())],
    )
}

/// The notice of what changed on disk between `before`, what the system prompt describes, and `after`, for an agent offered the tools named `tools`: skills and agent kinds added, changed or removed, and instruction files changed or removed with what they now say. A new skill is said to load with `skill`, and a new agent kind to run with `task`, only when `tools` names that tool. When nothing differs, it says that the system prompt holds again.
pub fn context_changes(before: &Discovered, after: &Discovered, tools: &[String]) -> String {
    let offered = |name: &str| tools.iter().any(|tool| tool == name);
    let mut lines = Vec::new();
    for skill in after.skills.iter().filter(|skill| !skill.hidden) {
        let known = before.skills.iter().find(|old| old.name == skill.name);
        if known.is_none_or(|old| old.description != skill.description || old.hidden) {
            let how = match offered("skill") {
                true => format!("with `skill` or by reading `{}`", skill.path.display()),
                false => format!("by reading `{}`", skill.path.display()),
            };
            lines.push(format!(
                "- Skill `{}` is now available: {} Load it {how}.",
                skill.name, skill.description
            ));
        }
    }
    for skill in before.skills.iter().filter(|skill| !skill.hidden) {
        if !after.skills.iter().any(|new| new.name == skill.name) {
            lines.push(format!("- Skill `{}` is no longer available.", skill.name));
        }
    }
    for agent in &after.agents {
        let known = before.agents.iter().find(|old| old.name == agent.name);
        if known.is_none_or(|old| old.description != agent.description) {
            let to = match offered("task") {
                true => " to `task`",
                false => "",
            };
            lines.push(format!(
                "- Subagent kind `{}` is now available{to}: {}",
                agent.name, agent.description
            ));
        }
    }
    for agent in &before.agents {
        if !after.agents.iter().any(|new| new.name == agent.name) {
            lines.push(format!(
                "- Subagent kind `{}` is no longer available.",
                agent.name
            ));
        }
    }
    for file in &after.instructions {
        let known = before.instructions.iter().find(|old| old.path == file.path);
        if known.is_none_or(|old| old.content != file.content) {
            lines.push(format!(
                "- Instruction file `{0}` now reads:\n<file path=\"{0}\">\n{1}\n</file>",
                file.path.display(),
                file.content.trim_end()
            ));
        }
    }
    for file in &before.instructions {
        if !after.instructions.iter().any(|new| new.path == file.path) {
            lines.push(format!(
                "- Instruction file `{}` was removed; stop following it.",
                file.path.display()
            ));
        }
    }
    let changes = lines.join("\n");
    render(CHANGES, &[("changes", changes.as_str())])
}

#[cfg(test)]
/// Tests for the notices.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};
    use crate::{Locations, discover};

    /// The environment of a folder with no repository, on `date`.
    fn on(date: &str) -> Environment {
        Environment {
            directory: "/w".into(),
            os: "linux".into(),
            date: date.into(),
            branch: None,
            model: None,
        }
    }

    /// The reminder names the date, and the branch only when there is one.
    #[test]
    fn environment_names_date_and_branch() {
        let mut environment = on("2026-11-06");
        assert_eq!(
            environment_reminder(&environment),
            "<environment>\nToday's date: 2026-11-06.\n</environment>"
        );
        environment.branch = Some("main".into());
        assert!(environment_reminder(&environment).contains("Git branch: main."));
    }

    /// The last block of a kind is found whole, and a missing one is not made up.
    #[test]
    fn finds_the_last_notice() {
        let text = format!(
            "{}\n\nhello {}",
            environment_reminder(&on("2026-11-05")),
            environment_reminder(&on("2026-11-06"))
        );
        assert_eq!(
            Notice::Environment.find_last(&text),
            Some(environment_reminder(&on("2026-11-06")).as_str())
        );
        assert_eq!(Notice::PlanMode.find_last(&text), None);
        assert_eq!(Notice::PlanMode.find_last("<plan-mode> cut"), None);
    }

    /// The memory notice lists the memories it is given, points at `memory_edit` only when it is offered, says so when there are none, and is found again as a notice of its kind.
    #[test]
    fn memory_notices() {
        let tools: Vec<String> = vec!["memory_edit".into()];
        let listed = memory_notice("- [a1] Use tabs.", &tools);
        assert!(listed.starts_with("<memory>") && listed.ends_with("</memory>"));
        assert!(listed.contains("- [a1] Use tabs.") && listed.contains("`memory_edit`"));
        assert!(!memory_notice("- [a1] Use tabs.", &[]).contains("memory_edit"));
        assert!(listed.contains("`memory_edit`.\n- [a1] Use tabs.\n</memory>"));
        let empty = memory_notice("", &tools);
        assert_eq!(
            empty,
            "<memory>\nNo memories are saved for this project yet.\n</memory>"
        );
        let text = format!("{empty}\n\nhello");
        assert_eq!(Notice::Memory.find_last(&text), Some(empty.as_str()));
    }

    /// Turning plan mode on lists the tools it allows and its rules; turning it off says so.
    #[test]
    fn plan_notices() {
        let tools: Vec<String> = vec!["read".into(), "exit_plan_mode".into()];
        let on = plan_mode_notice(true, &tools);
        assert!(on.starts_with("<plan-mode>") && on.ends_with("</plan-mode>"));
        assert!(on.contains("- `read`\n- `exit_plan_mode`"));
        assert!(on.contains("Plan mode active"));
        let off = plan_mode_notice(false, &tools);
        assert!(off.contains("Plan mode is now off"));
        assert_eq!(Notice::PlanMode.find_last(&off), Some(off.as_str()));
    }

    /// Added, changed and removed skills, agents and instruction files are each told, and no change says the system prompt holds.
    #[test]
    fn changes_on_disk_are_told() {
        let base = scratch("notices-changes");
        put(&base, "AGENTS.md", "Old rule.");
        put(
            &base,
            ".claude/skills/pdf/SKILL.md",
            "---\ndescription: Reads PDFs\n---\nbody",
        );
        let locations = Locations {
            directory: base.clone(),
            root: Some(base.clone()),
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let before = discover(&locations, &[]);
        let tools: Vec<String> = vec!["skill".into(), "task".into()];
        let same = context_changes(&before, &before, &tools);
        assert!(same.contains("again as the system prompt describes it"));
        put(&base, "AGENTS.md", "New rule.");
        std::fs::remove_dir_all(base.join(".claude/skills/pdf")).unwrap();
        put(
            &base,
            ".claude/skills/csv/SKILL.md",
            "---\ndescription: Reads CSV\n---\nbody",
        );
        put(
            &base,
            ".claude/agents/critic.md",
            "---\ndescription: Finds flaws\n---\nBe critical.",
        );
        let after = discover(&locations, &[]);
        let told = context_changes(&before, &after, &tools);
        assert!(told.starts_with("<context-changes>"));
        assert!(told.contains("Skill `csv` is now available: Reads CSV"));
        assert!(told.contains("Skill `pdf` is no longer available."));
        assert!(told.contains("Subagent kind `critic` is now available to `task`: Finds flaws"));
        assert!(told.contains("New rule."));
        assert!(!told.contains("Old rule."));
        assert!(told.contains("Load it with `skill` or by reading"));
    }

    /// A new skill or agent kind is never said to be used through a tool the agent is not offered.
    #[test]
    fn changes_point_only_at_offered_tools() {
        let base = scratch("notices-unoffered");
        let locations = Locations {
            directory: base.clone(),
            root: Some(base.clone()),
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let before = discover(&locations, &[]);
        put(
            &base,
            ".claude/skills/csv/SKILL.md",
            "---\ndescription: Reads CSV\n---\nbody",
        );
        put(
            &base,
            ".claude/agents/critic.md",
            "---\ndescription: Finds flaws\n---\nBe critical.",
        );
        let after = discover(&locations, &[]);
        let told = context_changes(&before, &after, &[]);
        assert!(told.contains("Skill `csv` is now available: Reads CSV Load it by reading `"));
        assert!(!told.contains("`skill`"));
        assert!(told.contains("Subagent kind `critic` is now available: Finds flaws"));
        assert!(!told.contains("`task`"));
    }
}
