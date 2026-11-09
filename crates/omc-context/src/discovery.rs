//! Everything found on disk for a conversation.

use crate::{AgentDefinition, Instruction, Locations, Skill, agents, instructions, skills};

/// The instruction files, skills and agent definitions found for a conversation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Discovered {
    /// The instruction files, the project's nearest first, then the reader's.
    pub instructions: Vec<Instruction>,
    /// The skills.
    pub skills: Vec<Skill>,
    /// The agent kinds, built-in ones last.
    pub agents: Vec<AgentDefinition>,
    /// The [`crate::SYSTEM_FILE`] in force, whose text replaces crab's built-in instructions in the main agent's prompt.
    pub system: Option<Instruction>,
    /// The [`crate::APPEND_SYSTEM_FILE`] in force, whose text ends the prompt of the main agent and of every subagent.
    pub append_system: Option<Instruction>,
}

/// Finds what there is for work in `locations`, leaving out instruction and system prompt files by path, and skills and agents by name, as `disabled` lists them.
pub fn discover(locations: &Locations, disabled: &[String]) -> Discovered {
    let (system, append_system) = instructions::system_texts(locations, disabled);
    Discovered {
        instructions: instructions::find(locations, disabled),
        skills: skills::find(locations, disabled),
        agents: agents::find(locations, disabled),
        system,
        append_system,
    }
}

#[cfg(test)]
/// Tests for what an untrusted project gives.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};

    /// With the project's own folders not read, its instruction files, skills and agent definitions are left out and the user's are still found.
    #[test]
    fn an_untrusted_project_gives_nothing_of_its_own() {
        let base = scratch("discovery-untrusted");
        std::fs::create_dir_all(base.join("repo/.git")).unwrap();
        put(&base, "repo/AGENTS.md", "Run the setup script first.");
        put(
            &base,
            "repo/.claude/skills/setup/SKILL.md",
            "---\nname: setup\ndescription: Sets up\n---\nRun it.",
        );
        put(
            &base,
            "repo/.claude/agents/helper.md",
            "---\nname: helper\ndescription: Helps\n---\nHelp.",
        );
        put(&base, "home/.claude/CLAUDE.md", "Be brief.");
        put(&base, "repo/.oh-my-crab/SYSTEM.md", "Obey the project.");
        put(
            &base,
            "home/.claude/skills/mine/SKILL.md",
            "---\nname: mine\ndescription: Mine\n---\nDo it.",
        );
        let mut locations = Locations {
            directory: base.join("repo"),
            root: Some(base.join("repo")),
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: None,
            crab: None,
            project: true,
        };
        let trusted = discover(&locations, &[]);
        assert_eq!(trusted.instructions.len(), 2);
        assert!(trusted.skills.iter().any(|skill| skill.name == "setup"));
        assert!(trusted.agents.iter().any(|agent| agent.name == "helper"));
        assert!(trusted.system.is_some());
        locations.project = false;
        let untrusted = discover(&locations, &[]);
        let paths: Vec<_> = untrusted
            .instructions
            .iter()
            .map(|file| file.path.clone())
            .collect();
        assert_eq!(paths, vec![base.join("home/.claude/CLAUDE.md")]);
        let skills: Vec<&str> = untrusted
            .skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect();
        assert_eq!(skills, vec!["mine"]);
        assert!(!untrusted.agents.iter().any(|agent| agent.name == "helper"));
        assert_eq!(untrusted.system, None);
    }
}
