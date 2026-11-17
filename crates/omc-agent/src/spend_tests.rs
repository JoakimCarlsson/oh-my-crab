//! Tests of what subagents cost: each run's spend, across follow-ups and the subagents it starts, the line its result ends with, what its announcements carry, and the budget that stops it.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_context::builtin_agents;
use omc_providers::{ChatModel, ProviderError, Reply, Request, Sink, Tokens};
use omc_tools::{Jobs, Registry, Tool};
use serde_json::json;

use crate::testkit::{RequestSeen, Router, call, deny_all, say, scratch};
use crate::{AgentEvent, EventSink, Nesting, Roster, SubagentState, Task, TaskSettings, Throttle};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A model answering as its router says, each answer billed ten input and two output tokens and charged `dollars` to the subagent run it was made for, as the conversation's priced models do.
struct Charging {
    /// What decides each answer.
    router: Arc<Router>,
    /// What each answer costs, when the model's prices are known.
    dollars: Option<f64>,
}

#[async_trait]
impl ChatModel for Charging {
    /// Answers as the router says and charges what the answer cost.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let tokens = Tokens {
            input: 10,
            output: 2,
            ..Tokens::default()
        };
        let reply = self.router.complete(request, sink).await?;
        crate::charge(&tokens, self.dollars);
        Ok(Reply {
            tokens: Some(tokens),
            ..reply
        })
    }
}

/// A `task` tool over the built-in tools of a scratch folder called `name`, whose subagents ask `model`, may start subagents `depth` levels down and are put on `roster`; with the log of its events.
fn tool(name: &str, model: Arc<dyn ChatModel>, depth: usize, roster: Arc<Roster>) -> (Task, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Task::new(TaskSettings {
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
        nesting: Nesting::new(depth, Arc::new(Throttle::new(8))).with_roster(roster),
    });
    (task, log)
}

/// Every subagent announcement in `log`, nested ones included, with its id, state and cost in millionths of a dollar.
fn spends(log: &Log) -> Vec<(String, SubagentState, Option<u64>)> {
    /// The announcement `event` holds, under the calls that wrap it.
    fn unwrap(event: &AgentEvent) -> Option<(String, SubagentState, Option<u64>)> {
        match event {
            AgentEvent::Subagent(notice) => Some((
                notice.id.clone(),
                notice.state,
                notice.spent.and_then(|spent| spent.cost),
            )),
            AgentEvent::Nested { event, .. } => unwrap(event),
            _ => None,
        }
    }
    log.lock().unwrap().iter().filter_map(unwrap).collect()
}

/// A priced subagent's result ends with what it cost, its announcements carry its spend as it grows and at its end, and a follow-up's result says what that run and all its runs cost.
#[tokio::test]
async fn results_and_announcements_carry_the_cost() {
    let roster = Arc::new(Roster::default());
    let router = Router::new(|_: &RequestSeen| say("done"));
    let model = Arc::new(Charging {
        router,
        dollars: Some(0.1),
    });
    let (task, log) = tool("spend-basic", model, 1, roster);
    let first = task.call_as("t1", json!({"task": "look"})).await.unwrap();
    assert!(
        first.text.ends_with("\n\n(Subagent cost: $0.10.)"),
        "{}",
        first.text
    );
    assert_eq!(
        spends(&log),
        [
            ("t1".to_owned(), SubagentState::Running, None),
            ("t1".to_owned(), SubagentState::Running, Some(100_000)),
            ("t1".to_owned(), SubagentState::Finished, Some(100_000)),
        ]
    );
    let second = task
        .call_as("t2", json!({"continue": "sub-1", "task": "more"}))
        .await
        .unwrap();
    assert!(
        second
            .text
            .ends_with("\n\n(Subagent cost: $0.10 this run, $0.20 in all.)"),
        "{}",
        second.text
    );
    assert_eq!(
        spends(&log).last().unwrap(),
        &("t2".to_owned(), SubagentState::Finished, Some(200_000))
    );
}

/// What a nested subagent costs counts toward the subagent that started it too, once, beside that one's own requests.
#[tokio::test]
async fn a_child_counts_toward_its_parent() {
    let router = Router::new(|seen: &RequestSeen| {
        let outer = seen
            .last_user
            .as_deref()
            .unwrap_or_default()
            .contains("outer");
        if outer && seen.messages == 1 {
            call("c1", "task", json!({"task": "inner"}))
        } else {
            say("done")
        }
    });
    let model = Arc::new(Charging {
        router,
        dollars: Some(0.1),
    });
    let (task, log) = tool("spend-nested", model, 2, Arc::new(Roster::default()));
    let done = task.call_as("t1", json!({"task": "outer"})).await.unwrap();
    assert!(
        done.text.ends_with("\n\n(Subagent cost: $0.30.)"),
        "{}",
        done.text
    );
    let ends: Vec<_> = spends(&log)
        .into_iter()
        .filter(|(_, state, _)| *state == SubagentState::Finished)
        .collect();
    assert_eq!(
        ends,
        [
            ("t1:c1".to_owned(), SubagentState::Finished, Some(100_000)),
            ("t1".to_owned(), SubagentState::Finished, Some(300_000)),
        ]
    );
}

/// A subagent that spends more than the budget is stopped after the step it is in and returns what it said last with a note naming the budget and what it spent.
#[tokio::test]
async fn the_budget_stops_a_subagent_after_its_step() {
    let roster = Arc::new(Roster::default());
    roster.set_budget(Some(0.15));
    let router = Router::new(|seen: &RequestSeen| Reply {
        text: format!("step {}", seen.messages),
        ..call(
            &format!("g{}", seen.messages),
            "glob",
            json!({"pattern": "*.rs"}),
        )
    });
    let model = Arc::new(Charging {
        router: router.clone(),
        dollars: Some(0.1),
    });
    let (task, _) = tool("spend-budget", model, 1, roster);
    let done = task.call_as("t1", json!({"task": "loop"})).await.unwrap();
    assert_eq!(router.seen.lock().unwrap().len(), 2);
    assert!(
        done.text.starts_with("step 3\n\n# Budget\n"),
        "{}",
        done.text
    );
    assert!(
        done.text
            .contains("spent $0.20 this run, more than the $0.15"),
        "{}",
        done.text
    );
    assert!(
        done.text.ends_with("(Subagent cost: $0.20.)"),
        "{}",
        done.text
    );
}

/// A model whose prices are not known never trips the budget and earns no cost line, while its announcements still count its requests.
#[tokio::test]
async fn unpriced_models_never_trip_the_budget() {
    let roster = Arc::new(Roster::default());
    roster.set_budget(Some(0.01));
    let router = Router::new(|seen: &RequestSeen| {
        if seen.messages < 5 {
            call(
                &format!("g{}", seen.messages),
                "glob",
                json!({"pattern": "*.rs"}),
            )
        } else {
            say("finished")
        }
    });
    let model = Arc::new(Charging {
        router: router.clone(),
        dollars: None,
    });
    let (task, log) = tool("spend-unpriced", model, 1, roster);
    let done = task.call_as("t1", json!({"task": "loop"})).await.unwrap();
    assert!(done.text.starts_with("finished"), "{}", done.text);
    assert!(
        !done.text.contains("Budget") && !done.text.contains("cost"),
        "{}",
        done.text
    );
    assert_eq!(router.seen.lock().unwrap().len(), 3);
    let last = log
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|event| match event {
            AgentEvent::Subagent(notice) => notice.spent,
            _ => None,
        })
        .unwrap();
    assert_eq!((last.requests, last.cost), (3, None));
}
