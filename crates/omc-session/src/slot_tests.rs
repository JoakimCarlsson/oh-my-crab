//! Tests of turns asking the models of their slots: compaction, plan mode, subagents and backups, each with a scripted model of its own.

use std::sync::Mutex;

use async_trait::async_trait;
use omc_providers::{ProviderError, Reply, Request, Sink, ToolCall};
use omc_wire::{Event, SlotScope};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

use super::*;
use crate::slots::SlotFiles;

/// A model that answers the `n`th request it is sent, counted from one, as `reply` says, and keeps the system prompt of each.
struct Recording {
    /// What it answers the request of each number.
    reply: fn(usize) -> Result<Reply, ProviderError>,
    /// The system prompt of every request, in order.
    systems: Mutex<Vec<String>>,
}

impl Recording {
    /// A model answering as `reply` says.
    fn new(reply: fn(usize) -> Result<Reply, ProviderError>) -> Arc<Self> {
        Arc::new(Self {
            reply,
            systems: Mutex::new(Vec::new()),
        })
    }

    /// How many requests it was sent.
    fn asked(&self) -> usize {
        self.systems.lock().unwrap().len()
    }
}

#[async_trait]
impl ChatModel for Recording {
    /// Records the request's system prompt and answers it.
    async fn complete(
        &self,
        request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let mut systems = self.systems.lock().unwrap();
        systems.push(request.system.to_owned());
        (self.reply)(systems.len())
    }
}

/// A reply saying `text`.
fn said(text: &str) -> Result<Reply, ProviderError> {
    Ok(Reply {
        text: text.to_owned(),
        ..Reply::default()
    })
}

/// A reply that calls `name` once with `arguments`.
fn calling(name: &str, arguments: serde_json::Value) -> Result<Reply, ProviderError> {
    Ok(Reply {
        tool_calls: vec![ToolCall {
            id: name.into(),
            name: name.into(),
            arguments,
        }],
        ..Reply::default()
    })
}

/// An empty scratch folder named `name` under the build folder.
fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-session/slot-turns")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A conversation open on Claude's model `m` in `mode`, the provider listing `m` and the ids of `models`, keeping its slots in `dir`, with each of `slots` set to its model and backups, and each of `models` given for its id; with the events it sends.
fn conversation(
    dir: &std::path::Path,
    mode: Mode,
    slots: &[(&str, &str, &[&str])],
    models: Vec<(&str, Arc<dyn ChatModel>)>,
) -> (Arc<Shared>, UnboundedReceiver<Event>) {
    let (out, events) = tokio::sync::mpsc::unbounded_channel();
    let shared = Shared::new(out);
    let listed = std::iter::once("m")
        .chain(models.iter().map(|(id, _)| *id))
        .map(str::to_owned)
        .collect();
    shared.open(Provider::Claude, listed, "m".into(), mode);
    let files = SlotFiles {
        user: Some(dir.join("slots.json")),
        project: None,
    };
    for (name, model, backups) in slots {
        files
            .set_slot(
                SlotScope::User,
                name,
                None,
                Some((*model).to_owned()),
                backups.iter().map(|backup| (*backup).to_owned()).collect(),
            )
            .unwrap();
    }
    shared.set_slot_files(files);
    shared
        .named_models
        .lock()
        .unwrap()
        .extend(models.into_iter().map(|(id, model)| (id.to_owned(), model)));
    (shared, events)
}

/// A turn for `shared` in `dir` asking the model `m`, with the built-in tools and agents and `history`.
fn turn_in(dir: &std::path::Path, shared: &Arc<Shared>, history: Vec<Message>) -> Turn {
    Turn {
        provider: Provider::Claude,
        model: "m".into(),
        base: Arc::new(Registry::builtin(dir).unwrap()),
        denied: Arc::new(Vec::new()),
        discovered: Arc::new(Discovered {
            agents: omc_context::builtin_agents(),
            ..Discovered::default()
        }),
        disabled: Arc::new(Vec::new()),
        locations: Arc::new(Locations {
            directory: dir.to_path_buf(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        }),
        shared: shared.clone(),
        history,
        scribe: None,
        isolation: None,
        hooks: None,
    }
}

/// A compaction is summarized by the `fast` slot's model, not the conversation's.
#[tokio::test]
async fn compaction_asks_the_fast_slot() {
    let dir = scratch("compaction");
    let main = Recording::new(|_| said("main summary"));
    let fast = Recording::new(|_| said("fast summary"));
    let (shared, _events) = conversation(
        &dir,
        Mode::Yolo,
        &[(slots::FAST, "claude/fast-1", &[])],
        vec![("fast-1", fast.clone())],
    );
    let history = vec![
        Message::User("a".repeat(200_000)),
        Message::Assistant(Reply {
            text: "first".into(),
            ..Reply::default()
        }),
        Message::User("b".repeat(200_000)),
        Message::Assistant(Reply {
            text: "second".into(),
            ..Reply::default()
        }),
    ];
    let turn = turn_in(&dir, &shared, history);
    let Ended::Done(history) = turn.run_with(main.clone(), Job::Compact(None)).await else {
        panic!("the compaction failed");
    };
    assert_eq!(main.asked(), 0);
    assert_eq!(fast.asked(), 1);
    assert!(history.iter().any(
        |message| matches!(message, Message::Compaction(done) if done.summary.contains("fast summary"))
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

/// In plan mode the `plan` slot's model plans, and once the plan is approved the conversation's model carries it out.
#[tokio::test]
async fn plan_mode_asks_the_plan_slot() {
    let dir = scratch("plan");
    let main = Recording::new(|_| said("implemented"));
    let planner = Recording::new(|_| calling("exit_plan_mode", json!({"plan": "# Plan\n1. x"})));
    let (shared, mut events) = conversation(
        &dir,
        Mode::Yolo,
        &[(slots::PLAN, "claude/planner-1", &[])],
        vec![("planner-1", planner.clone())],
    );
    shared.set_mode(Mode::Plan);
    *shared.model_override.lock().unwrap() = Some(main.clone());
    let turn = turn_in(&dir, &shared, Vec::new());
    let running = tokio::spawn(turn.run(Job::Prompt("plan it".into(), Vec::new())));
    let ticket = loop {
        match events.recv().await {
            Some(Event::Ask(asked)) => break asked.ticket,
            Some(_) => {}
            None => panic!("no plan was proposed"),
        }
    };
    shared.questions.answer(ticket, Some("allow"), None);
    let Ended::Done(_) = running.await.unwrap() else {
        panic!("the turn failed");
    };
    assert_eq!(planner.asked(), 1);
    assert_eq!(main.asked(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// A subagent whose kind names no model asks the `subagent` slot's model.
#[tokio::test]
async fn subagents_ask_the_subagent_slot() {
    let dir = scratch("subagent");
    let main = Recording::new(|asked| match asked {
        1 => calling("task", json!({"agent": "task", "task": "do step 1"})),
        _ => said("done"),
    });
    let helper = Recording::new(|_| said("step done"));
    let (shared, _events) = conversation(
        &dir,
        Mode::Yolo,
        &[(slots::SUBAGENT, "claude/helper-1", &[])],
        vec![("helper-1", helper.clone())],
    );
    let turn = turn_in(&dir, &shared, Vec::new());
    let Ended::Done(_) = turn
        .run_with(main.clone(), Job::Prompt("delegate".into(), Vec::new()))
        .await
    else {
        panic!("the turn failed");
    };
    assert_eq!(main.asked(), 2);
    assert_eq!(helper.asked(), 1);
    assert!(helper.systems.lock().unwrap()[0].contains("assigned to you by the main agent"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A conversation's model still overloaded after every retry hands the rest of the turn to the `main` slot's backup, and the client is told with a `ModelFallback` event; a sign-in failure ends the turn without one.
#[tokio::test]
async fn an_overloaded_model_falls_back_on_the_main_backups() {
    let dir = scratch("fallback");
    let main = Recording::new(|_| {
        Err(ProviderError::Status {
            status: "529".into(),
            body: "overloaded".into(),
            retry_after: Some(std::time::Duration::ZERO),
        })
    });
    let spare = Recording::new(|_| said("spare answered"));
    let (shared, mut events) = conversation(
        &dir,
        Mode::Yolo,
        &[(slots::MAIN, "claude/m", &["claude/spare-1"])],
        vec![("spare-1", spare.clone())],
    );
    let turn = turn_in(&dir, &shared, Vec::new());
    let Ended::Done(history) = turn
        .run_with(main.clone(), Job::Prompt("go".into(), Vec::new()))
        .await
    else {
        panic!("the turn failed");
    };
    assert_eq!(spare.asked(), 1);
    assert!(
        matches!(history.last(), Some(Message::Assistant(reply)) if reply.text == "spare answered")
    );
    let mut fallbacks = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let Event::ModelFallback { slot, from, to, .. } = event {
            fallbacks.push((slot, from, to));
        }
    }
    assert_eq!(
        fallbacks,
        [("main".to_owned(), "m".to_owned(), "spare-1".to_owned())]
    );
    let refused = Recording::new(|_| {
        Err(ProviderError::Status {
            status: "401".into(),
            body: "invalid x-api-key".into(),
            retry_after: None,
        })
    });
    let turn = turn_in(&dir, &shared, Vec::new());
    let Ended::Failed(_) = turn
        .run_with(refused, Job::Prompt("go".into(), Vec::new()))
        .await
    else {
        panic!("the turn did not fail");
    };
    assert_eq!(spare.asked(), 1);
    while let Ok(event) = events.try_recv() {
        assert!(!matches!(event, Event::ModelFallback { .. }));
    }
    let _ = std::fs::remove_dir_all(&dir);
}
