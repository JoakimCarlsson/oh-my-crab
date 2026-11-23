//! The system prompt, built from the templates in `prompts/`.

use crate::instructions::blocks;
use crate::template::render;
use crate::{Discovered, Environment, environment_reminder};

/// The main agent's prompt, crab's built-in instructions, which a [`crate::SYSTEM_FILE`] starts as a copy of to change them.
pub const SYSTEM_TEMPLATE: &str = include_str!("prompts/system.md");

/// The parts of the main agent's prompt crab always adds, each with the tag that places it, put at the end of a [`crate::SYSTEM_FILE`] that leaves its tag out.
const KEPT: [(&str, &str); 6] = [
    (
        "{{orchestrator}}",
        "{{#if has_task}}{{#if orchestrator}}# Delegation\n{{orchestrator}}{{/if}}{{/if}}",
    ),
    ("{{runtime}}", "{{runtime}}"),
    ("{{unnamed}}", "{{unnamed}}"),
    ("{{project}}", "{{project}}"),
    ("{{append}}", "{{append}}"),
    ("{{plan}}", "{{plan}}"),
];

/// The skills and the tool inventory, part of the main agent's prompt whether or not its instructions are replaced.
const RUNTIME: &str = include_str!("prompts/runtime.md");

/// What the main agent's prompt says while the conversation has no name.
const UNNAMED: &str = include_str!("prompts/unnamed.md");

/// What the main agent's prompt says about delegation while orchestrator mode is on, in place of holding subagents back.
const ORCHESTRATOR: &str = include_str!("prompts/orchestrator.md");

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
    /// Whether the conversation has no name yet, which the prompt then asks the agent to give it.
    pub unnamed: bool,
    /// Whether orchestrator mode is on, in which the main agent's prompt tells it to lead and hand work to subagents by work category; it only shows when the agent has the `task` tool.
    pub orchestrator: bool,
    /// The outline of the working directory from [`crate::workspace_tree`], made once when the conversation started; empty for none.
    pub tree: &'a str,
}

/// The system prompt of the main agent for `input`.
///
/// With orchestrator mode on and the `task` tool among the tools, the prompt tells the agent to lead and hand work to subagents by work category, also after a [`crate::SYSTEM_FILE`]; with it off the prompt is what it is without the mode.
///
/// A [`crate::SYSTEM_FILE`] that was found is a changed copy of [`SYSTEM_TEMPLATE`] and is rendered in its place; the skills, the tool inventory, the project, the [`crate::APPEND_SYSTEM_FILE`] and the rules of plan mode are added at its end when it leaves their tags out, so it changes crab's instructions but never drops them. It holds nothing that changes from day to day, so that the provider's prompt cache keeps it; the date and the branch go to the agent in [`crate::environment_reminder`] instead.
pub fn build_prompt(input: PromptInput<'_>) -> String {
    let flags = flags(input.tools);
    let project = project(&input);
    let skills = skills(input.discovered);
    let plan = if input.plan {
        render(PLAN, &flags_ref(&flags))
    } else {
        String::new()
    };
    let tools = inventory(input.tools);
    let mut vars = flags_ref(&flags);
    vars.extend([("skills", skills.as_str()), ("tools", tools.as_str())]);
    let runtime = render(RUNTIME, &vars);
    let unnamed = if input.unnamed { UNNAMED } else { "" };
    let orchestrator = if input.orchestrator {
        ORCHESTRATOR.trim()
    } else {
        ""
    };
    let append = appended(input.discovered);
    vars.extend([
        ("runtime", runtime.as_str()),
        ("unnamed", unnamed),
        ("orchestrator", orchestrator),
        ("project", project.as_str()),
        ("append", append),
        ("plan", plan.as_str()),
    ]);
    match input.discovered.system.as_ref() {
        Some(file) => render(&completed(&file.content), &vars),
        None => render(SYSTEM_TEMPLATE, &vars),
    }
}

/// `template`, a [`crate::SYSTEM_FILE`], with every part in [`KEPT`] whose tag it leaves out added at its end.
fn completed(template: &str) -> String {
    let mut whole = template.trim().to_owned();
    for (tag, part) in KEPT {
        if !whole.contains(tag) {
            whole.push_str("\n\n");
            whole.push_str(part);
        }
    }
    whole
}

/// The text of the [`crate::APPEND_SYSTEM_FILE`] found, trimmed, or nothing.
fn appended(discovered: &Discovered) -> &str {
    discovered
        .append_system
        .as_ref()
        .map_or("", |file| file.content.trim())
}

/// The system prompt of a subagent whose role is `role`, for `input`; a subagent lives for one task, so its prompt carries the date and the branch itself.
///
/// An [`crate::APPEND_SYSTEM_FILE`] that was found goes after the environment; a [`crate::SYSTEM_FILE`] does not apply, since the role takes the place of crab's built-in instructions.
pub fn build_subagent_prompt(input: PromptInput<'_>, role: &str) -> String {
    let flags = flags(input.tools);
    let project = project(&input);
    let environment = environment_reminder(input.environment);
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
        ("environment", environment.as_str()),
        ("append", appended(input.discovered)),
        ("plan", plan.as_str()),
        ("tools", tools.as_str()),
    ]);
    render(SUBAGENT, &vars)
}

/// One `has_<tool>` flag for each tool the prompt knows how to talk about, set when `tools` names the tool.
pub(crate) fn flags(tools: &[String]) -> Vec<(String, String)> {
    [
        "read",
        "grep",
        "glob",
        "bash",
        "edit",
        "write",
        "todo",
        "ask",
        "task",
        "script",
        "skill",
        "ast_grep",
        "ast_edit",
        "lsp",
        "remember",
        "memory_search",
        "memory_edit",
        "web_search",
    ]
    .iter()
    .map(|tool| {
        let has = tools.iter().any(|name| name == tool);
        (format!("has_{tool}"), if has { "1" } else { "" }.to_owned())
    })
    .collect()
}

/// `flags` as the pairs the renderer takes.
pub(crate) fn flags_ref(flags: &[(String, String)]) -> Vec<(&str, &str)> {
    flags
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect()
}

/// The rules of plan mode for an agent that has `tools`.
pub(crate) fn plan_rules(tools: &[String]) -> String {
    render(PLAN, &flags_ref(&flags(tools)))
}

/// The list of tools, one to a line.
pub(crate) fn inventory(tools: &[String]) -> String {
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

/// The workstation, the outline of the working directory and the instruction files, as the project part of a prompt.
fn project(input: &PromptInput<'_>) -> String {
    let environment = input.environment;
    let directory = environment.directory.display().to_string();
    let rules = blocks(&input.discovered.instructions);
    render(
        PROJECT,
        &[
            ("directory", directory.as_str()),
            ("os", environment.os.as_str()),
            ("model", environment.model.as_deref().unwrap_or_default()),
            ("rules", rules.as_str()),
            ("tree", input.tree),
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
            crab: None,
            project: true,
        };
        let discovered = discover(&locations, &[]);
        let environment = Environment::detect(&locations, "model-x");
        let tools: Vec<String> = ["read", "bash", "task", "todo"].map(String::from).to_vec();
        let prompt = build_prompt(PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &tools,
            plan: false,
            unnamed: false,
            orchestrator: false,
            tree: "",
        });
        assert!(!prompt.contains("<workspace-tree>"));
        assert!(prompt.contains("- pdf: Reads PDFs"));
        assert!(prompt.contains("Use tabs. {{not_a_tag}}"));
        assert!(prompt.contains("Be brief."));
        assert!(prompt.contains("User rule."));
        assert!(!prompt.contains("Git branch") && !prompt.contains(&environment.date));
        assert!(environment_reminder(&environment).contains("Git branch: main."));
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
            crab: None,
            project: true,
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
            unnamed: false,
            orchestrator: false,
            tree: "",
        });
        assert!(prompt.contains("Plan mode active"));
        assert!(!prompt.contains("`set_title`"));
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
            crab: None,
            project: true,
        };
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "m");
        let prompt = build_subagent_prompt(
            PromptInput {
                environment: &environment,
                discovered: &discovered,
                tools: &[],
                plan: false,
                unnamed: false,
                orchestrator: false,
                tree: "src/\n  lib.rs",
            },
            "Be a scout.",
        );
        assert!(prompt.starts_with("§ Role\nBe a scout."));
        assert!(prompt.contains("<project-context>"));
        assert!(prompt.contains(&environment_reminder(&environment)));
        assert!(prompt.contains("src/\n  lib.rs\n</workspace-tree>"));
    }

    /// The outline of the working directory goes into the project part, said to be a snapshot.
    #[test]
    fn prompt_carries_the_tree() {
        let base = scratch("system-tree");
        put(&base, "src/main.rs", "");
        put(&base, "Cargo.toml", "");
        let locations = Locations {
            directory: base.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let tree = crate::workspace_tree(&base);
        assert_eq!(tree, "src/\n  main.rs\nCargo.toml");
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "");
        let prompt = build_prompt(PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &[],
            plan: false,
            unnamed: false,
            orchestrator: false,
            tree: &tree,
        });
        let start = prompt.find("<workspace-tree>").unwrap();
        let end = prompt.find("</workspace-tree>").unwrap();
        let block = &prompt[start..end];
        assert!(block.contains("snapshot from the start of this conversation"));
        assert!(block.ends_with("src/\n  main.rs\nCargo.toml\n"));
        assert!(end < prompt.find("</project-context>").unwrap());
    }

    /// An unnamed conversation is asked to name itself, and a named one is not.
    #[test]
    fn unnamed_conversations_are_asked_for_a_name() {
        let locations = Locations {
            directory: scratch("system-unnamed"),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "");
        let tools: Vec<String> = vec!["read".to_owned(), "set_title".to_owned()];
        let input = |unnamed| PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &tools,
            plan: false,
            unnamed,
            orchestrator: false,
            tree: "",
        };
        assert!(build_prompt(input(true)).contains("call `set_title` once"));
        assert!(!build_prompt(input(false)).contains("call `set_title` once"));
    }

    /// Orchestrator mode adds its rules to the delegation section in place of holding subagents back, only for an agent that has `task`, also after a `SYSTEM.md`; with it off the prompt is the same as before.
    #[test]
    fn orchestrator_mode_shapes_the_delegation_section() {
        let base = scratch("system-orchestrator");
        let locations = Locations {
            directory: base.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let discovered = Discovered::default();
        let environment = Environment::detect(&locations, "m");
        let with_task: Vec<String> = ["read", "task"].map(String::from).to_vec();
        let without: Vec<String> = vec!["read".to_owned()];
        let input = |tools, orchestrator| PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools,
            plan: false,
            unnamed: false,
            orchestrator,
            tree: "",
        };
        let off = build_prompt(input(&with_task, false));
        let on = build_prompt(input(&with_task, true));
        assert!(off.contains("# Delegation\nNo subagents unless"));
        assert!(!off.contains("Orchestrator mode") && !off.contains("`category`"));
        assert!(on.contains("# Delegation\nOrchestrator mode on. You lead"));
        assert!(!on.contains("No subagents unless"));
        for told in [
            "by `category`",
            "cheapest category that fits",
            "the subagent starts blank",
            "one writer per area",
            "unverified work is not done",
            "with `continue`",
        ] {
            assert!(on.contains(told), "{told}");
        }
        assert!(on.contains("with `continue`; NEVER a new one that must learn everything again.\n- Fan genuine slices"));
        assert_eq!(
            build_prompt(input(&without, true)),
            build_prompt(input(&without, false))
        );
        put(&base, ".oh-my-crab/SYSTEM.md", "You are a pirate.\n");
        let replaced = discover(&locations, &[]);
        let own = |orchestrator| {
            build_prompt(PromptInput {
                discovered: &replaced,
                ..input(&with_task, orchestrator)
            })
        };
        assert!(own(true).contains("You are a pirate.\n\n# Delegation\nOrchestrator mode on."));
        assert!(!own(false).contains("# Delegation"));
    }

    /// A `SYSTEM.md` is rendered in place of crab's built-in instructions, with the skills, the tools, the project and plan mode added when it leaves them out; an `APPEND_SYSTEM.md` follows the project in the main prompt and the environment in a subagent's, which never takes the `SYSTEM.md`.
    #[test]
    fn system_files_shape_the_prompt() {
        let base = scratch("system-files");
        put(&base, "repo/AGENTS.md", "Use tabs.");
        put(
            &base,
            "repo/.claude/skills/pdf/SKILL.md",
            "---\ndescription: Reads PDFs\n---\nbody",
        );
        put(
            &base,
            "repo/.oh-my-crab/SYSTEM.md",
            "You are a {{pirate}}.\n{{#if has_task}}You may delegate.{{/if}}\n",
        );
        put(
            &base,
            "config/oh-my-crab/APPEND_SYSTEM.md",
            "Answer in French.",
        );
        let locations = Locations {
            directory: base.join("repo"),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: Some(base.join("config/oh-my-crab")),
            project: true,
        };
        let discovered = discover(&locations, &[]);
        let environment = Environment::detect(&locations, "m");
        let tools: Vec<String> = ["read", "task", "set_title"].map(String::from).to_vec();
        let input = PromptInput {
            environment: &environment,
            discovered: &discovered,
            tools: &tools,
            plan: true,
            unnamed: true,
            orchestrator: false,
            tree: "",
        };
        let prompt = build_prompt(input);
        assert!(prompt.starts_with("You are a .\nYou may delegate.\n"));
        assert!(!prompt.contains("crab's trusted coding assistant"));
        assert!(!prompt.contains("# Delegation"));
        assert!(prompt.contains("- pdf: Reads PDFs"));
        assert!(prompt.contains("- `task`"));
        assert!(prompt.contains("call `set_title` once"));
        assert!(prompt.contains("Use tabs."));
        assert!(prompt.contains("Plan mode active"));
        let append = prompt.find("Answer in French.").unwrap();
        assert!(prompt.find("</project-context>").unwrap() < append);
        assert!(append < prompt.find("Plan mode active").unwrap());
        let builtin = build_prompt(PromptInput {
            discovered: &Discovered {
                system: None,
                ..discovered.clone()
            },
            ..input
        });
        assert!(builtin.contains("crab's trusted coding assistant"));
        let append = builtin.find("Answer in French.").unwrap();
        assert!(builtin.find("</project-context>").unwrap() < append);
        assert!(append < builtin.find("Plan mode active").unwrap());
        let subagent = build_subagent_prompt(input, "Be a scout.");
        assert!(subagent.starts_with("§ Role\nBe a scout."));
        assert!(!subagent.contains("pirate"));
        assert!(
            subagent.find(&environment_reminder(&environment)).unwrap()
                < subagent.find("Answer in French.").unwrap()
        );
    }

    /// A `SYSTEM.md` that is an unchanged copy of the built-in instructions gives the built-in prompt.
    #[test]
    fn a_copy_of_the_template_changes_nothing() {
        let base = scratch("system-copy");
        put(&base, ".oh-my-crab/SYSTEM.md", SYSTEM_TEMPLATE);
        let locations = Locations {
            directory: base.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let copied = discover(&locations, &[]);
        let environment = Environment::detect(&locations, "m");
        let tools: Vec<String> = ["read", "task"].map(String::from).to_vec();
        let input = PromptInput {
            environment: &environment,
            discovered: &copied,
            tools: &tools,
            plan: true,
            unnamed: true,
            orchestrator: true,
            tree: "",
        };
        let builtin = build_prompt(PromptInput {
            discovered: &Discovered {
                system: None,
                ..copied.clone()
            },
            ..input
        });
        assert_eq!(build_prompt(input), builtin);
    }
}
