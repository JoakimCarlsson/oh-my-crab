//! The system prompt, built from the templates in `prompts/`.

use crate::template::render;
use crate::{Discovered, Environment};

/// The main agent's prompt.
const SYSTEM: &str = include_str!("prompts/system.md");

/// The prompt of a subagent.
const SUBAGENT: &str = include_str!("prompts/subagent.md");

/// The project and workstation part of both prompts.
const PROJECT: &str = include_str!("prompts/project.md");

/// What the prompt of plan mode adds.
const PLAN: &str = include_str!("prompts/plan.md");

/// What a system prompt is built from.
#[derive(Clone, Copy, Debug)]
pub struct PromptInput<'a> {
    /// The workstation.
    pub environment: &'a Environment,
    /// The instruction files and skills found.
    pub discovered: &'a Discovered,
    /// The names of the tools the agent has.
    pub tools: &'a [String],
    /// Whether plan mode is on.
    pub plan: bool,
}

/// The system prompt of the main agent for `input`.
pub fn build_prompt(input: PromptInput<'_>) -> String {
    let flags = flags(&input);
    let project = project(&input);
    let skills = skills(input.discovered);
    let plan = if input.plan {
        render(PLAN, &flags_ref(&flags))
    } else {
        String::new()
    };
    let tools = inventory(input.tools);
    let mut vars = flags_ref(&flags);
    vars.extend([
        ("skills", skills.as_str()),
        ("project", project.as_str()),
        ("plan", plan.as_str()),
        ("tools", tools.as_str()),
    ]);
    render(SYSTEM, &vars)
}

/// The system prompt of a subagent whose role is `role`, for `input`.
pub fn build_subagent_prompt(input: PromptInput<'_>, role: &str) -> String {
    let flags = flags(&input);
    let project = project(&input);
    let plan = if input.plan {
        render(PLAN, &flags_ref(&flags))
    } else {
        String::new()
    };
    let tools = inventory(input.tools);
    let mut vars = flags_ref(&flags);
    vars.extend([
        ("agent", role),
        ("project", project.as_str()),
        ("plan", plan.as_str()),
        ("tools", tools.as_str()),
    ]);
    render(SUBAGENT, &vars)
}

/// One `has_<tool>` flag for each tool the prompt knows how to talk about, set when `input` has the tool.
fn flags(input: &PromptInput<'_>) -> Vec<(String, String)> {
    [
        "read", "grep", "glob", "bash", "edit", "write", "todo", "ask", "task", "skill",
    ]
    .iter()
    .map(|tool| {
        let has = input.tools.iter().any(|name| name == tool);
        (format!("has_{tool}"), if has { "1" } else { "" }.to_owned())
    })
    .collect()
}

/// `flags` as the pairs the renderer takes.
fn flags_ref(flags: &[(String, String)]) -> Vec<(&str, &str)> {
    flags
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// The list of tools, one to a line.
fn inventory(tools: &[String]) -> String {
    tools
        .iter()
        .map(|tool| format!("- `{tool}`"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The skills the agent may load, one to a line.
fn skills(discovered: &Discovered) -> String {
    discovered
        .skills
        .iter()
        .filter(|skill| !skill.hidden)
        .map(|skill| format!("- {}: {}", skill.name, skill.description))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The workstation and instruction files, as the project part of a prompt.
fn project(input: &PromptInput<'_>) -> String {
    let environment = input.environment;
    let directory = environment.directory.display().to_string();
    let rules = input
        .discovered
        .instructions
        .iter()
        .map(|file| {
            format!(
                "<file path=\"{}\">\n{}\n</file>",
                file.path.display(),
                file.content.trim_end()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    render(
        PROJECT,
        &[
            ("directory", directory.as_str()),
            ("os", environment.os.as_str()),
            ("date", environment.date.as_str()),
            ("branch", environment.branch.as_deref().unwrap_or_default()),
            ("model", environment.model.as_deref().unwrap_or_default()),
            ("rules", rules.as_str()),
        ],
    )
}

#[cfg(test)]
/// Tests for the system prompt.
mod tests {
    use super::*;
    use crate::testutil::{put, scratch};
    use crate::{Locations, discover};

    /// Everything discovered shows up in the prompt, and the environment with it.
    #[test]
    fn prompt_carries_context() {
        let base = scratch("system-prompt");
        put(&base, "repo/.git/HEAD", "ref: refs/heads/main\n");
        put(&base, "repo/AGENTS.md", "Use tabs. {{not_a_tag}}");
        put(&base, "repo/pkg/CLAUDE.md", "Be brief.");
        put(
            &base,
            "repo/.claude/skills/pdf/SKILL.md",
            "---\ndescription: Reads PDFs\n---\nbody",
        );
        put(&base, "home/.claude/CLAUDE.md", "User rule.");
        let locations = Locations {
            directory: base.join("repo/pkg"),
            root: Some(base.join("repo")),
            home: Some(base.join("home")),
            claude: Some(base.join("home/.claude")),
            codex: Some(base.join("home/.codex")),
        };
        let discovered = discover(&locations, &[]);
        let environment = Environment::detect(&locations, "model-x");
        let tools: Vec<String> = ["read", "bash", "task", "todo"].map(String::from).to_vec();
        let prompt = build_prompt(PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &tools,
            plan: false,
        });
        assert!(prompt.contains("- pdf: Reads PDFs"));
        assert!(prompt.contains("Use tabs. {{not_a_tag}}"));
        assert!(prompt.contains("Be brief."));
        assert!(prompt.contains("User rule."));
        assert!(prompt.contains("- Git branch: main"));
        assert!(prompt.contains("- Model: model-x"));
        assert!(prompt.contains(&format!(
            "- Working directory: {}",
            base.join("repo/pkg").display()
        )));
        assert!(prompt.contains("# Delegation"));
        assert!(!prompt.contains("Plan mode active"));
        assert_eq!(prompt.matches("{{").count(), 1);
        assert!(prompt.find("Be brief.") < prompt.find("Use tabs."));
    }

    /// Tools the agent lacks are not talked about, and plan mode adds its rules.
    #[test]
    fn tools_and_plan_mode_shape_the_prompt() {
        let locations = Locations {
            directory: scratch("system-plan"),
            root: None,
            home: None,
            claude: None,
            codex: None,
        };
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "");
        let tools: Vec<String> = ["read", "grep", "exit_plan_mode"]
            .map(String::from)
            .to_vec();
        let prompt = build_prompt(PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &tools,
            plan: true,
        });
        assert!(prompt.contains("Plan mode active"));
        assert!(!prompt.contains("# Delegation"));
        assert!(!prompt.contains("`bash`: real binaries"));
        assert!(prompt.contains("- `exit_plan_mode`"));
    }

    /// A subagent's prompt starts from its role and carries the project.
    #[test]
    fn subagent_prompt_has_role_and_project() {
        let locations = Locations {
            directory: scratch("system-sub"),
            root: None,
            home: None,
            claude: None,
            codex: None,
        };
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "m");
        let prompt = build_subagent_prompt(
            PromptInput {
                environment: &environment,
                discovered: &discovered,
                tools: &[],
                plan: false,
            },
            "Be a scout.",
        );
        assert!(prompt.starts_with("§ Role\nBe a scout."));
        assert!(prompt.contains("<project-context>"));
    }
}
