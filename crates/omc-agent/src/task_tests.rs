//! Tests of the `task` tool's batches of subagents and structured results.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_context::{AgentDefinition, Origin, builtin_agents};
use omc_providers::{ChatModel, Message, ProviderError, Reply, Request, Sink, ToolCall};
use omc_tools::{Concurrency, Jobs, Registry, Tool, ToolError};
use serde_json::{Value, json};

use crate::testkit::{RequestSeen, Router, call, deny_all, read_args, said, say, scratch};
use crate::{
    AgentEvent, Context, EventSink, MAX_CONCURRENT, MAX_TASKS, Task, TaskSettings, run_turn,
};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// The schema of a result holding a whole-number `count`.
fn counting() -> Value {
    json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    })
}

/// A `task` tool over the built-in tools in a scratch folder called `name`, which holds `a.txt`, starting the `agents` with `model`, logging its events and starting background runs in `jobs`; with the log and the folder.
fn task_tool(
    name: &str,
    model: Arc<dyn ChatModel>,
    agents: Vec<AgentDefinition>,
    jobs: Arc<Jobs>,
) -> (Task, Log, PathBuf) {
    let dir = scratch(name);
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Task::new(TaskSettings {
        models: crate::Models::default(),
        nesting: crate::Nesting::default(),
        model,
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents,
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        hooks: None,
        jobs,
        isolation: None,
    });
    (task, log, dir)
}

/// A `task` tool like [`task_tool`] with the built-in agents and background runs of its own.
fn simple(name: &str, model: Arc<dyn ChatModel>) -> (Task, Log) {
    let (task, log, _) = task_tool(name, model, builtin_agents(), Arc::new(Jobs::new(|_| {})));
    (task, log)
}

/// The last user message a request carried, or nothing.
fn told(seen: &RequestSeen) -> &str {
    seen.last_user.as_deref().unwrap_or_default()
}

/// The steps of the call `call` that the log holds, as their id, whether they started and whether they failed.
fn steps(log: &Log, call: &str) -> Vec<(String, bool, bool)> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Nested { parent, event, .. } if parent == call => match event.as_ref() {
                AgentEvent::ToolStart { id, .. } => Some((id.clone(), true, false)),
                AgentEvent::ToolDone { id, is_error, .. } => Some((id.clone(), false, *is_error)),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A model that fails every request of a subagent whose assignment holds `boom`, and leaves the rest to a [`Router`].
struct Failing {
    /// Answers the requests that do not fail.
    router: Arc<Router>,
}

#[async_trait]
impl ChatModel for Failing {
    /// Fails the requests whose first message holds `boom`, and passes the rest on.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        if matches!(request.messages.first(), Some(Message::User(text)) if text.contains("boom")) {
            return Err(ProviderError::NoModels);
        }
        self.router.complete(request, sink).await
    }
}

/// Every subagent of a batch runs, each outcome comes back under a heading in call order, and each subagent's work is filed under its own step of the call.
#[tokio::test]
async fn a_batch_runs_every_subagent_and_labels_the_results() {
    let model = Router::new(|seen| match (seen.system.as_str(), seen.messages) {
        ("SUB:scout", 1) => call("s1", "read", read_args("a.txt")),
        ("SUB:scout", _) => say("beta holds alpha"),
        _ => say("did alpha"),
    });
    let (task, log) = simple("task-batch", model);
    let out = task
        .call_as(
            "t1",
            json!({"tasks": [
                {"task": "alpha"},
                {"agent": "scout", "task": "beta", "description": "look for beta"}
            ]}),
        )
        .await
        .unwrap();
    assert!(out.text.starts_with("2 of 2 subagents completed."));
    assert!(
        out.text
            .contains("## 1. task: alpha (completed)\ndid alpha")
    );
    assert!(
        out.text
            .contains("## 2. scout: look for beta (completed)\nbeta holds alpha")
    );
    let mut steps = steps(&log, "t1");
    steps.sort();
    assert_eq!(
        steps,
        vec![
            ("1".into(), false, false),
            ("1".into(), true, false),
            ("2".into(), false, false),
            ("2".into(), true, false)
        ]
    );
    let log = log.lock().unwrap();
    assert!(
        !log.iter()
            .any(|event| matches!(event, AgentEvent::Spawned { .. }))
    );
    assert!(log.iter().any(|event| matches!(
        event,
        AgentEvent::Nested { parent, agent, event }
            if parent == "t1" && agent == "scout" && matches!(
                event.as_ref(),
                AgentEvent::Nested { parent, event, .. }
                    if parent == "2" && matches!(event.as_ref(), AgentEvent::ToolStart { name, .. } if name == "read")
            )
    )));
}

/// A subagent that fails leaves the others to finish, and the call fails only when every one of them did.
#[tokio::test]
async fn one_failure_does_not_sink_the_others() {
    let model = Arc::new(Failing {
        router: Router::new(|_| say("fine")),
    });
    let (task, log) = simple("task-batch-failure", model.clone());
    let out = task
        .call_as("t1", json!({"tasks": [{"task": "boom"}, {"task": "calm"}]}))
        .await
        .unwrap();
    assert!(out.text.starts_with("1 of 2 subagents completed."));
    assert!(
        out.text
            .contains("## 1. task: boom (failed)\nthe task agent failed:")
    );
    assert!(out.text.contains("## 2. task: calm (completed)\nfine"));
    assert!(steps(&log, "t1").contains(&("1".into(), false, true)));
    let all = task
        .call_as(
            "t2",
            json!({"tasks": [{"task": "boom"}, {"task": "boom again"}]}),
        )
        .await
        .unwrap_err();
    assert!(all.to_string().starts_with("0 of 2 subagents completed."));
}

/// A background batch returns a handle for each subagent at once, and `wait` reports each of them.
#[tokio::test]
async fn a_background_batch_yields_a_handle_each() {
    let jobs = Arc::new(Jobs::new(|_| {}));
    let model = Router::new(|seen| {
        say(&format!(
            "done {}",
            told(seen).lines().last().unwrap_or_default()
        ))
    });
    let (task, log, _) = task_tool(
        "task-batch-background",
        model,
        builtin_agents(),
        jobs.clone(),
    );
    let out = task
        .call_as(
            "t1",
            json!({"background": true, "tasks": [{"task": "a"}, {"task": "b"}, {"agent": "scout", "task": "c"}]}),
        )
        .await
        .unwrap();
    assert!(
        out.text
            .starts_with("Started 3 subagents in the background:")
    );
    for handle in ["bg-1: task: a", "bg-2: task: b", "bg-3: scout: c"] {
        assert!(out.text.contains(handle));
    }
    let mut texts = Vec::new();
    for _ in 0..3 {
        let finished = jobs.wait().await.unwrap();
        assert!(finished.ok);
        texts.push(said(&finished.text).to_owned());
    }
    texts.sort();
    assert_eq!(texts, vec!["done a", "done b", "done c"]);
    assert!(jobs.wait().await.is_err());
    assert_eq!(
        steps(&log, "t1")
            .iter()
            .filter(|(_, started, failed)| !started && !failed)
            .count(),
        3
    );
}

/// A subagent asked for a structured result gets `submit_result`, is told the schema, and its accepted value is the result.
#[tokio::test]
async fn a_structured_result_comes_back_as_json() {
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call("s1", "submit_result", json!({"result": {"count": 2}}))
        } else {
            say("submitted")
        }
    });
    let (task, _) = simple("task-structured", model.clone());
    let out = task
        .call_as(
            "t1",
            json!({"agent": "scout", "task": "count", "output_schema": counting()}),
        )
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&out.text).unwrap(),
        json!({"count": 2})
    );
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen[0].tools.contains(&"submit_result".to_owned()));
    assert!(told(&seen[0]).contains("\"count\""));
}

/// A subagent whose result never matches the schema fails with the reasons, after one reminder.
#[tokio::test]
async fn a_result_breaking_the_schema_fails() {
    let model = Router::new(|seen| {
        if seen.messages % 4 == 1 {
            call("s1", "submit_result", json!({"result": {"count": "two"}}))
        } else {
            say("done")
        }
    });
    let (task, _) = simple("task-structured-invalid", model.clone());
    let error = task
        .call_as("t1", json!({"task": "count", "output_schema": counting()}))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("did not match the schema"));
    assert!(text.contains("result.count: expected integer, got string"));
    assert_eq!(model.seen.lock().unwrap().len(), 4);
}

/// A subagent that ends without submitting is reminded once, and fails when it still does not.
#[tokio::test]
async fn a_missing_result_is_nudged_once_then_fails() {
    let model = Router::new(|_| say("here is prose"));
    let (task, _) = simple("task-structured-missing", model.clone());
    let error = task
        .call_as("t1", json!({"task": "count", "output_schema": counting()}))
        .await
        .unwrap_err();
    let text = error.to_string();
    assert!(text.contains("never called submit_result"));
    assert!(text.contains("here is prose"));
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(told(&seen[1]).contains("Call `submit_result` now"));
}

/// A subagent that submits once reminded succeeds.
#[tokio::test]
async fn a_nudged_subagent_can_still_submit() {
    let model = Router::new(|seen| match seen.messages {
        1 => say("forgot"),
        3 => call("s1", "submit_result", json!({"result": {"count": 5}})),
        _ => say("ok"),
    });
    let (task, _) = simple("task-structured-nudged", model);
    let out = task
        .call_as("t1", json!({"task": "count", "output_schema": counting()}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&out.text).unwrap(),
        json!({"count": 5})
    );
}

/// A kind of agent whose definition declares an output schema hands back structured results without the call asking, and is listed as doing so.
#[tokio::test]
async fn a_definition_can_declare_the_schema() {
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call("s1", "submit_result", json!({"result": {"count": 1}}))
        } else {
            say("ok")
        }
    });
    let mut agents = builtin_agents();
    agents.push(AgentDefinition {
        name: "counter".into(),
        description: "counts".into(),
        tools: Some(vec!["read".into()]),
        prompt: "Count.".into(),
        output_schema: Some(counting()),
        isolated: false,
        model: None,
        path: None,
        origin: Origin::User,
    });
    let (task, _, _) = task_tool(
        "task-structured-definition",
        model.clone(),
        agents,
        Arc::new(Jobs::new(|_| {})),
    );
    assert!(
        task.description().contains(
            "`counter` (READ-ONLY; investigation only, no edits) (returns structured JSON)"
        )
    );
    let out = task
        .call_as("t1", json!({"agent": "counter", "task": "count"}))
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&out.text).unwrap(),
        json!({"count": 1})
    );
    assert_eq!(
        model.seen.lock().unwrap()[0].tools,
        vec!["read", "submit_result"]
    );
}

/// Calls that give both forms, neither, too many items, a bad schema or an unknown agent start nothing.
#[tokio::test]
async fn bad_calls_start_nothing() {
    let model = Router::new(|_| say("ok"));
    let (task, log) = simple("task-batch-invalid", model.clone());
    let many: Vec<Value> = (0..=MAX_TASKS).map(|_| json!({"task": "x"})).collect();
    let calls = [
        json!({"task": "x", "tasks": [{"task": "y"}]}),
        json!({}),
        json!({"tasks": []}),
        json!({"tasks": many}),
        json!({"task": "x", "output_schema": "object"}),
    ];
    for args in calls {
        assert!(matches!(
            task.call_as("t1", args).await,
            Err(ToolError::InvalidArguments(_))
        ));
    }
    let unknown = task
        .call_as(
            "t1",
            json!({"tasks": [{"task": "x"}, {"agent": "ghost", "task": "y"}]}),
        )
        .await
        .unwrap_err();
    assert!(
        unknown
            .to_string()
            .starts_with("tasks[1]: unknown agent ghost")
    );
    assert!(log.lock().unwrap().is_empty());
    assert!(model.seen.lock().unwrap().is_empty());
}

/// A call's description counts its subagents and names their work.
#[test]
fn a_batch_is_described_by_its_items() {
    let (task, _) = simple("task-batch-describe", Router::new(|_| say("ok")));
    assert_eq!(
        task.describe(
            &json!({"tasks": [{"task": "alpha\nmore"}, {"task": "b", "description": "look"}]})
        ),
        "2 subagents: alpha, look"
    );
    assert_eq!(task.describe(&json!({"tasks": [{"task": "solo"}]})), "solo");
}

/// A call is shared when every subagent it starts is of a kind that only looks, and exclusive when any of them can write or its kind is unknown.
#[test]
fn only_looking_subagents_are_shared() {
    let (task, _) = simple("task-concurrency", Router::new(|_| say("ok")));
    let class = |args: Value| task.concurrency(&args);
    assert_eq!(class(json!({"task": "x"})), Concurrency::Exclusive);
    assert_eq!(
        class(json!({"agent": "scout", "task": "x"})),
        Concurrency::Shared
    );
    assert_eq!(
        class(json!({"agent": "scout", "tasks": [{"task": "a"}, {"task": "b"}]})),
        Concurrency::Shared
    );
    assert_eq!(
        class(json!({"tasks": [{"agent": "scout", "task": "a"}, {"task": "b"}]})),
        Concurrency::Exclusive
    );
    assert_eq!(
        class(json!({"agent": "nobody", "task": "x"})),
        Concurrency::Exclusive
    );
}

/// A model whose subagent requests take turns with each other, recording how many are under way at once.
struct Counting {
    /// The requests under way.
    now: AtomicUsize,
    /// The most that were under way at once.
    most: AtomicUsize,
}

#[async_trait]
impl ChatModel for Counting {
    /// Yields a few times while counted as under way, then answers.
    async fn complete(
        &self,
        _request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(now, Ordering::SeqCst);
        for _ in 0..20 {
            tokio::task::yield_now().await;
        }
        self.now.fetch_sub(1, Ordering::SeqCst);
        Ok(say("ok"))
    }
}

/// Two full batches started together never run more than the cap of subagents at once, and they do run together.
#[tokio::test]
async fn batches_run_at_most_the_cap_at_once() {
    let model = Arc::new(Counting {
        now: AtomicUsize::new(0),
        most: AtomicUsize::new(0),
    });
    let (task, _) = simple("task-batch-cap", model.clone());
    let items: Vec<Value> = (0..MAX_TASKS)
        .map(|index| json!({"task": format!("t{index}")}))
        .collect();
    let (first, second) = tokio::join!(
        task.call_as("t1", json!({ "tasks": items.clone() })),
        task.call_as("t2", json!({ "tasks": items })),
    );
    for out in [first.unwrap(), second.unwrap()] {
        assert!(
            out.text
                .starts_with(&format!("{0} of {0} subagents completed.", MAX_TASKS))
        );
    }
    let most = model.most.load(Ordering::SeqCst);
    assert!(most > 1 && most <= MAX_CONCURRENT, "{most} ran at once");
}

/// A model whose subagent requests never finish, so that only an abort ends them.
struct Stuck {
    /// Counts the requests that began.
    started: Arc<AtomicUsize>,
    /// Counts the stuck requests that were dropped.
    dropped: Arc<AtomicUsize>,
}

/// A guard that counts its drop.
struct Dropped(Arc<AtomicUsize>);

impl Drop for Dropped {
    /// Counts the drop.
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ChatModel for Stuck {
    /// Starts three subagents in one call for the main agent, and hangs for the subagents.
    async fn complete(
        &self,
        request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        if request.system == "MAIN" {
            return Ok(Reply {
                tool_calls: vec![ToolCall {
                    id: "t1".into(),
                    name: "task".into(),
                    arguments: json!({"tasks": [{"task": "a"}, {"task": "b"}, {"task": "c"}]}),
                }],
                ..Reply::default()
            });
        }
        let _guard = Dropped(self.dropped.clone());
        self.started.fetch_add(1, Ordering::SeqCst);
        std::future::pending().await
    }
}

/// Aborting the turn that started a batch ends every subagent of it.
#[tokio::test]
async fn aborting_a_turn_aborts_a_whole_batch() {
    let started = Arc::new(AtomicUsize::new(0));
    let dropped = Arc::new(AtomicUsize::new(0));
    let model = Arc::new(Stuck {
        started: started.clone(),
        dropped: dropped.clone(),
    });
    let (task, _, dir) = task_tool(
        "task-batch-abort",
        model.clone(),
        builtin_agents(),
        Arc::new(Jobs::new(|_| {})),
    );
    let mut registry = Registry::builtin(&dir).unwrap();
    registry.register(task);
    let handle = tokio::spawn(async move {
        let mut history = Vec::new();
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
            &mut history,
            "go".into(),
            &|_| {},
        )
        .await;
    });
    while started.load(Ordering::SeqCst) < 3 {
        tokio::task::yield_now().await;
    }
    handle.abort();
    let _ = handle.await;
    assert_eq!(dropped.load(Ordering::SeqCst), 3);
}
