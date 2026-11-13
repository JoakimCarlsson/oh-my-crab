//! Tests of sending a follow-up to a finished subagent that is kept with its conversation.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use omc_context::builtin_agents;
use omc_tools::{Jobs, Registry, Tool, ToolError};
use serde_json::json;

use crate::testkit::{Router, deny_all, say, scratch};
use crate::{AgentEvent, EventSink, Nesting, Roster, SubagentState, Task, TaskSettings, Throttle};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A model whose every reply says how many messages the request held and what the last one from the user was.
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

/// A `task` tool over the built-in tools of a scratch folder called `name`, whose subagents ask `model` and are put on `roster`; with the log of its events.
fn tool(name: &str, model: Arc<Router>, roster: Arc<Roster>) -> (Task, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Task::new(TaskSettings {
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
    });
    (task, log)
}

/// The id and the state of every subagent announcement in `log`, in order.
fn announced(log: &Log) -> Vec<(String, SubagentState)> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Subagent(notice) => Some((notice.id.clone(), notice.state)),
            _ => None,
        })
        .collect()
}

/// A finished subagent's result names its handle, and a follow-up sent to that handle goes on from its conversation, is announced under the new call, and leaves it kept under the same handle.
#[tokio::test]
async fn a_follow_up_goes_on_from_the_conversation() {
    let roster = Arc::new(Roster::default());
    let model = counting();
    let (task, log) = tool("follow-up-basic", model.clone(), roster.clone());
    let first = task
        .call_as("t1", json!({"agent": "scout", "task": "look around"}))
        .await
        .unwrap();
    assert_eq!(
        first.text,
        "1 messages, last: look around\n\n(Subagent kept as sub-1; `continue: \"sub-1\"` sends it a follow-up.)"
    );
    assert_eq!(roster.kept(), 1);
    let second = task
        .call_as("t2", json!({"continue": "sub-1", "task": "now also fix X"}))
        .await
        .unwrap();
    assert!(
        second
            .text
            .starts_with("3 messages, last: now also fix X\n\n(Subagent kept as sub-1;")
    );
    let third = task
        .call_as("t3", json!({"continue": "sub-1", "task": "and Y"}))
        .await
        .unwrap();
    assert!(third.text.starts_with("5 messages, last: and Y"));
    assert_eq!(roster.kept(), 1);
    assert!(
        model
            .seen
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.system == "SUB:scout")
    );
    assert_eq!(
        announced(&log),
        [
            ("t1".to_owned(), SubagentState::Running),
            ("t1".to_owned(), SubagentState::Finished),
            ("t2".to_owned(), SubagentState::Running),
            ("t2".to_owned(), SubagentState::Finished),
            ("t3".to_owned(), SubagentState::Running),
            ("t3".to_owned(), SubagentState::Finished),
        ]
    );
}

/// A handle that names no kept subagent fails with advice to start a new one, and arguments that only fit a new subagent are refused without taking the kept one.
#[tokio::test]
async fn unknown_handles_and_stray_arguments_are_refused() {
    let roster = Arc::new(Roster::default());
    let (task, _) = tool("follow-up-refused", counting(), roster.clone());
    task.call_as("t1", json!({"task": "work"})).await.unwrap();
    let unknown = task
        .call_as("t2", json!({"continue": "sub-9", "task": "more"}))
        .await
        .unwrap_err();
    assert!(matches!(&unknown, ToolError::Failed(text) if text.contains("Start a new subagent")));
    let stray = task
        .call_as(
            "t3",
            json!({"continue": "sub-1", "task": "more", "agent": "scout"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(&stray, ToolError::InvalidArguments(text) if text.contains("`agent`")));
    let empty = task
        .call_as("t4", json!({"continue": "sub-1"}))
        .await
        .unwrap_err();
    assert!(matches!(empty, ToolError::InvalidArguments(_)));
    assert_eq!(roster.kept(), 1);
}

/// Only so many finished subagents are kept: the one kept longest goes, and a follow-up to it fails while one to the newest works.
#[tokio::test]
async fn the_longest_kept_goes_when_there_are_too_many() {
    let roster = Arc::new(Roster::keeping(1, Duration::from_secs(3600)));
    let (task, _) = tool("follow-up-count", counting(), roster.clone());
    task.call_as("t1", json!({"tasks": [{"task": "a"}, {"task": "b"}]}))
        .await
        .unwrap();
    assert_eq!(roster.kept(), 1);
    let done = task.call_as("t2", json!({"task": "c"})).await.unwrap();
    assert!(done.text.contains("kept as sub-3"));
    assert!(
        task.call_as("t3", json!({"continue": "sub-1", "task": "more"}))
            .await
            .is_err()
    );
    assert!(
        task.call_as("t4", json!({"continue": "sub-3", "task": "more"}))
            .await
            .is_ok()
    );
}

/// A finished subagent kept too long is gone, and one never kept has no handle in its result.
#[tokio::test]
async fn subagents_kept_too_long_are_gone() {
    let never = Arc::new(Roster::keeping(8, Duration::ZERO));
    let (task, _) = tool("follow-up-never", counting(), never.clone());
    let done = task.call_as("t1", json!({"task": "a"})).await.unwrap();
    assert_eq!(done.text, "1 messages, last: a");
    assert_eq!(never.kept(), 0);
    let briefly = Arc::new(Roster::keeping(8, Duration::from_millis(40)));
    let (task, _) = tool("follow-up-age", counting(), briefly.clone());
    task.call_as("t1", json!({"task": "a"})).await.unwrap();
    assert_eq!(briefly.kept(), 1);
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(
        task.call_as("t2", json!({"continue": "sub-1", "task": "more"}))
            .await
            .is_err()
    );
    assert_eq!(briefly.kept(), 0);
}

/// A subagent asked for a structured result is not kept, so that its result stays the data it handed back.
#[tokio::test]
async fn structured_results_are_not_kept() {
    let roster = Arc::new(Roster::default());
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            crate::testkit::call("s1", "submit_result", json!({"result": {"n": 1}}))
        } else {
            say("handed in")
        }
    });
    let (task, _) = tool("follow-up-structured", model, roster.clone());
    let schema = json!({"type": "object", "properties": {"n": {"type": "integer"}}});
    let done = task
        .call_as("t1", json!({"task": "count", "output_schema": schema}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&done.text).unwrap(),
        json!({"n": 1})
    );
    assert_eq!(roster.kept(), 0);
}
