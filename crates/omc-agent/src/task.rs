//! The `task` tool: runs a subagent in a loop of its own and returns what it said last.

use std::sync::Arc;

use async_trait::async_trait;
use omc_context::{AgentDefinition, render, subagent_assignment};
use omc_providers::{ChatModel, Effort, Message};
use omc_tools::{Registry, Tool, ToolError, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{AgentEvent, Approvals, Context, run_turn};

/// The tools a subagent never gets: starting subagents of its own, and the ones that talk to the reader about the main agent's work.
const WITHHELD: &[&str] = &["task", "exit_plan_mode", "ask", "todo"];

/// The kind of subagent used when the call names none.
const DEFAULT_AGENT: &str = "task";

/// How the prompt of a subagent is built from its role and the names of its tools.
pub type PromptBuilder = dyn Fn(&AgentDefinition, &[String]) -> String + Send + Sync;

/// Where the events of a run go.
pub type EventSink = dyn Fn(AgentEvent) + Send + Sync;

/// What the `task` tool needs to run subagents.
pub struct TaskSettings {
    /// The model the subagents ask.
    pub model: Arc<dyn ChatModel>,
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
}

/// Arguments of a `task` call.
#[derive(Deserialize)]
struct Args {
    /// The kind of subagent.
    agent: Option<String>,
    /// What it is to do.
    task: String,
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
                format!("- `{}`{only_looks}: {}", agent.name, agent.description)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let description = render(
            include_str!("prompts/task.md"),
            &[("agents", agents.as_str())],
        );
        Self {
            settings,
            description,
        }
    }

    /// The tools `agent` gets: those of the registry the tool was given, less what is withheld, and only those the definition lists when it lists any.
    fn tools_for(&self, agent: &AgentDefinition) -> Registry {
        self.settings.registry.filtered(|tool| {
            !WITHHELD.contains(&tool.name())
                && agent
                    .tools
                    .as_ref()
                    .is_none_or(|listed| listed.iter().any(|name| name == tool.name()))
        })
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
        json!({
            "type": "object",
            "properties": {
                "agent": { "type": "string", "description": "The kind of subagent; omit for the default." },
                "task": { "type": "string", "description": "The self-contained assignment." },
                "description": { "type": "string", "description": "A few words naming the work, shown to the user." }
            },
            "required": ["task"]
        })
    }

    /// Describes a call by its description, or the start of its assignment.
    fn describe(&self, args: &Value) -> String {
        let named = args["description"].as_str().filter(|text| !text.is_empty());
        let said = named.or_else(|| args["task"].as_str()).unwrap_or_default();
        said.lines()
            .next()
            .unwrap_or_default()
            .chars()
            .take(120)
            .collect()
    }

    /// Runs the call without an id, which only loses the grouping of the subagent's work.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Runs the subagent the call asks for and returns its last message; its tool activity is reported under the call `id`.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let kind = args.agent.as_deref().unwrap_or(DEFAULT_AGENT);
        let agent = self
            .settings
            .agents
            .iter()
            .find(|agent| agent.name == kind)
            .ok_or_else(|| {
                let known: Vec<&str> = self
                    .settings
                    .agents
                    .iter()
                    .map(|a| a.name.as_str())
                    .collect();
                ToolError::Failed(format!(
                    "unknown agent {kind}; available: {}",
                    known.join(", ")
                ))
            })?;
        let registry = self.tools_for(agent);
        let system = (self.settings.prompt)(agent, &registry.names());
        let events = self.settings.events.clone();
        (events)(AgentEvent::Spawned {
            id: id.to_owned(),
            agent: agent.name.clone(),
        });
        let parent = id.to_owned();
        let name = agent.name.clone();
        let nested = move |event: AgentEvent| {
            if matches!(
                event,
                AgentEvent::Delta(_) | AgentEvent::Thought(_) | AgentEvent::Used(_)
            ) {
                return;
            }
            events(AgentEvent::Nested {
                parent: parent.clone(),
                agent: name.clone(),
                event: Box::new(event),
            });
        };
        let mut history = Vec::new();
        let context = Context {
            model: self.settings.model.as_ref(),
            registry: &registry,
            approvals: &self.settings.approvals,
            system: &system,
            effort: self.settings.effort,
            refresh: None,
        };
        run_turn(
            &context,
            &mut history,
            subagent_assignment(&args.task),
            &nested,
        )
        .await
        .map_err(|error| ToolError::Failed(format!("the {kind} agent failed: {error}")))?;
        let reply = last_reply(&history);
        if reply.is_empty() {
            Ok(ToolOutput::new("The agent finished without a reply."))
        } else {
            Ok(ToolOutput::new(reply))
        }
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
            model,
            registry: Arc::new(base),
            approvals: deny_all(),
            events,
            agents: builtin_agents(),
            effort: None,
            prompt: Arc::new(|agent, _| format!("SUB:{}", agent.name)),
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
            agent: "scout".into()
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

    /// A scout only gets the read-only tools it lists, and no subagent can start subagents.
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
        assert_eq!(tools_of("SUB:scout"), vec!["glob", "grep", "read"]);
        assert_eq!(
            tools_of("SUB:task"),
            vec!["bash", "edit", "glob", "grep", "read", "write"]
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
        /// Calls `task` twice for the main agent, and hangs for the subagents.
        async fn complete(
            &self,
            request: omc_providers::Request<'_>,
            _sink: &mut dyn omc_providers::Sink,
        ) -> Result<omc_providers::Reply, omc_providers::ProviderError> {
            if request.system == "MAIN" {
                let mut reply = call("t1", "task", json!({"task": "a"}));
                reply.tool_calls.push(omc_providers::ToolCall {
                    id: "t2".into(),
                    name: "task".into(),
                    arguments: json!({"task": "b"}),
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
            model: model.clone(),
            registry: Arc::new(base),
            approvals: deny_all(),
            events: Arc::new(|_| {}),
            agents: builtin_agents(),
            effort: None,
            prompt: Arc::new(|agent, _| format!("SUB:{}", agent.name)),
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
