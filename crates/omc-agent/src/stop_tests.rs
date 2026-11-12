//! Tests of stopping one subagent by its id while its siblings and the turn go on.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_context::builtin_agents;
use omc_providers::{ChatModel, Message, ProviderError, Reply, Request, Sink};
use omc_tools::{Jobs, Registry, Tool};
use serde_json::json;

use crate::testkit::{Router, call, deny_all, say, scratch};
use crate::{
    AgentEvent, EventSink, Nesting, Roster, SubagentNotice, SubagentState, Task, TaskSettings,
    Throttle,
};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A model that never answers a subagent whose assignment holds `hang`, fails every request of one whose assignment holds `boom`, and leaves the rest to a [`Router`].
struct Hanging {
    /// Answers the requests that do not hang.
    router: Arc<Router>,
}

#[async_trait]
impl ChatModel for Hanging {
    /// Hangs for the requests whose first message holds `hang`, fails those whose first message holds `boom`, and passes the rest on.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        if matches!(request.messages.first(), Some(Message::User(text)) if text.contains("hang")) {
            return std::future::pending().await;
        }
        if matches!(request.messages.first(), Some(Message::User(text)) if text.contains("boom")) {
            return Err(ProviderError::NoModels);
        }
        self.router.complete(request, sink).await
    }
}

/// A `task` tool over the built-in tools of a scratch folder called `name`, whose subagents ask `model`, the `task` kind of which may start subagents of its own, putting its subagents on `roster` and starting background runs in `jobs`; with the log of its events.
fn tool(
    name: &str,
    model: Arc<dyn ChatModel>,
    roster: Arc<Roster>,
    jobs: Arc<Jobs>,
) -> (Task, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let mut agents = builtin_agents();
    agents[1].tools.as_mut().unwrap().push("task".into());
    let task = Task::new(TaskSettings {
        model,
        models: crate::Models::default(),
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents,
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs,
        isolation: None,
        hooks: None,
        nesting: Nesting::new(2, Arc::new(Throttle::new(8))).with_roster(roster),
    });
    (task, log)
}

/// Every subagent announcement in `log`, nested ones included, with the `task` call id their nesting gives them, as a client sees it.
fn notices(log: &Log) -> Vec<SubagentNotice> {
    /// The announcement `event` holds, under the calls that wrap it.
    fn unwrap(event: &AgentEvent) -> Option<SubagentNotice> {
        match event {
            AgentEvent::Subagent(notice) => Some(notice.clone()),
            AgentEvent::Nested { parent, event, .. } => {
                unwrap(event).map(|notice| SubagentNotice {
                    call: format!("{parent}:{}", notice.call),
                    ..notice
                })
            }
            _ => None,
        }
    }
    log.lock().unwrap().iter().filter_map(unwrap).collect()
}

/// The states the subagent `id` was announced in, in order.
fn states(log: &Log, id: &str) -> Vec<SubagentState> {
    notices(log)
        .into_iter()
        .filter(|notice| notice.id == id)
        .map(|notice| notice.state)
        .collect()
}

/// Waits until the subagent `id` is on `roster`.
async fn running(roster: &Roster, id: &str) {
    while !roster.running(id) {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// Stopping one subagent of a batch of three ends it; the other two finish, and the result holds their outputs and a note that the reader stopped the third.
#[tokio::test]
async fn stopping_one_of_a_batch_leaves_the_others_be() {
    let roster = Arc::new(Roster::default());
    let model = Arc::new(Hanging {
        router: Router::new(|_| say("done")),
    });
    let (task, log) = tool(
        "stop-batch",
        model,
        roster.clone(),
        Arc::new(Jobs::new(|_| {})),
    );
    let call = tokio::spawn(async move {
        task.call_as(
            "t1",
            json!({"tasks": [{"task": "a"}, {"task": "b"}, {"task": "hang"}]}),
        )
        .await
    });
    running(&roster, "t1/3").await;
    assert!(roster.stop("t1/3"));
    let out = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(out.text.starts_with("2 of 3 subagents completed."));
    assert!(out.text.contains("## 1. task: a (completed)\ndone"));
    assert!(out.text.contains("## 2. task: b (completed)\ndone"));
    assert!(out.text.contains("## 3. task: hang (failed)\n"));
    assert!(out.text.contains("stopped by the reader"));
    assert_eq!(
        states(&log, "t1/1"),
        [SubagentState::Running, SubagentState::Finished]
    );
    assert_eq!(
        states(&log, "t1/3"),
        [SubagentState::Running, SubagentState::Stopped]
    );
    assert!(
        notices(&log)
            .iter()
            .all(|notice| notice.call == "t1" && notice.agent == "task" && !notice.background)
    );
    assert!(!roster.stop("t1/3"));
    assert!(!roster.stop("t1/1"));
}

/// Stopping a background subagent makes `wait` report it as failed with the note, while the other background run goes on.
#[tokio::test]
async fn stopping_a_background_subagent_leaves_the_others_running() {
    let roster = Arc::new(Roster::default());
    let jobs = Arc::new(Jobs::new(|_| {}));
    let model = Arc::new(Hanging {
        router: Router::new(|_| say("done")),
    });
    let (task, log) = tool("stop-background", model, roster.clone(), jobs.clone());
    task.call_as(
        "t1",
        json!({"background": true, "tasks": [{"task": "hang one"}, {"task": "hang two"}]}),
    )
    .await
    .unwrap();
    assert!(roster.stop("t1/1"));
    let finished = tokio::time::timeout(Duration::from_secs(10), jobs.wait())
        .await
        .unwrap()
        .unwrap();
    assert!(!finished.ok);
    assert_eq!(finished.id, "bg-1");
    assert!(finished.text.contains("stopped by the reader"));
    assert!(roster.running("t1/2"));
    assert_eq!(
        states(&log, "t1/1"),
        [SubagentState::Running, SubagentState::Stopped]
    );
    assert_eq!(states(&log, "t1/2"), [SubagentState::Running]);
    assert!(notices(&log).iter().all(|notice| notice.background));
    jobs.stop_all();
}

/// Stopping a subagent that started subagents of its own ends them too, each announced under an id holding its parent's.
#[tokio::test]
async fn stopping_a_subagent_stops_its_children() {
    let roster = Arc::new(Roster::default());
    let model = Arc::new(Hanging {
        router: Router::new(|seen| match seen.messages {
            1 => call(
                "s1",
                "task",
                json!({"agent": "scout", "task": "hang below"}),
            ),
            _ => say("parent done"),
        }),
    });
    let (task, log) = tool(
        "stop-nested",
        model,
        roster.clone(),
        Arc::new(Jobs::new(|_| {})),
    );
    let call = tokio::spawn(async move { task.call_as("t1", json!({"task": "parent"})).await });
    running(&roster, "t1:s1").await;
    assert!(roster.stop("t1"));
    let failed = tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(failed.to_string().contains("stopped by the reader"));
    assert!(!roster.running("t1:s1"));
    assert_eq!(
        states(&log, "t1"),
        [SubagentState::Running, SubagentState::Stopped]
    );
    assert_eq!(
        states(&log, "t1:s1"),
        [SubagentState::Running, SubagentState::Stopped]
    );
    let child = notices(&log)
        .into_iter()
        .find(|notice| notice.id == "t1:s1")
        .unwrap();
    assert_eq!(child.call, "t1:s1");
    assert_eq!(child.agent, "scout");
}

/// A subagent that runs to the end in the foreground is announced as finished, and one that fails as failed, under the same id as when it started.
#[tokio::test]
async fn a_lone_subagent_is_announced_when_it_starts_and_ends() {
    let roster = Arc::new(Roster::default());
    let model = Arc::new(Hanging {
        router: Router::new(|_| say("done")),
    });
    let (task, log) = tool(
        "stop-lone",
        model,
        roster.clone(),
        Arc::new(Jobs::new(|_| {})),
    );
    task.call_as("t1", json!({"task": "a"})).await.unwrap();
    task.call_as("t2", json!({"task": "boom"}))
        .await
        .unwrap_err();
    assert_eq!(
        states(&log, "t1"),
        [SubagentState::Running, SubagentState::Finished]
    );
    assert_eq!(
        states(&log, "t2"),
        [SubagentState::Running, SubagentState::Failed]
    );
    assert!(!roster.running("t1"));
}
