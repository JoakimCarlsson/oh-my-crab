//! Tests of subagents asking models of their own and starting subagents of their own.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_context::{AgentDefinition, Origin, builtin_agents};
use omc_providers::{ChatModel, Effort};
use omc_tools::{Jobs, Registry, Tool, ToolError};
use serde_json::json;

use crate::testkit::{RequestSeen, Router, call, deny_all, say, scratch};
use crate::{
    AgentEvent, EventSink, ModelPicker, Models, Nesting, Picked, Task, TaskSettings, Throttle,
};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A picker that knows one other model, `small-1`, by the alias `small` or its id.
struct Small {
    /// The model it hands out.
    model: Arc<Router>,
}

#[async_trait]
impl ModelPicker for Small {
    /// Hands out the small model for `small` or `small-1`, and fails for anything else.
    async fn pick(&self, wanted: &str) -> Result<Picked, String> {
        match wanted {
            "small" | "small-1" => Ok(Picked {
                model: self.model.clone(),
                name: "small-1".to_owned(),
                effort: Some(Effort::Low),
            }),
            other => Err(format!("no model matching {other}; models are: small-1")),
        }
    }
}

/// A `task` tool over the built-in tools of a scratch folder called `name`, starting the `agents` with `model` called `big-1`, finding other models with `picker`, standing at `nesting`; with the log of its events.
///
/// A subagent's system prompt is `SUB:` and its kind, followed by `+task` when it has a `task` tool of its own.
fn tool(
    name: &str,
    model: Arc<dyn ChatModel>,
    agents: Vec<AgentDefinition>,
    picker: Option<Arc<dyn ModelPicker>>,
    nesting: Nesting,
) -> (Task, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Task::new(TaskSettings {
        model,
        models: Models {
            current: "big-1".to_owned(),
            picker,
        },
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents,
        effort: Some(Effort::High),
        prompt: Arc::new(|agent, tools, _, _| {
            let nests = if tools.iter().any(|tool| tool == "task") {
                "+task"
            } else {
                ""
            };
            format!("SUB:{}{nests}", agent.name)
        }),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: None,
        nesting,
    });
    (task, log)
}

/// A definition of the kind `quick`, which names the model `model`.
fn quick(model: &str) -> AgentDefinition {
    AgentDefinition {
        name: "quick".into(),
        description: "answers fast".into(),
        tools: Some(vec!["read".into()]),
        prompt: "Answer.".into(),
        output_schema: None,
        isolated: false,
        model: Some(model.into()),
        path: None,
        origin: Origin::Project,
    }
}

/// The models the `Spawned` events of the log name, wherever they are filed, with the kind each starts.
fn spawned(log: &Log) -> Vec<(String, Option<String>)> {
    /// The spawn `event` is or holds, when it is or holds one.
    fn found(event: &AgentEvent) -> Option<(String, Option<String>)> {
        match event {
            AgentEvent::Spawned { agent, model, .. } => Some((agent.clone(), model.clone())),
            AgentEvent::Nested { event, .. } => found(event),
            _ => None,
        }
    }
    log.lock().unwrap().iter().filter_map(found).collect()
}

/// A definition naming a model runs on it, a call naming one runs on it, a call saying `inherit` runs on the model of the agent starting it whatever the definition names, and so does one where nothing names a model; each spawn says which.
#[tokio::test]
async fn definitions_and_calls_pick_the_model() {
    let big = Router::new(|_| say("big answered"));
    let small = Router::new(|_| say("small answered"));
    let mut agents = builtin_agents();
    agents.push(quick("small"));
    let (task, log) = tool(
        "nesting-models",
        big.clone(),
        agents,
        Some(Arc::new(Small {
            model: small.clone(),
        })),
        Nesting::default(),
    );
    let by_definition = task
        .call_as("t1", json!({"agent": "quick", "task": "x"}))
        .await
        .unwrap();
    assert_eq!(by_definition.text, "small answered");
    let by_call = task
        .call_as("t2", json!({"task": "y", "model": "small-1"}))
        .await
        .unwrap();
    assert_eq!(by_call.text, "small answered");
    let overridden = task
        .call_as(
            "t3",
            json!({"agent": "quick", "task": "z", "model": "inherit"}),
        )
        .await
        .unwrap();
    assert_eq!(overridden.text, "big answered");
    let inherited = task.call_as("t4", json!({"task": "w"})).await.unwrap();
    assert_eq!(inherited.text, "big answered");
    assert_eq!(small.seen.lock().unwrap().len(), 2);
    assert_eq!(big.seen.lock().unwrap().len(), 2);
    assert_eq!(
        spawned(&log),
        vec![
            ("quick".to_owned(), Some("small-1".to_owned())),
            ("task".to_owned(), Some("small-1".to_owned())),
            ("quick".to_owned(), Some("big-1".to_owned())),
            ("task".to_owned(), Some("big-1".to_owned())),
        ]
    );
    assert!(task.description().contains("(model: small): answers fast"));
}

/// A model that cannot be found, or any model where none can be picked, is an error the model reads, and nothing runs.
#[tokio::test]
async fn an_unknown_model_is_an_error_for_the_model() {
    let big = Router::new(|_| say("ran"));
    let small = Router::new(|_| say("ran"));
    let mut agents = builtin_agents();
    agents.push(quick("ghost"));
    let (task, log) = tool(
        "nesting-unknown-model",
        big.clone(),
        agents.clone(),
        Some(Arc::new(Small { model: small })),
        Nesting::default(),
    );
    let by_call = task
        .call_as("t1", json!({"task": "x", "model": "huge"}))
        .await
        .unwrap_err();
    assert!(matches!(by_call, ToolError::Failed(_)));
    let text = by_call.to_string();
    assert!(
        text.contains("the call names the model huge") && text.contains("models are: small-1"),
        "{text}"
    );
    let by_definition = task
        .call_as(
            "t2",
            json!({"tasks": [{"task": "a"}, {"agent": "quick", "task": "b"}]}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(
        by_definition.starts_with("tasks[1]: its definition names the model ghost"),
        "{by_definition}"
    );
    let (unpicking, _) = tool(
        "nesting-no-picker",
        big.clone(),
        agents,
        None,
        Nesting::default(),
    );
    let refused = unpicking
        .call_as("t3", json!({"task": "x", "model": "small"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("cannot be given a model of their own"),
        "{refused}"
    );
    assert!(big.seen.lock().unwrap().is_empty());
    assert!(log.lock().unwrap().is_empty());
}

/// Whether `seen` is a request of the subagent whose assignment holds `marker`.
fn of(seen: &RequestSeen, marker: &str) -> bool {
    seen.system.starts_with("SUB:")
        && seen
            .last_user
            .as_deref()
            .is_some_and(|text| text.contains(marker))
}

/// A subagent starts one of its own, which asks the model of its parent and gets no `task` tool at the deepest level; its work is filed under its parent's call, and its result comes back through its parent.
#[tokio::test]
async fn subagents_nest_down_to_the_deepest_level() {
    let model = Router::new(|seen| {
        if of(seen, "level two") {
            say("two done")
        } else if of(seen, "level one") && seen.messages == 1 {
            call("s1", "task", json!({"agent": "scout", "task": "level two"}))
        } else {
            say("one done")
        }
    });
    let mut agents = builtin_agents();
    agents[1].tools.as_mut().unwrap().push("task".into());
    let (task, log) = tool(
        "nesting-depth",
        model.clone(),
        agents,
        None,
        Nesting::new(2, Arc::new(Throttle::new(4))),
    );
    let done = task
        .call_as("t1", json!({"task": "level one"}))
        .await
        .unwrap();
    assert_eq!(done.text, "one done");
    let seen = model.seen.lock().unwrap();
    let level_one = seen.iter().find(|seen| of(seen, "level one")).unwrap();
    assert_eq!(level_one.system, "SUB:task+task");
    assert!(level_one.tools.contains(&"task".to_owned()));
    let level_two = seen.iter().find(|seen| of(seen, "level two")).unwrap();
    assert_eq!(level_two.system, "SUB:scout");
    assert!(!level_two.tools.contains(&"task".to_owned()));
    let results: Vec<String> = seen
        .iter()
        .filter_map(|seen| seen.last_user.clone())
        .collect();
    assert!(!results.is_empty());
    drop(seen);
    assert!(log.lock().unwrap().iter().any(|event| matches!(
        event,
        AgentEvent::Nested { parent, agent, event }
            if parent == "t1" && agent == "task" && matches!(
                event.as_ref(),
                AgentEvent::Spawned { id, agent, model }
                    if id == "s1" && agent == "scout" && model.as_deref() == Some("big-1")
            )
    )));
    assert!(
        task.description()
            .contains("A child may start subagents of its own, which cannot start more.")
    );
}

/// With one level allowed, no subagent gets a `task` tool, and the tool says children cannot start subagents.
#[tokio::test]
async fn one_level_allows_no_nesting() {
    let model = Router::new(|_| say("done"));
    let (task, _) = tool(
        "nesting-one-level",
        model.clone(),
        builtin_agents(),
        None,
        Nesting::new(1, Arc::new(Throttle::new(4))),
    );
    task.call_as("t1", json!({"task": "x"})).await.unwrap();
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen[0].system, "SUB:task");
    assert!(!seen[0].tools.contains(&"task".to_owned()));
    assert!(
        task.description()
            .contains("cannot ask the user or start subagents of their own")
    );
}

/// A subagent cannot start subagents in the background; the model is told so.
#[tokio::test]
async fn a_subagent_cannot_start_background_subagents() {
    let model = Router::new(|seen| {
        if of(seen, "parent") && seen.messages == 1 {
            call("s1", "task", json!({"task": "child", "background": true}))
        } else {
            say("ok")
        }
    });
    let (task, log) = tool(
        "nesting-background",
        model.clone(),
        builtin_agents(),
        None,
        Nesting::default(),
    );
    task.call_as("t1", json!({"task": "parent"})).await.unwrap();
    assert!(log.lock().unwrap().iter().any(|event| matches!(
        event,
        AgentEvent::Nested { event, .. } if matches!(
            event.as_ref(),
            AgentEvent::ToolDone { is_error: true, text, .. } if text.contains("cannot start subagents in the background")
        )
    )));
    assert!(
        !model
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|seen| of(seen, "child"))
    );
}

/// With a single seat for the whole tree, a subagent waiting for its own two subagents lends them its seat, so all of them finish and the seat is free again afterwards.
#[tokio::test]
async fn one_seat_runs_a_whole_tree_without_stalling() {
    let model = Router::new(|seen| {
        if of(seen, "parent") && seen.messages == 1 {
            call(
                "s1",
                "task",
                json!({"tasks": [{"task": "child a"}, {"task": "child b"}]}),
            )
        } else if of(seen, "parent") {
            say("parent done")
        } else {
            say("child done")
        }
    });
    let throttle = Arc::new(Throttle::new(1));
    let (task, _) = tool(
        "nesting-one-seat",
        model.clone(),
        builtin_agents(),
        None,
        Nesting::new(2, throttle.clone()),
    );
    let done = tokio::time::timeout(
        Duration::from_secs(20),
        task.call_as(
            "t1",
            json!({"tasks": [{"task": "parent one"}, {"task": "parent two"}]}),
        ),
    )
    .await
    .expect("the tree stalled")
    .unwrap();
    assert!(
        done.text.starts_with("2 of 2 subagents completed."),
        "{}",
        done.text
    );
    assert_eq!(
        model
            .seen
            .lock()
            .unwrap()
            .iter()
            .filter(|seen| of(seen, "child"))
            .count(),
        4
    );
    assert_eq!(throttle.free(), 1);
}
