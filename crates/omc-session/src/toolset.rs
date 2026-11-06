//! The tools a turn runs with, for each mode.

use std::sync::Arc;

use omc_agent::{Approvals, EventSink, Hooks, Isolation, Task, TaskSettings};
use omc_context::{Discovered, Environment, PromptInput, build_subagent_prompt};
use omc_memory::Memories;
use omc_providers::{ChatModel, Effort};
use omc_tools::{
    Ask, Checkpoint, CheckpointTool, ContextNotes, ExitPlanMode, Host, Jobs, MemoryEdit,
    MemorySearch, NewContext, Notebook, Registry, Remember, Rewind, SetTitle, Todo, Wait,
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
    /// The outline of the working directory, made when the conversation started; empty for none.
    pub(crate) tree: Arc<str>,
    /// The notes the agent keeps.
    pub(crate) notebook: Arc<Notebook>,
    /// The checkpoint the agent marks, when it has the checkpoint and rewind tools.
    pub(crate) checkpoint: Option<Arc<Checkpoint>>,
    /// Whether the conversation has no name yet, so that the agent is given the tool to name it.
    pub(crate) unnamed: bool,
    /// The background runs of the conversation.
    pub(crate) jobs: Arc<Jobs>,
    /// How subagents get git worktrees of their own, when they can.
    pub(crate) isolation: Option<Arc<Isolation>>,
    /// The memories the agent reads and writes, when it has its memory.
    pub(crate) memories: Option<Arc<Memories>>,
    /// The hooks subagents run, when there are any.
    pub(crate) hooks: Option<Arc<dyn Hooks>>,
}

/// The tools of a turn in each mode.
pub(crate) struct Toolset {
    /// What the agent has outside plan mode.
    full: Arc<Registry>,
    /// What the agent has in plan mode.
    plan: Arc<Registry>,
}

impl Toolset {
    /// Builds the tools from `inputs`.
    pub(crate) fn build(inputs: &Inputs<'_>) -> Self {
        Self {
            full: Arc::new(assemble(inputs, false)),
            plan: Arc::new(assemble(inputs, true)),
        }
    }

    /// What the agent has in plan mode when `plan` is set, and otherwise outside it.
    pub(crate) fn tools(&self, plan: bool) -> &Arc<Registry> {
        if plan { &self.plan } else { &self.full }
    }
}

/// The tools for plan mode when `plan` is set, and otherwise for the rest.
///
/// With memories, the agent can save, search and change them, while its subagents can only search them: what a subagent learns comes back in its result for the main agent to judge before anything is saved.
fn assemble(inputs: &Inputs<'_>, plan: bool) -> Registry {
    let mut base = if plan {
        inputs.base.read_only()
    } else {
        inputs.base.clone()
    };
    if let Some(memories) = &inputs.memories {
        base.register(MemorySearch::new(memories.clone()));
    }
    let sub_base = Arc::new(base.foreground());
    let mut registry = base;
    if let Some(memories) = &inputs.memories {
        registry.register(Remember::new(memories.clone()));
        registry.register(MemoryEdit::new(memories.clone()));
    }
    registry.register(Todo::new(inputs.host.clone()));
    registry.register(Ask::new(inputs.host.clone()));
    registry.register(ContextNotes::new(inputs.notebook.clone()));
    registry.register(NewContext::new(inputs.notebook.clone()));
    registry.register(Wait::new(inputs.jobs.clone()));
    if inputs.unnamed {
        registry.register(SetTitle::new(inputs.host.clone()));
    }
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
    let tree = inputs.tree.clone();
    registry.register(Task::new(TaskSettings {
        model: inputs.model.clone(),
        registry: sub_base,
        approvals: inputs.approvals.clone(),
        events: inputs.events.clone(),
        agents: discovered.agents.clone(),
        effort: inputs.effort,
        prompt: Arc::new(move |agent, tools, apart| {
            let moved = apart.map(|directory| Environment {
                directory: directory.to_path_buf(),
                branch: None,
                ..(*environment).clone()
            });
            build_subagent_prompt(
                PromptInput {
                    environment: moved.as_ref().unwrap_or(&environment),
                    discovered: &discovered,
                    tools,
                    plan: false,
                    unnamed: false,
                    tree: &tree,
                },
                &agent.prompt,
            )
        }),
        jobs: inputs.jobs.clone(),
        isolation: inputs.isolation.clone(),
        hooks: inputs.hooks.clone(),
    }));
    for name in inputs.denied {
        registry.remove(name);
    }
    registry
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
        toolset_full(denied, checkpoint, None)
    }

    /// Builds the toolset like [`toolset_with`], with `memories` given to the agent when they are set.
    fn toolset_full(
        denied: &[String],
        checkpoint: Option<Arc<Checkpoint>>,
        memories: Option<Arc<Memories>>,
    ) -> Toolset {
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
            project: true,
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
            tree: Arc::from(""),
            notebook: shared.notebook.clone(),
            checkpoint,
            jobs: shared.jobs.clone(),
            unnamed: true,
            isolation: None,
            memories,
            hooks: None,
        })
    }

    /// With memories, both modes have the three memory tools; without, neither has any.
    #[test]
    fn memory_tools_follow_the_memories() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/toolset-memory");
        let memories = Arc::new(Memories::new(
            &omc_memory::Store::new(root),
            omc_memory::Project::of(std::path::Path::new(env!("CARGO_MANIFEST_DIR"))),
            None,
        ));
        let tools = toolset_full(&[], None, Some(memories));
        for plan in [false, true] {
            let names = tools.tools(plan).names();
            for name in ["remember", "memory_search", "memory_edit"] {
                assert!(names.iter().any(|n| n == name), "{name} missing");
            }
        }
        for plan in [false, true] {
            let names = toolset(&[]).tools(plan).names();
            assert!(
                !names
                    .iter()
                    .any(|n| n.starts_with("memory") || n == "remember")
            );
        }
    }

    /// The checkpoint and rewind tools are there in both modes when they are given and in neither otherwise.
    #[test]
    fn checkpoint_tools_follow_the_setting() {
        for plan in [false, true] {
            let names = toolset(&[]).tools(plan).names();
            assert!(
                !names.iter().any(|n| n == "checkpoint") && !names.iter().any(|n| n == "rewind")
            );
            let names = toolset_with(&[], Some(Arc::new(Checkpoint::new())))
                .tools(plan)
                .names();
            assert!(names.iter().any(|n| n == "checkpoint") && names.iter().any(|n| n == "rewind"));
        }
    }

    /// Plan mode keeps the read-only tools and the plan tool; the rest of the time the plan tool is absent and writing is possible.
    #[test]
    fn plan_mode_filters_the_tools() {
        let tools = toolset(&[]);
        assert_eq!(
            tools.tools(true).names(),
            vec![
                "ask",
                "ast_grep",
                "context_notes",
                "exit_plan_mode",
                "glob",
                "grep",
                "new_context",
                "read",
                "set_title",
                "task",
                "todo",
                "wait",
                "web_search"
            ]
        );
        assert_eq!(
            tools.tools(false).names(),
            vec![
                "ask",
                "ast_edit",
                "ast_grep",
                "bash",
                "context_notes",
                "edit",
                "glob",
                "grep",
                "new_context",
                "read",
                "set_title",
                "task",
                "todo",
                "wait",
                "web_search",
                "write"
            ]
        );
    }

    /// Denied tools are left out of both modes.
    #[test]
    fn denied_tools_are_left_out() {
        let tools = toolset(&["task".to_owned(), "bash".to_owned()]);
        assert!(tools.tools(false).get("task").is_none());
        assert!(tools.tools(false).get("bash").is_none());
        assert!(tools.tools(true).get("task").is_none());
    }
}
