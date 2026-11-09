//! The tools a turn runs with, for each mode.

use std::sync::Arc;

use omc_agent::{
    Approvals, EventSink, Hooks, Isolation, Models, Nesting, Task, TaskSettings, Throttle,
};
use omc_context::{Discovered, Environment, PromptInput, build_subagent_prompt, subagent_plan};
use omc_memory::Memories;
use omc_providers::{ChatModel, Effort};
use omc_tools::{
    Ask, Checkpoint, CheckpointTool, ContextNotes, Deferral, ExitPlanMode, Host, Jobs, MemoryEdit,
    MemorySearch, NewContext, Notebook, Registry, Remember, Rewind, SetTitle, Todo, Wait,
};

/// Gives the plan the reader approved while it is being carried out, which the subagents started meanwhile are told of; nothing while there is none or a new plan is being made.
pub(crate) type PlanSource = dyn Fn() -> Option<Arc<str>> + Send + Sync;

/// What the tools and prompts of a turn are built from.
pub(crate) struct Inputs<'a> {
    /// The built-in tools, tool-server tools and the skill tool.
    pub(crate) base: &'a Registry,
    /// The names of the tools the agent is not given.
    pub(crate) denied: &'a [String],
    /// Which tools are left out of the tool list until the model looks them up.
    pub(crate) deferral: &'a Deferral,
    /// The reader the tools report to.
    pub(crate) host: Arc<dyn Host>,
    /// The model subagents ask unless another is named.
    pub(crate) model: Arc<dyn ChatModel>,
    /// What that model is called, and how subagents find another by name.
    pub(crate) models: Models,
    /// The seats subagents take to run, shared by all of them.
    pub(crate) throttle: Arc<Throttle>,
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
    /// The plan being carried out, which subagents are told of.
    pub(crate) approved_plan: Arc<PlanSource>,
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

/// The tools the agent and its subagents share in plan mode when `plan` is set, and otherwise outside it: the built-in ones, or those that only look, with memory search when there are memories, less every tool the client denied, the ones the deferral names left out of the tool list until looked up.
fn base_tools(inputs: &Inputs<'_>, plan: bool) -> Registry {
    let mut base = if plan {
        inputs.base.read_only()
    } else {
        inputs.base.clone()
    };
    if let Some(memories) = &inputs.memories {
        base.register(MemorySearch::new(memories.clone()));
    }
    for name in inputs.denied {
        base.remove(name);
    }
    base.defer(inputs.deferral);
    base
}

/// The tools for plan mode when `plan` is set, and otherwise for the rest.
///
/// With memories, the agent can save, search and change them, while its subagents can only search them: what a subagent learns comes back in its result for the main agent to judge before anything is saved.
///
/// Subagents get the tools [`subagent_tools`] leaves them, never one the client denied. The tools that depend on what else the agent has, such as `script`, are fitted to the tools of the mode, so that a script in plan mode can only call the tools that only look.
fn assemble(inputs: &Inputs<'_>, plan: bool) -> Registry {
    let base = base_tools(inputs, plan);
    let sub_base = Arc::new(subagent_tools(&base));
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
        registry.register(
            ExitPlanMode::new(inputs.host.clone(), todo).with_notebook(inputs.notebook.clone()),
        );
    }
    let discovered = inputs.discovered.clone();
    let environment = inputs.environment.clone();
    let tree = inputs.tree.clone();
    let approved_plan = inputs.approved_plan.clone();
    registry.register(Task::new(TaskSettings {
        model: inputs.model.clone(),
        models: inputs.models.clone(),
        registry: sub_base,
        approvals: inputs.approvals.clone(),
        events: inputs.events.clone(),
        agents: discovered.agents.clone(),
        effort: inputs.effort,
        prompt: Arc::new(move |agent, tools, apart, model| {
            let own = subagent_environment(&environment, apart, model);
            let prompt = build_subagent_prompt(
                PromptInput {
                    environment: own.as_ref().unwrap_or(&environment),
                    discovered: &discovered,
                    tools,
                    plan: false,
                    unnamed: false,
                    tree: &tree,
                },
                &agent.prompt,
            );
            with_plan(prompt, approved_plan().as_deref())
        }),
        jobs: inputs.jobs.clone(),
        isolation: inputs.isolation.clone(),
        hooks: inputs.hooks.clone(),
        nesting: Nesting::new(omc_agent::max_depth(), inputs.throttle.clone()),
    }));
    for name in inputs.denied {
        registry.remove(name);
    }
    registry.defer(inputs.deferral);
    registry.fitted()
}

/// `prompt`, a subagent's system prompt, followed by `plan`, the plan being carried out, when there is one.
fn with_plan(prompt: String, plan: Option<&str>) -> String {
    match plan {
        Some(plan) => format!("{prompt}\n\n{}", subagent_plan(plan)),
        None => prompt,
    }
}

/// The environment a subagent's prompt tells of, when it differs from the main agent's `environment`: the folder `apart` it works in when it works in a worktree of its own, with no branch, and the `model` it asks when that is known and another.
fn subagent_environment(
    environment: &Environment,
    apart: Option<&std::path::Path>,
    model: &str,
) -> Option<Environment> {
    let renamed = !model.is_empty() && environment.model.as_deref() != Some(model);
    if apart.is_none() && !renamed {
        return None;
    }
    let mut own = environment.clone();
    if let Some(directory) = apart {
        own.directory = directory.to_path_buf();
        own.branch = None;
    }
    if renamed {
        own.model = Some(model.to_owned());
    }
    Some(own)
}

/// The tools subagents get from `base`: the same, with copies that cannot start background runs, and without `debug`.
///
/// Subagents never get `debug`: its sessions belong to the conversation and outlive the calls that use them, so subagents running at once would steer the same program over each other, and a session one started would outlive it.
fn subagent_tools(base: &Registry) -> Registry {
    let mut tools = base.foreground();
    tools.remove("debug");
    tools
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
        with_inputs(denied, checkpoint, memories, Toolset::build)
    }

    /// Hands `use_inputs` the inputs of a toolset for the built-in tools in a scratch folder, denying `denied`, with `checkpoint` and `memories` when they are set, hiding no tool, and returns what it made of them.
    fn with_inputs<T>(
        denied: &[String],
        checkpoint: Option<Arc<Checkpoint>>,
        memories: Option<Arc<Memories>>,
        use_inputs: impl FnOnce(&Inputs<'_>) -> T,
    ) -> T {
        with_deferral(denied, checkpoint, memories, &Deferral::none(), use_inputs)
    }

    /// Hands `use_inputs` the inputs of a toolset like [`with_inputs`], hiding the tools `deferral` names, and returns what it made of them.
    fn with_deferral<T>(
        denied: &[String],
        checkpoint: Option<Arc<Checkpoint>>,
        memories: Option<Arc<Memories>>,
        deferral: &Deferral,
        use_inputs: impl FnOnce(&Inputs<'_>) -> T,
    ) -> T {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/toolset");
        std::fs::create_dir_all(&dir).unwrap();
        let mut base = Registry::builtin(&dir).unwrap();
        base.register(omc_script::Script::new(omc_tools::Spill::disabled()));
        let locations = omc_context::Locations {
            directory: dir.clone(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        use_inputs(&Inputs {
            base: &base,
            denied,
            deferral,
            host: Arc::new(SessionHost {
                shared: shared.clone(),
            }),
            model: Arc::new(Silent),
            models: Models::default(),
            throttle: shared.throttle.clone(),
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
            approved_plan: Arc::new(|| None),
        })
    }

    /// A subagent's prompt tells of the model it asks and of the worktree it works in, and is the main agent's environment when neither differs.
    #[test]
    fn subagents_are_told_their_own_model_and_folder() {
        let main = Environment {
            directory: "/work".into(),
            os: "linux".into(),
            date: "2026-01-01".into(),
            branch: Some("main".into()),
            model: Some("big".into()),
        };
        assert_eq!(subagent_environment(&main, None, ""), None);
        assert_eq!(subagent_environment(&main, None, "big"), None);
        let renamed = subagent_environment(&main, None, "small").unwrap();
        assert_eq!(renamed.model.as_deref(), Some("small"));
        assert_eq!(renamed.branch.as_deref(), Some("main"));
        let moved = subagent_environment(&main, Some(std::path::Path::new("/wt")), "").unwrap();
        assert_eq!(moved.directory, std::path::PathBuf::from("/wt"));
        assert_eq!((moved.branch, moved.model.as_deref()), (None, Some("big")));
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

    /// Plan mode keeps the read-only tools, the copies that only look of tools such as `lsp`, and the plan tool; the rest of the time the plan tool is absent and writing is possible.
    #[test]
    fn plan_mode_filters_the_tools() {
        let tools = toolset(&[]);
        assert!(tools.tools(true).get("lsp").unwrap().read_only());
        assert!(!tools.tools(false).get("lsp").unwrap().read_only());
        assert_eq!(
            tools.tools(true).names(),
            vec![
                "ask",
                "ast_grep",
                "context_notes",
                "exit_plan_mode",
                "glob",
                "grep",
                "lsp",
                "new_context",
                "read",
                "script",
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
                "lsp",
                "new_context",
                "read",
                "script",
                "set_title",
                "task",
                "todo",
                "wait",
                "web_search",
                "write"
            ]
        );
    }

    /// The script tool of each mode can call that mode's tools, less the ones scripts never get: in plan mode only those that look; a denied tool is not offered to scripts, and a denied script is gone.
    #[test]
    fn scripts_call_the_tools_of_their_mode() {
        let tools = toolset(&["bash".to_owned()]);
        let offered = |plan: bool| {
            let script = tools.tools(plan).get("script").cloned().unwrap();
            let description = script.description().to_owned();
            (script.read_only(), description)
        };
        let (looks, plan) = offered(true);
        assert!(looks);
        assert!(plan.contains("tools.read(") && plan.contains("tools.grep("));
        for absent in [
            "tools.write(",
            "tools.edit(",
            "tools.bash(",
            "tools.task(",
            "tools.ask(",
            "tools.exit_plan_mode(",
        ] {
            assert!(!plan.contains(absent), "{absent} offered in plan mode");
        }
        let (looks, full) = offered(false);
        assert!(!looks);
        assert!(full.contains("tools.write(") && full.contains("tools.edit("));
        assert!(!full.contains("tools.bash(") && !full.contains("tools.todo("));
        assert!(
            toolset(&["script".to_owned()])
                .tools(false)
                .get("script")
                .is_none()
        );
    }

    /// The agent has `debug` outside plan mode only, and subagents never have it.
    #[test]
    fn debug_is_the_main_agents_alone() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/toolset-debug");
        std::fs::create_dir_all(&dir).unwrap();
        let sandbox = omc_tools::Sandbox::new(&dir).unwrap();
        let mut base = Registry::builtin_for(sandbox.clone(), None, Default::default());
        base.register(omc_dap::DebugTool::new(
            sandbox,
            Arc::new(omc_dap::Sessions::new()),
        ));
        assert!(base.get("debug").is_some());
        assert!(base.read_only().get("debug").is_none());
        let sub = subagent_tools(&base);
        assert!(sub.get("debug").is_none());
        assert!(sub.get("bash").is_some());
    }

    /// Denied tools are left out of both modes.
    #[test]
    fn denied_tools_are_left_out() {
        let tools = toolset(&["task".to_owned(), "bash".to_owned()]);
        assert!(tools.tools(false).get("task").is_none());
        assert!(tools.tools(false).get("bash").is_none());
        assert!(tools.tools(true).get("task").is_none());
    }

    /// The hidden tools stay in both modes and in what subagents get, marked hidden, with a `load_tools` naming them; the prefix keeps them off the tool list it offers, with the same `load_tools`, and still runs them.
    #[tokio::test]
    async fn hidden_tools_stay_callable_but_off_the_list() {
        let deferral = Deferral::parse(Some("ast_grep,debug"));
        let (tools, sub) = with_deferral(&[], None, None, &deferral, |inputs| {
            (
                Toolset::build(inputs),
                subagent_tools(&base_tools(inputs, false)).fitted(),
            )
        });
        for plan in [false, true] {
            let registry = tools.tools(plan);
            assert!(registry.is_deferred("ast_grep") && !registry.is_deferred("grep"));
            let lookup = registry.get(omc_tools::LOAD_TOOLS).unwrap();
            assert!(
                lookup
                    .description()
                    .contains("- `ast_grep`: Search code by syntax tree")
            );
        }
        assert!(sub.is_deferred("ast_grep") && sub.get(omc_tools::LOAD_TOOLS).is_some());
        let locations = omc_context::Locations {
            directory: std::path::PathBuf::from("/work"),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let prefix = crate::prefix::Prefix::freeze(
            crate::prefix::Key {
                provider: omc_providers::Provider::Claude,
                model: "m".into(),
                checkpoints: false,
                memory: false,
            },
            &tools,
            &Environment::detect(&locations, "m"),
            "",
            Arc::new(Discovered::default()),
            false,
        );
        let planning: crate::prefix::Planning = Arc::new(|| false);
        let offered = prefix.registry(&tools, &planning);
        let listed: Vec<&str> = offered.offered().map(|tool| tool.name()).collect();
        assert!(listed.contains(&omc_tools::LOAD_TOOLS) && listed.contains(&"grep"));
        assert!(!listed.contains(&"ast_grep"));
        assert_eq!(
            offered.get(omc_tools::LOAD_TOOLS).unwrap().description(),
            prefix
                .registry(&tools, &planning)
                .get(omc_tools::LOAD_TOOLS)
                .unwrap()
                .description()
        );
        let loaded = offered
            .call(
                omc_tools::LOAD_TOOLS,
                serde_json::json!({"names": ["ast_grep"]}),
            )
            .await
            .unwrap();
        assert!(loaded.text.contains("<tool name=\"ast_grep\">"));
        let ran = offered
            .call(
                "ast_grep",
                serde_json::json!({"pattern": "fn $NAME()", "lang": "rust"}),
            )
            .await;
        assert!(!matches!(ran, Err(omc_tools::ToolError::Unknown(_))));
    }

    /// A tool the client denied is kept from subagents as well as from the agent, in both modes.
    #[test]
    fn denied_tools_are_kept_from_subagents() {
        with_inputs(
            &["bash".to_owned(), "write".to_owned()],
            None,
            None,
            |inputs| {
                for plan in [false, true] {
                    let sub = subagent_tools(&base_tools(inputs, plan));
                    assert!(sub.get("bash").is_none() && sub.get("write").is_none());
                    assert!(sub.get("read").is_some());
                }
            },
        );
    }
}
