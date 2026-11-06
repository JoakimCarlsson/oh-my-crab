//! The part of every request that comes before the conversation: the system prompt and the tool list.
//!
//! The provider's prompt cache keeps a request's prefix only while it stays byte for byte the same, and tools and system prompt come first, so a change to either costs the whole cache. They are made once for a conversation and kept; what changes later, plan mode turning on or off, the date, skills, agents and instruction files found again on disk, is told to the agent in a message at the start of the next turn, and the tools plan mode forbids are refused when they are called instead of being taken off the list.

use std::sync::Arc;

use async_trait::async_trait;
use omc_agent::RECALL;
use omc_context::{
    Discovered, Environment, Notice, PromptInput, build_prompt, context_changes,
    environment_reminder, memory_notice, plan_mode_notice,
};
use omc_providers::{Message, Provider};
use omc_tools::{Registry, Tool, ToolError, ToolOutput};
use serde_json::Value;

use crate::toolset::Toolset;

/// Says whether plan mode is on now.
pub(crate) type Planning = Arc<dyn Fn() -> bool + Send + Sync>;

/// What a prefix is made for. A new prefix is made when any of it changes: a new model has no cache to keep, and turning the checkpoint tools or memory on or off is the reader's choice to change the tools.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Key {
    /// The provider that serves the model.
    pub(crate) provider: Provider,
    /// The model.
    pub(crate) model: String,
    /// Whether the agent has the checkpoint and rewind tools.
    pub(crate) checkpoints: bool,
    /// Whether the agent has its memory.
    pub(crate) memory: bool,
}

/// A tool as the model is told about it.
#[derive(Clone)]
struct Spec {
    /// The name the model calls it by.
    name: &'static str,
    /// What it does, for the model.
    description: String,
    /// The JSON schema of its arguments.
    schema: Value,
}

/// The system prompt and tool list of a conversation, made once.
pub(crate) struct Prefix {
    /// What it was made for.
    key: Key,
    /// The system prompt.
    pub(crate) system: Arc<str>,
    /// The tools of both modes, by name.
    specs: Vec<Spec>,
    /// What was found on disk when it was made, which the system prompt describes.
    discovered: Arc<Discovered>,
}

impl Prefix {
    /// Makes the prefix for `key` from `toolset`: every tool of either mode, described as it is now, with `recall` among them although the agent only answers it once the history has been pruned or compacted, and a system prompt for that tool list outside plan mode, describing `environment`, the working directory outline `tree` and `discovered`, which asks the agent to name the conversation when it is `unnamed`.
    pub(crate) fn freeze(
        key: Key,
        toolset: &Toolset,
        environment: &Environment,
        tree: &str,
        discovered: Arc<Discovered>,
        unnamed: bool,
    ) -> Self {
        let mut specs: Vec<Spec> = Vec::new();
        for tool in toolset
            .tools(false)
            .iter()
            .chain(toolset.tools(true).iter())
        {
            if !specs.iter().any(|spec| spec.name == tool.name()) {
                specs.push(Spec {
                    name: tool.name(),
                    description: tool.description().to_owned(),
                    schema: tool.schema(),
                });
            }
        }
        if !specs.iter().any(|spec| spec.name == RECALL) {
            specs.push(recall_spec());
        }
        specs.sort_by_key(|spec| spec.name);
        let names: Vec<String> = specs.iter().map(|spec| spec.name.to_owned()).collect();
        let system = build_prompt(PromptInput {
            environment,
            discovered: &discovered,
            tools: &names,
            plan: false,
            unnamed,
            tree,
        });
        Self {
            key,
            system: Arc::from(system),
            specs,
            discovered,
        }
    }

    /// What the prefix was made for.
    pub(crate) fn key(&self) -> &Key {
        &self.key
    }

    /// The tools to offer: every tool of the prefix, described as when it was made, each running the tool of the same name in `toolset` for the mode `planning` says is on, and refusing the call when that mode has no such tool.
    pub(crate) fn registry(&self, toolset: &Toolset, planning: &Planning) -> Registry {
        let allowed: Arc<[String]> = self.allowed_in_plan(toolset).into();
        let mut registry = Registry::new();
        for spec in &self.specs {
            registry.register(Gated {
                spec: spec.clone(),
                full: toolset.tools(false).get(spec.name).cloned(),
                plan: toolset.tools(true).get(spec.name).cloned(),
                planning: planning.clone(),
                allowed: allowed.clone(),
            });
        }
        registry
    }

    /// The tools of the prefix that plan mode allows, by name.
    fn allowed_in_plan(&self, toolset: &Toolset) -> Vec<String> {
        let plan = toolset.tools(true);
        self.specs
            .iter()
            .filter(|spec| plan.get(spec.name).is_some())
            .map(|spec| spec.name.to_owned())
            .collect()
    }

    /// The notices the next prompt starts with, for a conversation whose history is `history`: the date and branch of `environment`, what differs between `discovered` and what the system prompt describes, pointing only at tools the prefix offers, and plan mode being `plan` with the tools `toolset` allows in it; each only when the latest notice of its kind the model still sees says something else.
    pub(crate) fn notices(
        &self,
        history: &[Message],
        environment: &Environment,
        discovered: &Discovered,
        plan: bool,
        toolset: &Toolset,
    ) -> Vec<String> {
        let visible = omc_agent::context(history);
        let mut told = Vec::new();
        let reminder = environment_reminder(environment);
        if last_notice(&visible, Notice::Environment) != Some(reminder.as_str()) {
            told.push(reminder);
        }
        let offered: Vec<String> = self.specs.iter().map(|spec| spec.name.to_owned()).collect();
        let changes = context_changes(&self.discovered, discovered, &offered);
        match last_notice(&visible, Notice::ContextChanges) {
            None if *self.discovered == *discovered => {}
            Some(last) if last == changes => {}
            _ => told.push(changes),
        }
        let wanted = plan_mode_notice(plan, &self.allowed_in_plan(toolset));
        let shown = last_plan_notice(&visible);
        if shown.map_or(plan, |shown| shown != wanted) {
            told.push(wanted);
        }
        told
    }

    /// Whether the next prompt of a conversation whose history is `history` starts with the digest of the memories: the agent has its memory and no digest is left in what the model sees, as at the start of a conversation and after a compaction or a fresh context window dropped the earlier one.
    ///
    /// A memory saved later in the conversation therefore reaches the model through the tool that saved it rather than a new digest, and the system prompt never changes for it.
    pub(crate) fn wants_memory(&self, history: &[Message]) -> bool {
        self.key.memory && last_notice(&omc_agent::context(history), Notice::Memory).is_none()
    }

    /// The notice that hands the agent `digest`, pointing only at tools the prefix offers.
    pub(crate) fn memory_notice(&self, digest: &str) -> String {
        let offered: Vec<String> = self.specs.iter().map(|spec| spec.name.to_owned()).collect();
        memory_notice(digest, &offered)
    }
}

/// The latest notice of `kind` in the reader's messages of `messages`.
fn last_notice(messages: &[Message], kind: Notice) -> Option<&str> {
    messages.iter().rev().find_map(|message| match message {
        Message::User(text) => kind.find_last(text),
        _ => None,
    })
}

/// The latest plan mode notice in `messages`, or nothing when there is none or the latest word on plan mode is an approved plan, which ended it.
fn last_plan_notice(messages: &[Message]) -> Option<&str> {
    for message in messages.iter().rev() {
        match message {
            Message::User(text) => {
                if let Some(notice) = Notice::PlanMode.find_last(text) {
                    return Some(notice);
                }
            }
            Message::ToolResults(results)
                if results
                    .iter()
                    .any(|result| result.name == EXIT_PLAN && !result.is_error) =>
            {
                return None;
            }
            _ => {}
        }
    }
    None
}

/// The name of the tool that ends plan mode when its plan is approved.
const EXIT_PLAN: &str = "exit_plan_mode";

/// What the model is told when it calls `recall` before the history was pruned or compacted, when the agent does not answer the call itself.
const NOTHING_TO_RECALL: &str = "`recall` has nothing to read yet: nothing has been left out of or summarized in this conversation, so all of it is still in your context.";

/// `recall` as the agent offers it once the history has been pruned or compacted.
fn recall_spec() -> Spec {
    let spec = omc_agent::recall_spec();
    Spec {
        name: RECALL,
        description: spec.description,
        schema: spec.schema,
    }
}

/// A tool of the prefix: described as it was when the prefix was made, and run as the tool of the mode that is on now.
///
/// A tool the mode on now lacks asks for no approval and refuses the call, so plan mode never changes anything even though the tools that change things stay on the list. A call is checked when it starts, which is before `exit_plan_mode` in the same batch can end plan mode, since that waits for the reader's answer.
struct Gated {
    /// How the tool is described.
    spec: Spec,
    /// The tool outside plan mode, when there is one.
    full: Option<Arc<dyn Tool>>,
    /// The tool in plan mode, when there is one.
    plan: Option<Arc<dyn Tool>>,
    /// Whether plan mode is on now.
    planning: Planning,
    /// The tools plan mode allows, by name, for the refusal to list.
    allowed: Arc<[String]>,
}

impl Gated {
    /// The tool for the mode on now.
    ///
    /// # Errors
    ///
    /// Returns what to tell the model when that mode has no such tool.
    fn current(&self) -> Result<&Arc<dyn Tool>, String> {
        let planning = (self.planning)();
        let chosen = if planning { &self.plan } else { &self.full };
        chosen.as_ref().ok_or_else(|| self.refusal(planning))
    }

    /// Why the tool cannot be called now, when `planning` says whether plan mode is on.
    fn refusal(&self, planning: bool) -> String {
        let name = self.spec.name;
        if planning && self.full.is_some() {
            format!(
                "`{name}` is not available in plan mode. Until the plan is approved, only these tools may be used: {}. Submit the plan with `{EXIT_PLAN}`.",
                self.allowed
                    .iter()
                    .map(|tool| format!("`{tool}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else if !planning && self.plan.is_some() {
            format!("`{name}` is only available in plan mode, which is off.")
        } else if name == RECALL {
            NOTHING_TO_RECALL.to_owned()
        } else {
            format!("`{name}` is no longer available in this conversation.")
        }
    }
}

#[async_trait]
impl Tool for Gated {
    /// The name the model calls the tool by.
    fn name(&self) -> &'static str {
        self.spec.name
    }

    /// The description the tool had when the prefix was made.
    fn description(&self) -> &str {
        &self.spec.description
    }

    /// The schema the tool had when the prefix was made.
    fn schema(&self) -> Value {
        self.spec.schema.clone()
    }

    /// Whether the tool of the mode on now only looks; a refused tool does nothing.
    fn read_only(&self) -> bool {
        self.current().map_or(true, |tool| tool.read_only())
    }

    /// Whether the tool of the mode on now checks its own arguments; a refused tool is said to, so that the call is refused with the reason rather than for its arguments.
    fn checks_arguments(&self) -> bool {
        self.current().map_or(true, |tool| tool.checks_arguments())
    }

    /// Whether the tool of the mode on now needs approval; a refused tool does not.
    fn needs_approval(&self) -> bool {
        self.current().is_ok_and(|tool| tool.needs_approval())
    }

    /// What the reader approves for the tool of the mode on now; nothing for a refused tool.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        self.current().ok()?.approval_scope(args)
    }

    /// The tool's own one-line description of a call with `args`, whichever mode has it.
    fn describe(&self, args: &Value) -> String {
        self.full
            .as_ref()
            .or(self.plan.as_ref())
            .map_or_else(|| args.to_string(), |tool| tool.describe(args))
    }

    /// The same tool with each mode's tool replaced by its copy that cannot start background runs, when either mode's tool can start them; nothing otherwise.
    fn foreground(&self) -> Option<Arc<dyn Tool>> {
        let copy = |tool: &Option<Arc<dyn Tool>>| tool.as_ref().and_then(|tool| tool.foreground());
        let (full, plan) = (copy(&self.full), copy(&self.plan));
        if full.is_none() && plan.is_none() {
            return None;
        }
        Some(Arc::new(Gated {
            spec: self.spec.clone(),
            full: full.or_else(|| self.full.clone()),
            plan: plan.or_else(|| self.plan.clone()),
            planning: self.planning.clone(),
            allowed: self.allowed.clone(),
        }))
    }

    /// Runs the tool of the mode on now with `args`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the mode on now has no such tool, and otherwise what the tool fails with.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.current().map_err(ToolError::Failed)?.call(args).await
    }

    /// Runs the tool of the mode on now for the call `id` with `args`.
    ///
    /// # Errors
    ///
    /// Same as [`Gated::call`].
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        self.current()
            .map_err(ToolError::Failed)?
            .call_as(id, args)
            .await
    }
}

#[cfg(test)]
/// Tests for the tools of the prefix.
mod tests {
    use serde_json::json;

    use super::*;

    /// A tool that checks its own arguments and has a copy that cannot start background runs, which says it is that copy.
    struct Careful {
        /// Whether this is the copy.
        copy: bool,
    }

    #[async_trait]
    impl Tool for Careful {
        /// Returns `careful`.
        fn name(&self) -> &'static str {
            "careful"
        }

        /// Describes the tool.
        fn description(&self) -> &str {
            "careful"
        }

        /// Takes anything.
        fn schema(&self) -> Value {
            json!({"type": "object"})
        }

        /// Checks its own arguments.
        fn checks_arguments(&self) -> bool {
            true
        }

        /// The copy that cannot start background runs.
        fn foreground(&self) -> Option<Arc<dyn Tool>> {
            Some(Arc::new(Careful { copy: true }))
        }

        /// Says whether it is the copy.
        async fn call(&self, _: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(self.copy.to_string()))
        }
    }

    /// A gated tool for `careful` outside plan mode only, with plan mode on when `planning`.
    fn gated(planning: bool) -> Gated {
        let tool = Careful { copy: false };
        Gated {
            spec: Spec {
                name: tool.name(),
                description: tool.description().to_owned(),
                schema: tool.schema(),
            },
            full: Some(Arc::new(tool)),
            plan: None,
            planning: Arc::new(move || planning),
            allowed: Arc::from(Vec::new()),
        }
    }

    /// Whether the tool checks its own arguments and its copy that cannot start background runs come from the tool of the mode on now; a refused tool leaves its arguments to the refusal.
    #[tokio::test]
    async fn gated_tools_forward_what_the_tool_says_of_itself() {
        let tool = gated(false);
        assert!(tool.checks_arguments());
        let copy = tool.foreground().expect("no copy");
        assert_eq!(copy.call(json!({})).await.unwrap().text, "true");
        let refused = gated(true);
        assert!(refused.checks_arguments());
        assert!(refused.call(json!({})).await.is_err());
    }
}
