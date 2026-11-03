//! The tools and system prompts a turn runs with, for each mode.

use std::sync::Arc;

use omc_agent::{Approvals, EventSink, Live, Task, TaskSettings};
use omc_context::{Discovered, Environment, PromptInput, build_prompt, build_subagent_prompt};
use omc_providers::{ChatModel, Effort};
use omc_tools::{
    Ask, Checkpoint, CheckpointTool, ContextNotes, ExitPlanMode, Host, NewContext, Notebook,
    Registry, Rewind, Todo,
};

/// What the tools and prompts of a turn are built from.
pub(crate) struct Inputs<'a> {
    /// The built-in tools, tool-server tools and the skill tool.
    pub(crate) base: &'a Registry,
    /// The names of the tools the agent is not given.
    pub(crate) denied: &'a [String],
    /// The reader the tools report to.
    pub(crate) host: Arc<dyn Host>,
    /// The model subagents ask.
    pub(crate) model: Arc<dyn ChatModel>,
    /// What is allowed without asking.
    pub(crate) approvals: Arc<Approvals>,
    /// Where the work of subagents is reported.
    pub(crate) events: Arc<EventSink>,
    /// How hard subagents think.
    pub(crate) effort: Option<Effort>,
    /// What was found on disk.
    pub(crate) discovered: Arc<Discovered>,
    /// The workstation.
    pub(crate) environment: Arc<Environment>,
    /// The notes the agent keeps.
    pub(crate) notebook: Arc<Notebook>,
    /// The checkpoint the agent marks, when it has the checkpoint and rewind tools.
    pub(crate) checkpoint: Option<Arc<Checkpoint>>,
}

/// The tools and system prompt of a turn in each mode.
pub(crate) struct Toolset {
    /// What the agent has outside plan mode.
    full: Live,
    /// What the agent has in plan mode.
    plan: Live,
}

impl Toolset {
    /// Builds the tools and prompts from `inputs`.
    pub(crate) fn build(inputs: &Inputs<'_>) -> Self {
        Self {
            full: assemble(inputs, false),
            plan: assemble(inputs, true),
        }
    }

    /// What the agent has in plan mode when `plan` is set, and otherwise outside it.
    pub(crate) fn live(&self, plan: bool) -> Live {
        let chosen = if plan { &self.plan } else { &self.full };
        Live {
            registry: chosen.registry.clone(),
            system: chosen.system.clone(),
        }
    }
}

/// The tools and prompt for plan mode when `plan` is set, and otherwise for the rest.
fn assemble(inputs: &Inputs<'_>, plan: bool) -> Live {
    let base = if plan {
        inputs.base.read_only()
    } else {
        inputs.base.clone()
    };
    let sub_base = Arc::new(base.clone());
    let mut registry = base;
    registry.register(Todo::new(inputs.host.clone()));
    registry.register(Ask::new(inputs.host.clone()));
    registry.register(ContextNotes::new(inputs.notebook.clone()));
    registry.register(NewContext::new(inputs.notebook.clone()));
    if let Some(checkpoint) = &inputs.checkpoint {
        registry.register(CheckpointTool::new(checkpoint.clone()));
        registry.register(Rewind::new(checkpoint.clone()));
    }
    if plan {
        let todo = !inputs.denied.iter().any(|name| name == "todo");
        registry.register(ExitPlanMode::new(inputs.host.clone(), todo));
    }
    let discovered = inputs.discovered.clone();
    let environment = inputs.environment.clone();
    registry.register(Task::new(TaskSettings {
        model: inputs.model.clone(),
        registry: sub_base,
        approvals: inputs.approvals.clone(),
        events: inputs.events.clone(),
        agents: discovered.agents.clone(),
        effort: inputs.effort,
        prompt: Arc::new(move |agent, tools| {
            build_subagent_prompt(
                PromptInput {
                    environment: &environment,
                    discovered: &discovered,
                    tools,
                    plan: false,
                },
                &agent.prompt,
            )
        }),
    }));
    for name in inputs.denied {
        registry.remove(name);
    }
    let system = build_prompt(PromptInput {
        environment: &inputs.environment,
        discovered: &inputs.discovered,
        tools: &registry.names(),
        plan,
    });
    Live {
        registry: Arc::new(registry),
        system: Arc::from(system),
    }
}

#[cfg(test)]
/// Tests for the tools of each mode.
mod tests {
    use super::*;
    use crate::shared::{SessionHost, Shared};
    use omc_providers::{ProviderError, Reply, Request, Sink};

    /// A model that is never asked.
    struct Silent;

    #[async_trait::async_trait]
    impl ChatModel for Silent {
        /// Fails, since the tests here never ask.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            Err(ProviderError::NoModels)
        }
    }

    /// Builds the toolset for the built-in tools in a scratch folder, denying `denied`.
    fn toolset(denied: &[String]) -> Toolset {
        toolset_with(denied, None)
    }

    /// Builds the toolset like [`toolset`], with `checkpoint` given to the agent when it is set.
    fn toolset_with(denied: &[String], checkpoint: Option<Arc<Checkpoint>>) -> Toolset {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/toolset");
        std::fs::create_dir_all(&dir).unwrap();
        let base = Registry::builtin(&dir).unwrap();
        let locations = omc_context::Locations {
            directory: dir.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
        };
        Toolset::build(&Inputs {
            base: &base,
            denied,
            host: Arc::new(SessionHost {
                shared: shared.clone(),
            }),
            model: Arc::new(Silent),
            approvals: shared.approvals.clone(),
            events: Arc::new(|_| {}),
            effort: None,
            discovered: Arc::new(Discovered::default()),
            environment: Arc::new(Environment::detect(&locations, "m")),
            notebook: shared.notebook.clone(),
            checkpoint,
        })
    }

    /// The checkpoint and rewind tools are there in both modes when they are given and in neither otherwise.
    #[test]
    fn checkpoint_tools_follow_the_setting() {
        for plan in [false, true] {
            let names = toolset(&[]).live(plan).registry.names();
            assert!(
                !names.iter().any(|n| n == "checkpoint") && !names.iter().any(|n| n == "rewind")
            );
            let names = toolset_with(&[], Some(Arc::new(Checkpoint::new())))
                .live(plan)
                .registry
                .names();
            assert!(names.iter().any(|n| n == "checkpoint") && names.iter().any(|n| n == "rewind"));
        }
    }

    /// Plan mode keeps the read-only tools and the plan tool; the rest of the time the plan tool is absent and writing is possible.
    #[test]
    fn plan_mode_filters_the_tools() {
        let tools = toolset(&[]);
        assert_eq!(
            tools.live(true).registry.names(),
            vec![
                "ask",
                "context_notes",
                "exit_plan_mode",
                "glob",
                "grep",
                "new_context",
                "read",
                "task",
                "todo"
            ]
        );
        assert_eq!(
            tools.live(false).registry.names(),
            vec![
                "ask",
                "bash",
                "context_notes",
                "edit",
                "glob",
                "grep",
                "new_context",
                "read",
                "task",
                "todo",
                "write"
            ]
        );
        assert!(tools.live(true).system.contains("Plan mode active"));
        assert!(!tools.live(false).system.contains("Plan mode active"));
    }

    /// Denied tools are left out of both modes.
    #[test]
    fn denied_tools_are_left_out() {
        let tools = toolset(&["task".to_owned(), "bash".to_owned()]);
        assert!(tools.live(false).registry.get("task").is_none());
        assert!(tools.live(false).registry.get("bash").is_none());
        assert!(tools.live(true).registry.get("task").is_none());
    }
}
