//! Tests of long-lived subagents: spawning one, sending it messages, waiting for its replies and killing it, and the messages and task list members of a crew share.

use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omc_context::builtin_agents;
use omc_tools::{Jobs, Registry, Tool, ToolError};
use serde_json::json;

use crate::crew::{MESSAGES_PER_RUN, member_tools};
use crate::testkit::{Router, call, deny_all, say, scratch};
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
    tools_in(name, model, roster, Arc::new(Crew::default()))
}

/// The crew tools over `crew` and a `task` tool for a scratch folder called `name`, asking `model`, with `roster`; by name, with the log of their events.
fn tools_in(
    name: &str,
    model: Arc<Router>,
    roster: Arc<Roster>,
    crew: Arc<Crew>,
) -> (std::collections::HashMap<&'static str, Arc<dyn Tool>>, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Arc::new(Task::new(TaskSettings {
        categories: Vec::new(),
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
    let tools = crew_tools(task, crew)
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

/// Tools by name, as [`tools_in`] gives them.
type Tools = std::collections::HashMap<&'static str, Arc<dyn Tool>>;

/// Spawns a scout called `name` with the first prompt `prompt` through `tools`, under the call id `call`, and returns its first reply.
async fn spawn(tools: &Tools, call: &str, name: &str, prompt: &str) -> String {
    tools["agent_spawn"]
        .call_as(
            call,
            json!({"name": name, "agent": "scout", "prompt": prompt}),
        )
        .await
        .unwrap();
    reply(tools, name).await
}

/// The next reply of the member `name`, waiting through `tools` until it owes one, as it does only once its worker took a message from its mailbox.
async fn reply(tools: &Tools, name: &str) -> String {
    for _ in 0..500 {
        let waited = tools["agent_wait"]
            .call(json!({"name": name, "timeout": 10}))
            .await
            .unwrap();
        if !waited.text.contains("owes no reply") {
            return waited.text;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("{name} never replied");
}

/// A message to an idle member wakes it, and its reply to the message goes to the lead; members have the crew tools.
#[tokio::test]
async fn a_message_wakes_an_idle_member() {
    let crew = Arc::new(Crew::default());
    let model = counting();
    let (tools, _) = tools_in(
        "crew-idle",
        model.clone(),
        Arc::new(Roster::default()),
        crew.clone(),
    );
    spawn(&tools, "c1", "a", "start").await;
    spawn(&tools, "c2", "b", "start").await;
    let sent = crew.post("a", "b", "please check the tests").unwrap();
    assert!(sent.contains("idle"), "{sent}");
    let heard = reply(&tools, "b").await;
    assert!(heard.contains("message from a"), "{heard}");
    let offered = &model.seen.lock().unwrap()[0].tools;
    for tool in ["crew_message", "crew_members", "crew_tasks"] {
        assert!(offered.iter().any(|name| name == tool), "{tool} missing");
    }
}

/// A message to a member that is working reaches it at its next step boundary, within the same run.
#[tokio::test]
async fn a_message_reaches_a_working_member_between_steps() {
    let crew = Arc::new(Crew::default());
    let weak = Arc::downgrade(&crew);
    let model = Router::new(move |seen| {
        let last = seen.last_user.clone().unwrap_or_default();
        if last.lines().last() == Some("work") {
            let crew = weak.upgrade().unwrap();
            let sent = crew.post("a", "b", "stop and look at x.rs").unwrap();
            assert!(sent.contains("working"), "{sent}");
            return call("m1", "crew_members", json!({}));
        }
        say(&format!("heard: {last}"))
    });
    let (tools, _) = tools_in(
        "crew-working",
        model,
        Arc::new(Roster::default()),
        crew.clone(),
    );
    let first = spawn(&tools, "c1", "b", "work").await;
    assert!(first.contains("<crew-message from=\"a\">"), "{first}");
    assert!(first.contains("stop and look at x.rs"), "{first}");
    let idle = tools["agent_wait"]
        .call(json!({"name": "b"}))
        .await
        .unwrap();
    assert!(idle.text.contains("owes no reply"));
}

/// `all` reaches every other live member and not the sender, and a message to the lead is queued among the sender's replies without waking anyone.
#[tokio::test]
async fn broadcast_and_lead_messages() {
    let crew = Arc::new(Crew::default());
    let (tools, _) = tools_in(
        "crew-broadcast",
        counting(),
        Arc::new(Roster::default()),
        crew.clone(),
    );
    spawn(&tools, "c1", "a", "start").await;
    spawn(&tools, "c2", "b", "start").await;
    spawn(&tools, "c3", "c", "start").await;
    assert_eq!(crew.post("a", "all", "heads up").unwrap(), "Sent to b, c.");
    assert!(reply(&tools, "b").await.contains("message from a"));
    assert!(reply(&tools, "c").await.contains("message from a"));
    crew.post("a", "lead", "found the bug").unwrap();
    assert_eq!(reply(&tools, "a").await, "Message from a:\nfound the bug");
    let listed = tools["agent_list"].call(json!({})).await.unwrap();
    assert!(
        listed.text.contains("- a: scout, idle, owes 0"),
        "{}",
        listed.text
    );
}

/// Messages to a member there is none of, or to one that was stopped, are refused, as are messages to oneself and past the cap of a run.
#[tokio::test]
async fn bad_recipients_and_the_cap_are_refused() {
    let crew = Arc::new(Crew::default());
    let roster = Arc::new(Roster::default());
    let (tools, _) = tools_in("crew-refused", counting(), roster.clone(), crew.clone());
    spawn(&tools, "c1", "a", "start").await;
    spawn(&tools, "c2", "b", "start").await;
    assert!(matches!(
        crew.post("a", "nobody", "x"),
        Err(ToolError::Failed(text)) if text.contains("no long-lived subagent is called nobody")
    ));
    assert!(roster.stop("c2"));
    let mut stopped = false;
    for _ in 0..500 {
        if let Err(ToolError::Failed(text)) = crew.post("a", "b", "x") {
            assert!(text.contains("stopped"), "{text}");
            stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(stopped);
    let sent = Arc::new(AtomicUsize::new(0));
    let own = member_tools(&crew, "a", sent.clone());
    let message = own
        .iter()
        .find(|tool| tool.name() == "crew_message")
        .unwrap();
    let to_self = message.call(json!({"to": "a", "message": "hi"})).await;
    assert!(matches!(to_self, Err(ToolError::InvalidArguments(_))));
    message
        .call(json!({"to": "lead", "message": "one"}))
        .await
        .unwrap();
    assert_eq!(sent.load(std::sync::atomic::Ordering::SeqCst), 1);
    sent.store(MESSAGES_PER_RUN, std::sync::atomic::Ordering::SeqCst);
    let capped = message.call(json!({"to": "lead", "message": "two"})).await;
    assert!(
        matches!(capped, Err(ToolError::Failed(text)) if text.contains("the most a member may"))
    );
}

/// The lead gets `crew_tasks` only once the crew has members; it adds and assigns tasks, and members claim them, one claim winning.
#[tokio::test]
async fn the_lead_shares_a_task_list_once_there_is_a_crew() {
    let crew = Arc::new(Crew::default());
    let roster = Arc::new(Roster::default());
    let (tools, _) = tools_in("crew-board", counting(), roster.clone(), crew.clone());
    assert!(!tools.contains_key("crew_tasks"));
    spawn(&tools, "c1", "a", "start").await;
    spawn(&tools, "c2", "b", "start").await;
    let (lead, _) = tools_in("crew-board-lead", counting(), roster, crew.clone());
    let board = &lead["crew_tasks"];
    board
        .call(json!({"action": "add", "title": "schema"}))
        .await
        .unwrap();
    board
        .call(json!({"action": "add", "title": "api", "after": ["t1"]}))
        .await
        .unwrap();
    let claim = |name: &str| {
        member_tools(&crew, name, Arc::new(AtomicUsize::new(0)))
            .into_iter()
            .find(|tool| tool.name() == "crew_tasks")
            .unwrap()
    };
    let (a, b) = (claim("a"), claim("b"));
    let early = b.call(json!({"action": "claim", "id": "t2"})).await;
    assert!(matches!(early, Err(ToolError::Failed(text)) if text.contains("waits on t1")));
    a.call(json!({"action": "claim", "id": "t1"}))
        .await
        .unwrap();
    let lost = b.call(json!({"action": "claim", "id": "t1"})).await;
    assert!(matches!(lost, Err(ToolError::Failed(text)) if text.contains("claimed by a")));
    a.call(json!({"action": "done", "id": "t1", "result": "tables made"}))
        .await
        .unwrap();
    let assigned = board
        .call(json!({"action": "assign", "id": "t2", "to": "b"}))
        .await
        .unwrap();
    assert!(assigned.text.contains("now b's"));
    let listed = a.call(json!({"action": "list"})).await.unwrap();
    assert!(listed.text.contains("- t1 [done by a: tables made] schema"));
    assert!(listed.text.contains("- t2 [claimed by b] api (after t1)"));
    tools["agent_kill"]
        .call(json!({"name": "b"}))
        .await
        .unwrap();
    let freed = a.call(json!({"action": "list"})).await.unwrap();
    assert!(freed.text.contains("- t2 [open] api"));
}

/// One-off `task` subagents never get the crew tools, even from a registry holding the lead's `crew_tasks`.
#[tokio::test]
async fn one_off_subagents_get_no_crew_tools() {
    let crew = Arc::new(Crew::default());
    let dir = scratch("crew-one-off");
    let mut registry = Registry::builtin(&dir).unwrap();
    registry.register(crate::board::CrewTasks {
        crew: Arc::downgrade(&crew),
        who: None,
    });
    let model = Router::new(|_| say("done"));
    let task = Task::new(TaskSettings {
        categories: Vec::new(),
        model: model.clone(),
        models: crate::Models::default(),
        registry: Arc::new(registry),
        approvals: deny_all(),
        events: Arc::new(|_| {}),
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: None,
        nesting: Nesting::new(1, Arc::new(Throttle::new(8))),
    });
    task.call_as("t1", json!({"task": "look"})).await.unwrap();
    let seen = model.seen.lock().unwrap();
    assert!(!seen.is_empty());
    for request in seen.iter() {
        assert!(
            !request.tools.iter().any(|name| name.starts_with("crew_")),
            "{:?}",
            request.tools
        );
    }
}
