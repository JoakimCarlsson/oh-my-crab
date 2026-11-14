//! Tests of long-lived subagents: spawning one, sending it messages, waiting for its replies and killing it.

use std::sync::{Arc, Mutex};

use omc_context::builtin_agents;
use omc_tools::{Jobs, Registry, Tool, ToolError};
use serde_json::json;

use crate::testkit::{Router, deny_all, say, scratch};
use crate::{
    AgentEvent, Crew, EventSink, Nesting, Roster, SubagentState, Task, TaskSettings, Throttle,
    crew_tools,
};

/// The events a test's tools reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A model whose every reply says how many messages the request held and the last line the user sent.
fn counting() -> Arc<Router> {
    Router::new(|seen| {
        say(&format!(
            "{} messages, last: {}",
            seen.messages,
            seen.last_user
                .as_deref()
                .unwrap_or_default()
                .lines()
                .last()
                .unwrap_or_default()
        ))
    })
}

/// The five crew tools over a `task` tool for a scratch folder called `name`, asking `model`, with `roster`; by name, with the log of their events.
fn tools(
    name: &str,
    model: Arc<Router>,
    roster: Arc<Roster>,
) -> (std::collections::HashMap<&'static str, Arc<dyn Tool>>, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Arc::new(Task::new(TaskSettings {
        model,
        models: crate::Models::default(),
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: None,
        nesting: Nesting::new(1, Arc::new(Throttle::new(8))).with_roster(roster),
    }));
    let tools = crew_tools(task, Arc::new(Crew::default()))
        .into_iter()
        .map(|tool| (tool.name(), tool))
        .collect();
    (tools, log)
}

/// The states the roster announced, in order.
fn announced(log: &Log) -> Vec<SubagentState> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Subagent(notice) => Some(notice.state),
            _ => None,
        })
        .collect()
}

/// A spawned subagent replies to its prompt, answers a follow-up knowing its conversation, is listed while alive, and is gone once killed.
#[tokio::test]
async fn spawn_send_wait_kill_round_trip() {
    let roster = Arc::new(Roster::default());
    let (tools, log) = tools("crew-round-trip", counting(), roster.clone());
    let spawned = tools["agent_spawn"]
        .call_as(
            "c1",
            json!({"name": "helper", "agent": "scout", "prompt": "look around"}),
        )
        .await
        .unwrap();
    assert!(spawned.text.contains("as helper"));
    let first = tools["agent_wait"]
        .call(json!({"name": "helper", "timeout": 10}))
        .await
        .unwrap();
    assert_eq!(first.text, "1 messages, last: look around");
    let idle = tools["agent_wait"]
        .call(json!({"name": "helper"}))
        .await
        .unwrap();
    assert!(idle.text.contains("owes no reply"));
    let sent = tools["agent_send"]
        .call_as("c2", json!({"name": "helper", "message": "now X"}))
        .await
        .unwrap();
    assert!(sent.text.contains("owes 1 reply"));
    let second = tools["agent_wait"]
        .call(json!({"name": "helper", "timeout": 10}))
        .await
        .unwrap();
    assert_eq!(second.text, "3 messages, last: now X");
    let listed = tools["agent_list"].call(json!({})).await.unwrap();
    assert!(listed.text.starts_with("- helper: scout, idle, owes 0"));
    let killed = tools["agent_kill"]
        .call(json!({"name": "helper"}))
        .await
        .unwrap();
    assert!(killed.text.starts_with("Ended helper"));
    tokio::task::yield_now().await;
    let gone = tools["agent_wait"].call(json!({"name": "helper"})).await;
    assert!(matches!(gone, Err(ToolError::Failed(_))));
    let empty = tools["agent_list"].call(json!({})).await.unwrap();
    assert!(empty.text.starts_with("No long-lived subagents"));
    assert_eq!(
        announced(&log),
        [SubagentState::Running, SubagentState::Stopped]
    );
}

/// Names must fit and be unique among the live subagents.
#[tokio::test]
async fn bad_and_taken_names_are_refused() {
    let (tools, _) = tools("crew-names", counting(), Arc::new(Roster::default()));
    let bad = tools["agent_spawn"]
        .call(json!({"name": "no spaces", "prompt": "x"}))
        .await;
    assert!(matches!(bad, Err(ToolError::InvalidArguments(_))));
    tools["agent_spawn"]
        .call(json!({"name": "a", "agent": "scout", "prompt": "x"}))
        .await
        .unwrap();
    let taken = tools["agent_spawn"]
        .call(json!({"name": "a", "agent": "scout", "prompt": "y"}))
        .await;
    assert!(matches!(taken, Err(ToolError::Failed(_))));
    let unknown = tools["agent_send"]
        .call(json!({"name": "b", "message": "hi"}))
        .await;
    assert!(matches!(unknown, Err(ToolError::Failed(_))));
}
