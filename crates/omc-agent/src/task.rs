//! The `task` tool: runs subagents, alone or several at once, each in a loop of its own, and returns what they said last or the structured result they handed back.
//!
//! A subagent may work apart from the main agent, in a git worktree of its own; what it changed is applied to the main agent's working tree when it ends, as [`crate::worktree`] tells.
//!
//! A subagent asks the model of the agent that starts it unless its definition or the call names another, which [`crate::picker`] finds. A subagent above the deepest level allowed gets a `task` tool of its own, and every subagent at every level takes a seat of one shared throttle to run, as [`crate::nesting`] tells.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use omc_context::{AgentDefinition, render, subagent_assignment};
use omc_providers::{ChatModel, Effort, Message};
use omc_tools::schema::check_schema;
use omc_tools::{Concurrency, Jobs, Registry, Tool, ToolError, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::nesting::{Nesting, Seat};
use crate::picker::Models;
use crate::submit::{SUBMIT_RESULT, Slot, SubmitResult};
use crate::worktree::{self, Handback, Isolation, Lease, Merge};
use crate::{AgentError, AgentEvent, Approvals, Context, Hooks, run_turn};

/// The tools a subagent never gets from the agent that starts it: that agent's own `task` tool, since a subagent that may start subagents gets one of its own, and the ones that talk to the reader about the main agent's work.
const WITHHELD: &[&str] = &[
    "task",
    "exit_plan_mode",
    "ask",
    "todo",
    "context_notes",
    "new_context",
    "checkpoint",
    "rewind",
    "wait",
];

/// The kind of subagent used when the call names none.
const DEFAULT_AGENT: &str = "task";

/// The most subagents one call may start.
pub const MAX_TASKS: usize = 32;

/// How many subagents run at once by default, across every call and level of a conversation; the rest wait for one of them to end or to wait for subagents of its own.
pub const MAX_CONCURRENT: usize = 32;

/// The names under which an agent definition's tool list lets its subagents start subagents of their own: crab's `task`, and `agent`, as Claude Code calls its tool.
const NESTING_TOOLS: &[&str] = &["task", "agent"];

/// What a subagent asked for a structured result is told about handing it back.
const STRUCTURED: &str = include_str!("prompts/structured.md");

/// What a subagent asked for a structured result is told when it ends without handing one back.
const NUDGE: &str = include_str!("prompts/submit_nudge.md");

/// What an isolated subagent is told about the worktree it works in.
const ISOLATED: &str = include_str!("prompts/isolated.md");

/// How the prompt of a subagent is built from its role, the names of its tools, the folder it works in when it works in a worktree of its own, and what the model it asks is called, empty when that is not known.
pub type PromptBuilder =
    dyn Fn(&AgentDefinition, &[String], Option<&Path>, &str) -> String + Send + Sync;

/// Where the events of a run go.
pub type EventSink = dyn Fn(AgentEvent) + Send + Sync;

/// What the `task` tool needs to run subagents.
pub struct TaskSettings {
    /// The model the subagents ask when neither their definition nor the call names another.
    pub model: Arc<dyn ChatModel>,
    /// What that model is called, and how other models are found by name.
    pub models: Models,
    /// The tools subagents may draw from.
    pub registry: Arc<Registry>,
    /// What is allowed without asking, shared with the main agent.
    pub approvals: Arc<Approvals>,
    /// Where the work of subagents is reported, under the call that started them.
    pub events: Arc<EventSink>,
    /// The kinds of subagent that may be started.
    pub agents: Vec<AgentDefinition>,
    /// How hard the subagents are asked to think.
    pub effort: Option<Effort>,
    /// Builds the system prompt of a subagent.
    pub prompt: Arc<PromptBuilder>,
    /// Where subagents started in the background run.
    pub jobs: Arc<Jobs>,
    /// How subagents are given worktrees of their own, or nothing when they cannot be, which refuses every call asking for one.
    pub isolation: Option<Arc<Isolation>>,
    /// The checks the user set up around tool calls and the end of a turn, which subagents run as the main agent does, each under its own id and kind.
    pub hooks: Option<Arc<dyn Hooks>>,
    /// The level the subagents run at, how deep they may go, and the seats they take to run.
    pub nesting: Nesting,
}

/// Arguments of a `task` call.
#[derive(Deserialize)]
struct Args {
    /// The kind of subagent, for the single assignment or for the items of `tasks` that name none.
    agent: Option<String>,
    /// What a single subagent is to do.
    task: Option<String>,
    /// A few words naming the single assignment.
    description: Option<String>,
    /// The schema of the single subagent's result, or of the results of the items of `tasks` that give none.
    output_schema: Option<Value>,
    /// Several assignments, run at once.
    tasks: Option<Vec<Item>>,
    /// Whether to start the subagents in the background and return a handle for each.
    #[serde(default)]
    background: bool,
    /// Whether the subagents work in git worktrees of their own, for the single assignment or the items of `tasks` that do not say; their kind's definition decides when nothing does.
    isolated: Option<bool>,
    /// The model the subagents ask, for the single assignment or the items of `tasks` that name none; their kind's definition decides when nothing does.
    model: Option<String>,
}

/// One assignment of a `task` call.
#[derive(Deserialize)]
struct Item {
    /// The kind of subagent.
    agent: Option<String>,
    /// What it is to do.
    task: String,
    /// A few words naming the work.
    description: Option<String>,
    /// The schema its result must match, when it is to hand back data.
    output_schema: Option<Value>,
    /// Whether it works in a git worktree of its own.
    isolated: Option<bool>,
    /// The model it asks.
    model: Option<String>,
}

impl Args {
    /// The assignments the call asks for: the single `task`, or the items of `tasks` with the call's `agent`, `output_schema`, `isolated` and `model` filled in where they give none.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the call gives both forms or neither, or more than [`MAX_TASKS`] items.
    fn items(self) -> Result<Vec<Item>, ToolError> {
        let Args {
            agent,
            task,
            description,
            output_schema,
            tasks,
            isolated,
            model,
            ..
        } = self;
        let invalid = |reason: String| Err(ToolError::InvalidArguments(reason));
        match (task, tasks) {
            (Some(_), Some(_)) => invalid("give either `task` or `tasks`, not both".to_owned()),
            (None, None) => invalid("missing `task`, or `tasks` for several".to_owned()),
            (Some(task), None) => Ok(vec![Item {
                agent,
                task,
                description,
                output_schema,
                isolated,
                model,
            }]),
            (None, Some(list)) if list.is_empty() => invalid("`tasks` is empty".to_owned()),
            (None, Some(list)) if list.len() > MAX_TASKS => invalid(format!(
                "`tasks` holds {} assignments; one call runs at most {MAX_TASKS}",
                list.len()
            )),
            (None, Some(list)) => Ok(list
                .into_iter()
                .map(|item| Item {
                    agent: item.agent.or_else(|| agent.clone()),
                    output_schema: item.output_schema.or_else(|| output_schema.clone()),
                    isolated: item.isolated.or(isolated),
                    model: item.model.or_else(|| model.clone()),
                    ..item
                })
                .collect()),
        }
    }
}

/// Where the work of one subagent is reported.
#[derive(Clone)]
struct Lane {
    /// The id of the `task` call that started it.
    call: String,
    /// Its place in the call's list, counted from one, when the call started several.
    place: Option<usize>,
    /// The kind of subagent.
    kind: String,
    /// What it is doing, in a few words.
    label: String,
    /// What the model it asks is called; empty when it is not known.
    model: String,
}

impl Lane {
    /// The event announcing the subagent: the call itself starting it, with the model it asks when that is known, when it is alone, or a step of the call when it is one of several.
    fn opened(&self) -> AgentEvent {
        match self.place {
            None => AgentEvent::Spawned {
                id: self.call.clone(),
                agent: self.kind.clone(),
                model: Some(self.model.clone()).filter(|model| !model.is_empty()),
            },
            Some(place) => self.under_call(AgentEvent::ToolStart {
                id: place.to_string(),
                name: "task".to_owned(),
                summary: self.label.clone(),
            }),
        }
    }

    /// The event ending the step of a subagent that is one of several with `outcome`; nothing for a lone one, whose call reports its own end.
    fn closed(&self, outcome: &Result<String, ToolError>) -> Option<AgentEvent> {
        let place = self.place?;
        let (text, is_error) = match outcome {
            Ok(text) => (text.clone(), false),
            Err(error) => (error.to_string(), true),
        };
        Some(self.under_call(AgentEvent::ToolDone {
            id: place.to_string(),
            name: "task".to_owned(),
            text,
            is_error,
            changes: Vec::new(),
        }))
    }

    /// `event`, something the subagent did, filed under it: under the call when it is alone, and under its step of the call when it is one of several.
    fn file(&self, event: AgentEvent) -> AgentEvent {
        match self.place {
            None => self.under_call(event),
            Some(place) => self.under_call(AgentEvent::Nested {
                parent: place.to_string(),
                agent: self.kind.clone(),
                event: Box::new(event),
            }),
        }
    }

    /// The event saying the subagent works in the worktree at `path`, and, once it ended, what became of its changes: about the call itself when it is alone, and about its step of the call when it is one of several.
    fn worktree(&self, path: PathBuf, merge: Option<Merge>) -> AgentEvent {
        match self.place {
            None => AgentEvent::Worktree {
                id: self.call.clone(),
                path,
                merge,
            },
            Some(place) => self.under_call(AgentEvent::Worktree {
                id: place.to_string(),
                path,
                merge,
            }),
        }
    }

    /// `event` filed under the call.
    fn under_call(&self, event: AgentEvent) -> AgentEvent {
        AgentEvent::Nested {
            parent: self.call.clone(),
            agent: self.kind.clone(),
            event: Box::new(event),
        }
    }

    /// The heading of the subagent's part of a combined result, which ended `ok` or not.
    fn heading(&self, ok: bool) -> String {
        let status = if ok { "completed" } else { "failed" };
        format!(
            "## {}. {}: {} ({status})",
            self.place.unwrap_or(1),
            self.kind,
            self.label
        )
    }
}

/// The schema a subagent's result must match, and where the result it hands back is kept.
struct Structured {
    /// The schema.
    schema: Value,
    /// Where `submit_result` keeps what it accepted.
    slot: Slot,
}

impl Structured {
    /// The result accepted so far, as the text the main agent reads.
    fn accepted(&self) -> Option<String> {
        self.slot.lock().value.as_ref().map(pretty)
    }

    /// The failure of a `kind` subagent that ended, even after a reminder, without an accepted result, with `history` as its conversation.
    fn missing(&self, kind: &str, history: &[Message]) -> ToolError {
        let why = match &self.slot.lock().rejected {
            Some(reasons) => format!("its last result did not match the schema:\n{reasons}"),
            None => match last_reply(history) {
                said if said.is_empty() => "it never called submit_result".to_owned(),
                said => format!("it never called submit_result; its last message was:\n{said}"),
            },
        };
        ToolError::Failed(format!(
            "the {kind} agent ended without a result matching output_schema, even after a reminder; {why}"
        ))
    }
}

/// `value` as indented JSON.
fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// `opening`, the first message of a subagent, with the `context` the hooks gave when it started placed before it, in a `<subagent-start-hook>` block; `opening` as it is when they gave none.
fn with_start_context(context: Option<String>, opening: String) -> String {
    match context {
        Some(context) => format!(
            "<subagent-start-hook>\n{}\n</subagent-start-hook>\n\n{opening}",
            context.trim()
        ),
        None => opening,
    }
}

/// The failure of a `kind` subagent whose turn failed with `error`.
fn failed(kind: &str, error: &AgentError) -> ToolError {
    ToolError::Failed(format!("the {kind} agent failed: {error}"))
}

/// A subagent, owning everything it needs so that it can run on a task of its own.
struct Run {
    /// The model it asks.
    model: Arc<dyn ChatModel>,
    /// The tools it has.
    registry: Registry,
    /// What is allowed without asking.
    approvals: Arc<Approvals>,
    /// Where its work is reported.
    events: Arc<EventSink>,
    /// How its work is filed.
    lane: Lane,
    /// Its system prompt.
    system: String,
    /// How hard it thinks.
    effort: Option<Effort>,
    /// What it is to do.
    assignment: String,
    /// The schema its result must match, when it is to hand back data.
    structured: Option<Structured>,
    /// The kind of subagent it is.
    definition: AgentDefinition,
    /// Whether it is to work in a git worktree of its own.
    isolated: bool,
    /// The worktree it works in, once it was given one.
    lease: Option<Lease>,
    /// The checks the user set up, as they apply to this subagent.
    hooks: Option<Arc<dyn Hooks>>,
    /// Its id among the subagents of the conversation, which its hooks and the subagents it starts are known by.
    id: String,
    /// Whether it gets a `task` tool of its own.
    nests: bool,
    /// The seat it holds while it runs.
    seat: Arc<Seat>,
}

impl Run {
    /// Has the subagent work in the worktree `lease`, made from `snapshot`: its tools that touch files are replaced by ones built by `isolation` for the worktree, and its assignment says where it works.
    ///
    /// # Errors
    ///
    /// Returns what building the tools fails with; the worktree is removed then.
    fn enter(
        &mut self,
        lease: Lease,
        snapshot: &worktree::Snapshot,
        isolation: &Isolation,
    ) -> Result<(), ToolError> {
        let directory = lease.directory();
        let own = (isolation.tools)(&directory)?;
        for tool in own.iter() {
            if self.registry.get(tool.name()).is_some() {
                self.registry.register_shared(tool.clone());
            }
        }
        let shown = directory.display().to_string();
        let submodules = snapshot.submodules.join(", ");
        self.assignment = format!(
            "{}\n\n{}",
            self.assignment,
            render(
                ISOLATED,
                &[
                    ("directory", shown.as_str()),
                    ("submodules", submodules.as_str()),
                    ("cloned", if lease.cloned() { "yes" } else { "" })
                ]
            )
        );
        self.lease = Some(lease);
        Ok(())
    }

    /// Runs the subagent to the end and returns what it said last, or the result it handed back when it was asked for one; one that ends without handing back a result is reminded once.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when a model request fails, or when a subagent asked for a result ends twice without one that matches the schema.
    async fn go(self) -> Result<String, ToolError> {
        let Run {
            model,
            registry,
            approvals,
            events,
            lane,
            system,
            effort,
            assignment,
            structured,
            hooks,
            ..
        } = self;
        let kind = lane.kind.clone();
        let report = move |event: AgentEvent| {
            if matches!(
                event,
                AgentEvent::Started(_)
                    | AgentEvent::Delta(_)
                    | AgentEvent::Thought(_)
                    | AgentEvent::Used(_)
            ) {
                return;
            }
            events(lane.file(event));
        };
        let context = Context {
            model: model.as_ref(),
            registry: &registry,
            approvals: &approvals,
            system: &system,
            effort,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: None,
            hooks: hooks.as_deref(),
        };
        let mut history = Vec::new();
        let opening = match &structured {
            Some(wanted) => format!(
                "{}\n\n{}",
                subagent_assignment(&assignment),
                render(STRUCTURED, &[("schema", pretty(&wanted.schema).as_str())])
            ),
            None => subagent_assignment(&assignment),
        };
        let opening = match &hooks {
            Some(hooks) => with_start_context(hooks.subagent_start().await, opening),
            None => opening,
        };
        run_turn(&context, &mut history, opening, &report)
            .await
            .map_err(|error| failed(&kind, &error))?;
        let Some(wanted) = structured else {
            let reply = last_reply(&history);
            return Ok(if reply.is_empty() {
                "The agent finished without a reply.".to_owned()
            } else {
                reply
            });
        };
        if let Some(result) = wanted.accepted() {
            return Ok(result);
        }
        run_turn(&context, &mut history, NUDGE.trim().to_owned(), &report)
            .await
            .map_err(|error| failed(&kind, &error))?;
        wanted
            .accepted()
            .ok_or_else(|| wanted.missing(&kind, &history))
    }
}

/// How a subagent ended, before what it changed in a worktree of its own is applied.
struct Ended {
    /// Where its work was reported.
    lane: Lane,
    /// Its result, or why it failed.
    outcome: Result<String, ToolError>,
    /// What it changed, when it worked in a worktree of its own.
    handback: Option<Handback>,
}

/// Runs `run` once it took a seat of `nesting`'s throttle, which it holds until it ends, takes what it changed from its worktree when it has one, reports the end of its step through `events` when it is one of several, and says how it ended.
async fn finish(mut run: Run, events: Arc<EventSink>, nesting: Nesting) -> Ended {
    let lane = run.lane.clone();
    let lease = run.lease.take();
    let seat = run.seat.clone();
    seat.fill(nesting.throttle.take().await);
    let outcome = run.go().await;
    seat.vacate();
    let handback = match lease {
        Some(lease) => Some(lease.hand_back().await),
        None => None,
    };
    if let Some(event) = lane.closed(&outcome) {
        events(event);
    }
    Ended {
        lane,
        outcome,
        handback,
    }
}

/// The lane and the outcome of the subagent that `ended`, after what it changed in a worktree of its own was applied to the main agent's working tree as `isolation` says, or saved when it could not be; what became of the changes is reported through `events` and added to the outcome.
async fn conclude(
    ended: Ended,
    isolation: Option<&Isolation>,
    events: &EventSink,
) -> (Lane, Result<String, ToolError>) {
    let Ended {
        lane,
        outcome,
        handback,
    } = ended;
    let (Some(handback), Some(isolation)) = (handback, isolation) else {
        return (lane, outcome);
    };
    let path = handback.directory.clone();
    let merge = handback.settle(outcome.is_ok(), &isolation.patches).await;
    let note = merge.note();
    events(lane.worktree(path, Some(merge)));
    let outcome = match outcome {
        Ok(text) => Ok(format!("{text}\n\n# Changes\n{note}")),
        Err(error) => Err(ToolError::Failed(format!("{error}\n\n# Changes\n{note}"))),
    };
    (lane, outcome)
}

/// A few words naming an assignment: its `description`, or the start of its `task`.
fn headline(description: Option<&str>, task: &str) -> String {
    description
        .filter(|text| !text.trim().is_empty())
        .unwrap_or(task)
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(120)
        .collect()
}

/// Starts subagents.
pub struct Task {
    /// What it runs them with.
    settings: TaskSettings,
    /// What the model is told it does.
    description: String,
}

impl Task {
    /// The tool, running subagents as `settings` says.
    pub fn new(settings: TaskSettings) -> Self {
        let agents = settings
            .agents
            .iter()
            .map(|agent| {
                let only_looks = if agent.read_only() {
                    " (READ-ONLY; investigation only, no edits)"
                } else {
                    ""
                };
                let structured = if agent.output_schema.is_some() {
                    " (returns structured JSON)"
                } else {
                    ""
                };
                let isolated = if agent.isolated { " (isolated)" } else { "" };
                let model = agent
                    .model
                    .as_ref()
                    .map(|model| format!(" (model: {model})"))
                    .unwrap_or_default();
                format!(
                    "- `{}`{only_looks}{structured}{isolated}{model}: {}",
                    agent.name, agent.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let description = render(
            include_str!("prompts/task.md"),
            &[
                ("agents", agents.as_str()),
                ("most", MAX_TASKS.to_string().as_str()),
                ("parallel", MAX_CONCURRENT.to_string().as_str()),
                ("nesting", nesting_note(&settings.nesting).as_str()),
            ],
        );
        Self {
            settings,
            description,
        }
    }

    /// The tools `agent` gets: those of the registry the tool was given, less what is withheld, and only those the definition lists when it lists any; a tool that depends on what else the agent has, such as `script`, is fitted to them.
    fn tools_for(&self, agent: &AgentDefinition) -> Registry {
        self.settings
            .registry
            .filtered(|tool| {
                !WITHHELD.contains(&tool.name())
                    && agent
                        .tools
                        .as_ref()
                        .is_none_or(|listed| listed.iter().any(|name| name == tool.name()))
            })
            .fitted()
    }

    /// Whether a subagent of the kind `agent` started by this tool gets a `task` tool of its own: it runs above the deepest level allowed, and its definition lists no tools or lists `task` or `agent` among them.
    fn nests(&self, agent: &AgentDefinition) -> bool {
        self.settings.nesting.nests()
            && agent.tools.as_ref().is_none_or(|listed| {
                listed
                    .iter()
                    .any(|name| NESTING_TOOLS.contains(&name.as_str()))
            })
    }

    /// The definition of the subagent kind `kind`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`], listing the kinds there are, when there is no such kind.
    fn agent(&self, kind: &str) -> Result<&AgentDefinition, ToolError> {
        self.settings
            .agents
            .iter()
            .find(|agent| agent.name == kind)
            .ok_or_else(|| {
                let known: Vec<&str> = self
                    .settings
                    .agents
                    .iter()
                    .map(|agent| agent.name.as_str())
                    .collect();
                ToolError::Failed(format!(
                    "unknown agent {kind}; available: {}",
                    known.join(", ")
                ))
            })
    }

    /// The model a subagent of the kind `agent` asks, what it is called and how hard it thinks: the one `wanted` by the call names, else the one its definition names, else the one of the agent starting it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`], saying who named it and why it cannot be used, when the model named is not one there is, or no other model can be picked here.
    async fn model_for(
        &self,
        agent: &AgentDefinition,
        wanted: Option<&str>,
    ) -> Result<(Arc<dyn ChatModel>, String, Option<Effort>), ToolError> {
        let named = wanted
            .map(|name| (name, "the call"))
            .or_else(|| agent.model.as_deref().map(|name| (name, "its definition")))
            .map(|(name, by)| (name.trim(), by))
            .filter(|(name, _)| !name.is_empty() && !name.eq_ignore_ascii_case("inherit"));
        let inherited = || {
            (
                self.settings.model.clone(),
                self.settings.models.current.clone(),
                self.settings.effort,
            )
        };
        let Some((name, by)) = named else {
            return Ok(inherited());
        };
        let Some(picker) = &self.settings.models.picker else {
            return Err(ToolError::Failed(format!(
                "{by} names the model {name} for the {} agent, but subagents cannot be given a model of their own here; leave out `model`",
                agent.name
            )));
        };
        let picked = picker.pick(name).await.map_err(|why| {
            ToolError::Failed(format!(
                "{by} names the model {name} for the {} agent, which cannot be used: {why}",
                agent.name
            ))
        })?;
        Ok((picked.model, picked.name, picked.effort))
    }

    /// The subagent for `item`, reporting under the call `call` at `place` in its list when it is one of several.
    ///
    /// A subagent asked for a structured result, by the item or by its kind's definition, gets the `submit_result` tool for it. Its system prompt, and the `task` tool of its own when it gets one, are only made by [`Task::arm`], once it is known where it works.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] for an unknown kind or a model that cannot be used, and [`ToolError::InvalidArguments`] for a schema that is not one.
    async fn prepare(
        &self,
        call: &str,
        place: Option<usize>,
        item: Item,
    ) -> Result<Run, ToolError> {
        let agent = self.agent(item.agent.as_deref().unwrap_or(DEFAULT_AGENT))?;
        let schema = item.output_schema.or_else(|| agent.output_schema.clone());
        if let Some(schema) = &schema {
            check_schema(schema).map_err(|reason| {
                ToolError::InvalidArguments(format!("output_schema: {reason}"))
            })?;
        }
        let (model, model_name, effort) = self.model_for(agent, item.model.as_deref()).await?;
        let mut registry = self.tools_for(agent);
        let structured = schema.map(|schema| {
            let slot = Slot::default();
            registry.register(SubmitResult::new(schema.clone(), slot.clone()));
            Structured { schema, slot }
        });
        let id = self.settings.nesting.id_of(call, place);
        let hooks = self
            .settings
            .hooks
            .as_ref()
            .map(|hooks| hooks.for_subagent(&id, &agent.name));
        Ok(Run {
            model,
            registry,
            approvals: self.settings.approvals.clone(),
            events: self.settings.events.clone(),
            lane: Lane {
                call: call.to_owned(),
                place,
                kind: agent.name.clone(),
                label: headline(item.description.as_deref(), &item.task),
                model: model_name,
            },
            system: String::new(),
            effort,
            assignment: item.task,
            structured,
            isolated: item.isolated.unwrap_or(agent.isolated),
            definition: agent.clone(),
            lease: None,
            hooks,
            id,
            nests: self.nests(agent),
            seat: Arc::new(Seat::default()),
        })
    }

    /// `runs`, with a worktree of its own given to each that is to work in one, all made from one snapshot of the working tree of the agent starting them.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`], saying why, when a worktree cannot be made: isolation is not available, the folder is not in a git repository, git is missing or fails. The worktrees made before are removed again.
    async fn isolate(&self, runs: Vec<Run>) -> Result<Vec<Run>, ToolError> {
        if !runs.iter().any(|run| run.isolated) {
            return Ok(runs);
        }
        let Some(isolation) = &self.settings.isolation else {
            return Err(ToolError::Failed(
                "subagents cannot work in worktrees of their own here; run them without `isolated`"
                    .to_owned(),
            ));
        };
        let snapshot = worktree::snapshot(isolation)
            .await
            .map_err(ToolError::Failed)?;
        let mut given = Vec::with_capacity(runs.len());
        for mut run in runs {
            if run.isolated {
                let lease = worktree::lease(isolation, &snapshot)
                    .await
                    .map_err(ToolError::Failed)?;
                run.enter(lease, &snapshot, isolation)?;
            }
            given.push(run);
        }
        Ok(given)
    }

    /// Readies `run` to start, now that it is known where it works: a subagent that may start subagents of its own gets a `task` tool one level down, drawing on its own tools, asking its model by default, reporting under its lane, and isolating its subagents from the worktree it works in, when it works in one, so that what they change is handed back to it; then its system prompt is made for the tools it has, the folder it works in and the model it asks.
    fn arm(&self, run: &mut Run) {
        let directory = run.lease.as_ref().map(Lease::directory);
        if run.nests {
            let pool = run.registry.filtered(|tool| tool.name() != SUBMIT_RESULT);
            let (events, lane) = (self.settings.events.clone(), run.lane.clone());
            let filed: Arc<EventSink> = Arc::new(move |event| events(lane.file(event)));
            let isolation = match (&directory, &self.settings.isolation) {
                (Some(directory), Some(isolation)) => Some(Arc::new(Isolation {
                    directory: directory.clone(),
                    store: isolation.store.clone(),
                    patches: isolation.patches.clone(),
                    tools: isolation.tools.clone(),
                    backend: isolation.backend,
                })),
                _ => self.settings.isolation.clone(),
            };
            run.registry.register(Task::new(TaskSettings {
                model: run.model.clone(),
                models: self.settings.models.named(&run.lane.model),
                registry: Arc::new(pool),
                approvals: self.settings.approvals.clone(),
                events: filed,
                agents: self.settings.agents.clone(),
                effort: run.effort,
                prompt: self.settings.prompt.clone(),
                jobs: self.settings.jobs.clone(),
                isolation,
                hooks: run.hooks.clone(),
                nesting: self.settings.nesting.below(run.seat.clone(), &run.id),
            }));
            run.registry = run.registry.fitted();
        }
        run.system = (self.settings.prompt)(
            &run.definition,
            &run.registry.names(),
            directory.as_deref(),
            &run.lane.model,
        );
    }

    /// Runs `runs` together, each once it took a seat of the shared throttle, with the seat of the subagent this tool belongs to given up meanwhile, then applies what those in worktrees of their own changed in the order given, and returns each one's lane and outcome in that order.
    async fn run_together(&self, runs: Vec<Run>) -> Vec<(Lane, Result<String, ToolError>)> {
        let events = &self.settings.events;
        let nesting = &self.settings.nesting;
        let ended: Vec<Ended> = nesting
            .lending(
                stream::iter(
                    runs.into_iter()
                        .map(|run| finish(run, events.clone(), nesting.clone())),
                )
                .buffered(MAX_TASKS)
                .collect(),
            )
            .await;
        let mut outcomes = Vec::with_capacity(ended.len());
        for ended in ended {
            outcomes
                .push(conclude(ended, self.settings.isolation.as_deref(), events.as_ref()).await);
        }
        outcomes
    }

    /// Starts `runs`, started by the call `call`, in the background, each once it took a seat of the shared throttle, and says which handle each got.
    fn start_in_background(&self, call: &str, runs: Vec<Run>) -> ToolOutput {
        let call = Some(call.to_owned()).filter(|call| !call.is_empty());
        let started: Vec<(String, Lane)> = runs
            .into_iter()
            .map(|run| {
                let lane = run.lane.clone();
                let events = self.settings.events.clone();
                let isolation = self.settings.isolation.clone();
                let nesting = self.settings.nesting.clone();
                let handle =
                    self.settings
                        .jobs
                        .spawn(lane.label.clone(), call.clone(), async move {
                            let ended = finish(run, events.clone(), nesting).await;
                            conclude(ended, isolation.as_deref(), events.as_ref())
                                .await
                                .1
                                .map_err(|error| error.to_string())
                        });
                (handle, lane)
            })
            .collect();
        if let [(handle, lane)] = started.as_slice()
            && lane.place.is_none()
        {
            return ToolOutput::new(format!(
                "Started the {} agent in the background as {handle}. Call `wait` to get its result when it ends.",
                lane.kind
            ));
        }
        let handles: Vec<String> = started
            .iter()
            .map(|(handle, lane)| format!("- {handle}: {}: {}", lane.kind, lane.label))
            .collect();
        ToolOutput::new(format!(
            "Started {} subagents in the background:\n{}\nCall `wait` once per handle to get each result as it ends.",
            started.len(),
            handles.join("\n")
        ))
    }
}

/// What the agent with the `task` tool standing at `nesting` is told about whether the subagents it starts can start subagents of their own.
fn nesting_note(nesting: &Nesting) -> String {
    let below = nesting.most.saturating_sub(nesting.depth);
    match below {
        0 => "Children start blank and cannot ask the user or start subagents of their own; give them everything they need.".to_owned(),
        1 => "Children start blank and cannot ask the user; give them everything they need. A child may start subagents of its own, which cannot start more.".to_owned(),
        levels => format!(
            "Children start blank and cannot ask the user; give them everything they need. A child may start subagents of its own, down to {levels} levels below yours."
        ),
    }
}

/// The text of the last reply in `history`.
fn last_reply(history: &[Message]) -> String {
    history
        .iter()
        .rev()
        .find_map(|message| match message {
            Message::Assistant(reply) => Some(reply.text.trim().to_owned()),
            _ => None,
        })
        .unwrap_or_default()
}

/// The combined result of several subagents, from each one's lane and outcome: every outcome under a heading, failing as a whole only when all of them failed.
fn combine(outcomes: Vec<(Lane, Result<String, ToolError>)>) -> Result<ToolOutput, ToolError> {
    let total = outcomes.len();
    let completed = outcomes
        .iter()
        .filter(|(_, outcome)| outcome.is_ok())
        .count();
    let parts: Vec<String> = outcomes
        .into_iter()
        .map(|(lane, outcome)| {
            let ok = outcome.is_ok();
            let text = outcome.unwrap_or_else(|error| error.to_string());
            format!("{}\n{text}", lane.heading(ok))
        })
        .collect();
    let report = format!(
        "{completed} of {total} subagents completed.\n\n{}",
        parts.join("\n\n")
    );
    if completed == 0 {
        Err(ToolError::Failed(report))
    } else {
        Ok(ToolOutput::new(report))
    }
}

#[async_trait]
impl Tool for Task {
    /// Returns `task`.
    fn name(&self) -> &'static str {
        "task"
    }

    /// Describes the tool and the agents it can start.
    fn description(&self) -> &str {
        &self.description
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        let agent = json!({ "type": "string", "description": "The kind of subagent; omit for the default." });
        let description = json!({ "type": "string", "description": "A few words naming the work, shown to the user." });
        let output_schema = json!({
            "type": "object",
            "description": "A JSON Schema the result must match. The subagent hands its result to `submit_result`, and you get the checked JSON back instead of prose."
        });
        let isolated = json!({
            "type": "boolean",
            "description": "Run the subagent in a copy of your working tree of its own (a copy-on-write clone, or a git worktree); its changes are applied to yours as a patch when it ends."
        });
        let model = json!({
            "type": "string",
            "description": "The model the subagent asks instead of its kind's or yours: an alias (`haiku`, `sonnet`, `opus`, `inherit`), a model id, or `provider/model` for another signed-in provider."
        });
        json!({
            "type": "object",
            "properties": {
                "agent": agent,
                "task": { "type": "string", "description": "The self-contained assignment of a single subagent." },
                "description": description,
                "output_schema": output_schema,
                "tasks": {
                    "type": "array",
                    "description": "Several independent assignments, run in parallel; give this instead of `task`. Call-level `agent`, `output_schema`, `isolated` and `model` apply to items that omit them.",
                    "maxItems": MAX_TASKS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "agent": agent,
                            "task": { "type": "string", "description": "The self-contained assignment." },
                            "description": description,
                            "output_schema": output_schema,
                            "isolated": isolated,
                            "model": model
                        },
                        "required": ["task"]
                    }
                },
                "background": { "type": "boolean", "description": "Start the subagents in the background and return a handle for each at once; `wait` returns their results." },
                "isolated": isolated,
                "model": model
            }
        })
    }

    /// Shared when every subagent the call with `args` starts only looks: all the tools subagents are given only look, as in plan mode, or the definition of every kind it names lists only tools that look. Exclusive otherwise, and for arguments that cannot be read, since a subagent that can write changes files as any writing tool does.
    fn concurrency(&self, args: &Value) -> Concurrency {
        if self.settings.registry.iter().all(|tool| tool.read_only()) {
            return Concurrency::Shared;
        }
        let looks = serde_json::from_value::<Args>(args.clone())
            .ok()
            .and_then(|args| args.items().ok())
            .is_some_and(|items| {
                items.iter().all(|item| {
                    self.agent(item.agent.as_deref().unwrap_or(DEFAULT_AGENT))
                        .is_ok_and(AgentDefinition::read_only)
                })
            });
        if looks {
            Concurrency::Shared
        } else {
            Concurrency::Exclusive
        }
    }

    /// Describes a call by its description, or the start of its assignment; a call starting several names how many and what they do.
    fn describe(&self, args: &Value) -> String {
        let named = |item: &Value| {
            headline(
                item["description"].as_str(),
                item["task"].as_str().unwrap_or_default(),
            )
        };
        match args["tasks"].as_array() {
            Some(list) if list.len() > 1 => {
                let labels: Vec<String> = list.iter().map(named).collect();
                format!("{} subagents: {}", list.len(), labels.join(", "))
                    .chars()
                    .take(120)
                    .collect()
            }
            Some(list) if !list.is_empty() => named(&list[0]),
            _ => named(args),
        }
    }

    /// Runs the call without an id, which only loses the grouping of the subagents' work.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Runs the subagents the call asks for, their tool activity reported under the call `id`, and returns what they came to.
    ///
    /// Every assignment is checked, its model found, and every worktree asked for made, before any subagent starts. A single subagent's result is its last message or its structured result; several run at once and their outcomes are combined under headings, one failing leaving the others be. A subagent in a worktree of its own has what it changed applied to the working tree of the agent that started it when it ends, several in the order of the list, and its result says what became of it. In the background, each gets a handle for `wait` and the call returns at once; only the main agent can start subagents in the background.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for arguments that do not fit the schema or a subagent asking for the background, and [`ToolError::Failed`] when an assignment cannot be prepared, a worktree cannot be made, or every subagent failed.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let background = args.background;
        if background && self.settings.nesting.seat.is_some() {
            return Err(ToolError::InvalidArguments(
                "a subagent cannot start subagents in the background; leave out `background`"
                    .to_owned(),
            ));
        }
        let items = args.items()?;
        let several = items.len() > 1;
        let mut runs = Vec::with_capacity(items.len());
        for (index, item) in items.into_iter().enumerate() {
            let place = several.then_some(index + 1);
            let run = self
                .prepare(id, place, item)
                .await
                .map_err(|error| match place {
                    Some(_) => ToolError::Failed(format!("tasks[{index}]: {error}")),
                    None => error,
                })?;
            runs.push(run);
        }
        let mut runs = self.isolate(runs).await?;
        for run in &mut runs {
            self.arm(run);
        }
        for run in &runs {
            (self.settings.events)(run.lane.opened());
            if let Some(lease) = &run.lease {
                (self.settings.events)(run.lane.worktree(lease.directory(), None));
            }
        }
        if background {
            return Ok(self.start_in_background(id, runs));
        }
        let mut outcomes = self.run_together(runs).await;
        if several {
            return combine(outcomes);
        }
        outcomes.pop().map_or_else(
            || Err(ToolError::Failed("nothing to run".to_owned())),
            |(_, outcome)| outcome.map(ToolOutput::new),
        )
    }
}

#[cfg(test)]
/// Tests for the `task` tool.
mod tests {
    use std::sync::Mutex;

    use omc_context::builtin_agents;
    use omc_providers::Message;
    use omc_tools::Registry;
    use serde_json::json;

    use super::*;
    use crate::testkit::{Router, call, deny_all, read_args, say, scratch};

    /// Whether a request comes from a subagent, going by the prompt the tests give them.
    fn is_sub(system: &str) -> bool {
        system.starts_with("SUB:")
    }

    /// Builds a main registry with `read`, `grep`, `write` and a `task` tool over them that logs events into `log`.
    fn setup(
        name: &str,
        model: Arc<Router>,
        log: Arc<Mutex<Vec<AgentEvent>>>,
    ) -> (Registry, std::path::PathBuf) {
        let dir = scratch(name);
        std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        let base = Registry::builtin(&dir).unwrap();
        let events: Arc<EventSink> = Arc::new(move |event| log.lock().unwrap().push(event));
        let mut registry = base.clone();
        registry.register(Task::new(TaskSettings {
            models: crate::Models::default(),
            nesting: crate::Nesting::default(),
            model,
            registry: Arc::new(base),
            approvals: deny_all(),
            events,
            agents: builtin_agents(),
            effort: None,
            prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
            hooks: None,
            isolation: None,
            jobs: Arc::new(Jobs::new(|_| {})),
        }));
        (registry, dir)
    }

    /// Runs one turn of `prompt` on `registry` against `model`.
    async fn drive(
        model: &Router,
        registry: &Registry,
        log: &Arc<Mutex<Vec<AgentEvent>>>,
        prompt: &str,
    ) -> Vec<Message> {
        let mut history = Vec::new();
        let sink = log.clone();
        run_turn(
            &Context {
                model,
                registry,
                approvals: &deny_all(),
                system: "MAIN",
                effort: None,
                refresh: None,
                notebook: None,
                window: None,
                checkpoint: None,
                hooks: None,
            },
            &mut history,
            prompt.into(),
            &move |event| sink.lock().unwrap().push(event),
        )
        .await
        .unwrap();
        history
    }

    /// The subagent reads a file, its work is filed under the task call, and its last words are the result.
    #[tokio::test]
    async fn runs_a_subagent_and_folds_its_events() {
        let model = Router::new(|seen| {
            if is_sub(&seen.system) {
                if seen.messages == 1 {
                    call("s1", "read", read_args("a.txt"))
                } else {
                    say("it holds alpha")
                }
            } else if seen.messages == 1 {
                call(
                    "t1",
                    "task",
                    json!({"agent": "scout", "task": "look at a.txt"}),
                )
            } else {
                say("all done")
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let (registry, _) = setup("task-basic", model.clone(), log.clone());
        let history = drive(&model, &registry, &log, "go").await;
        let Message::ToolResults(results) = &history[2] else {
            panic!("no results");
        };
        assert_eq!(results[0].text, "it holds alpha");
        assert!(!results[0].is_error);
        let log = log.lock().unwrap();
        assert!(log.contains(&AgentEvent::Spawned {
            id: "t1".into(),
            agent: "scout".into(),
            model: None
        }));
        let nested: Vec<_> = log
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Nested {
                    parent,
                    agent,
                    event,
                } => Some((parent.as_str(), agent.as_str(), event)),
                _ => None,
            })
            .collect();
        assert_eq!(nested.len(), 2);
        assert!(
            nested
                .iter()
                .all(|(parent, agent, _)| *parent == "t1" && *agent == "scout")
        );
        assert!(
            matches!(nested[0].2.as_ref(), AgentEvent::ToolStart { name, .. } if name == "read")
        );
        assert!(matches!(
            nested[1].2.as_ref(),
            AgentEvent::ToolDone {
                is_error: false,
                ..
            }
        ));
        assert!(
            !log.iter()
                .any(|event| matches!(event, AgentEvent::Delta(text) if text == "it holds alpha"))
        );
    }

    /// A scout only gets the read-only tools it lists, and the default kind, which lists none, gets a `task` tool of its own one level down.
    #[tokio::test]
    async fn subagents_get_restricted_tools() {
        let model = Router::new(|seen| {
            if is_sub(&seen.system) {
                say("ok")
            } else if seen.messages == 1 {
                let mut reply = call("t1", "task", json!({"agent": "scout", "task": "x"}));
                reply.tool_calls.push(omc_providers::ToolCall {
                    id: "t2".into(),
                    name: "task".into(),
                    arguments: json!({"task": "y"}),
                });
                reply
            } else {
                say("done")
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let (registry, _) = setup("task-tools", model.clone(), log.clone());
        drive(&model, &registry, &log, "go").await;
        let seen = model.seen.lock().unwrap();
        let tools_of = |system: &str| {
            seen.iter()
                .find(|request| request.system == system)
                .map(|request| request.tools.clone())
                .unwrap()
        };
        assert_eq!(
            tools_of("SUB:scout"),
            vec!["ast_grep", "glob", "grep", "read"]
        );
        assert_eq!(
            tools_of("SUB:task"),
            vec![
                "ast_edit",
                "ast_grep",
                "bash",
                "code_search",
                "edit",
                "glob",
                "grep",
                "lsp",
                "read",
                "task",
                "web_search",
                "write"
            ]
        );
    }

    /// Parallel task calls both run and their results come back in call order.
    #[tokio::test]
    async fn parallel_tasks_all_report() {
        let model = Router::new(|seen| {
            if is_sub(&seen.system) {
                say(&format!(
                    "did {}",
                    seen.last_user
                        .clone()
                        .unwrap_or_default()
                        .lines()
                        .last()
                        .unwrap_or_default()
                ))
            } else if seen.messages == 1 {
                let mut reply = call("t1", "task", json!({"task": "first"}));
                reply.tool_calls.push(omc_providers::ToolCall {
                    id: "t2".into(),
                    name: "task".into(),
                    arguments: json!({"agent": "scout", "task": "second"}),
                });
                reply
            } else {
                say("merged")
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let (registry, _) = setup("task-parallel", model.clone(), log.clone());
        let history = drive(&model, &registry, &log, "go").await;
        let Message::ToolResults(results) = &history[2] else {
            panic!("no results");
        };
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].call_id, "t1");
        assert_eq!(results[0].text, "did first");
        assert_eq!(results[1].text, "did second");
        assert_eq!(history.len(), 4);
    }

    /// An unknown agent is an error the model reads, listing the kinds that exist.
    #[tokio::test]
    async fn unknown_agent_is_an_error() {
        let model = Router::new(|seen| {
            if seen.messages == 1 {
                call("t1", "task", json!({"agent": "ghost", "task": "x"}))
            } else {
                say("fine")
            }
        });
        let log = Arc::new(Mutex::new(Vec::new()));
        let (registry, _) = setup("task-unknown", model.clone(), log.clone());
        let history = drive(&model, &registry, &log, "go").await;
        let Message::ToolResults(results) = &history[2] else {
            panic!("no results");
        };
        assert!(results[0].is_error);
        assert!(results[0].text.contains("scout"));
    }

    /// A background subagent returns a handle at once, its events are filed under the call, and `wait` reports its last words.
    #[tokio::test]
    async fn a_background_subagent_is_reported_through_wait() {
        let model = Router::new(|_| say("found it"));
        let log = Arc::new(Mutex::new(Vec::new()));
        let sink = log.clone();
        let dir = scratch("task-background");
        let jobs = Arc::new(Jobs::new(|_| {}));
        let task = Task::new(TaskSettings {
            models: crate::Models::default(),
            nesting: crate::Nesting::default(),
            model,
            registry: Arc::new(Registry::builtin(&dir).unwrap()),
            approvals: deny_all(),
            events: Arc::new(move |event| sink.lock().unwrap().push(event)),
            agents: builtin_agents(),
            effort: None,
            prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
            hooks: None,
            isolation: None,
            jobs: jobs.clone(),
        });
        let started = task
            .call_as(
                "t1",
                json!({"agent": "scout", "task": "look", "background": true}),
            )
            .await
            .unwrap();
        assert!(started.text.contains("bg-1"));
        let finished = jobs.wait().await.unwrap();
        assert!(finished.ok);
        assert_eq!(finished.text, "found it");
        assert!(matches!(
            log.lock().unwrap().first(),
            Some(AgentEvent::Spawned { id, .. }) if id == "t1"
        ));
        assert!(jobs.wait().await.is_err());
    }

    /// A drop guard that counts how many were dropped.
    struct Guard(Arc<std::sync::atomic::AtomicUsize>);

    impl Drop for Guard {
        /// Counts the drop.
        fn drop(&mut self) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    /// A model whose subagent requests never finish, so that only an abort can end them.
    struct Stuck {
        /// Counts the stuck requests that were dropped.
        dropped: Arc<std::sync::atomic::AtomicUsize>,
        /// Counts the requests that began.
        started: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[async_trait]
    impl ChatModel for Stuck {
        /// Calls `task` twice for the main agent, with scouts, which only look, so that both calls run at once; and hangs for the subagents.
        async fn complete(
            &self,
            request: omc_providers::Request<'_>,
            _sink: &mut dyn omc_providers::Sink,
        ) -> Result<omc_providers::Reply, omc_providers::ProviderError> {
            if request.system == "MAIN" {
                let mut reply = call("t1", "task", json!({"agent": "scout", "task": "a"}));
                reply.tool_calls.push(omc_providers::ToolCall {
                    id: "t2".into(),
                    name: "task".into(),
                    arguments: json!({"agent": "scout", "task": "b"}),
                });
                return Ok(reply);
            }
            let _guard = Guard(self.dropped.clone());
            self.started
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::future::pending().await
        }
    }

    /// Aborting the task that runs a turn ends the subagents it started and leaves the history as it was.
    #[tokio::test]
    async fn aborting_a_turn_aborts_its_subagents() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let dropped = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let model = Arc::new(Stuck {
            dropped: dropped.clone(),
            started: started.clone(),
        });
        let dir = scratch("task-abort");
        let base = Registry::builtin(&dir).unwrap();
        let mut registry = base.clone();
        registry.register(Task::new(TaskSettings {
            models: crate::Models::default(),
            nesting: crate::Nesting::default(),
            model: model.clone(),
            registry: Arc::new(base),
            approvals: deny_all(),
            events: Arc::new(|_| {}),
            agents: builtin_agents(),
            effort: None,
            prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
            hooks: None,
            isolation: None,
            jobs: Arc::new(Jobs::new(|_| {})),
        }));
        let history = Arc::new(Mutex::new(vec![Message::User("earlier".into())]));
        let handle = {
            let (model, history) = (model.clone(), history.clone());
            tokio::spawn(async move {
                let mut mine = history.lock().unwrap().clone();
                let _ = run_turn(
                    &Context {
                        model: model.as_ref(),
                        registry: &registry,
                        approvals: &deny_all(),
                        system: "MAIN",
                        effort: None,
                        refresh: None,
                        notebook: None,
                        window: None,
                        checkpoint: None,
                        hooks: None,
                    },
                    &mut mine,
                    "go".into(),
                    &|_| {},
                )
                .await;
            })
        };
        while started.load(Ordering::SeqCst) < 2 {
            tokio::task::yield_now().await;
        }
        handle.abort();
        let _ = handle.await;
        assert_eq!(dropped.load(Ordering::SeqCst), 2);
        assert_eq!(history.lock().unwrap().len(), 1);
    }
}
