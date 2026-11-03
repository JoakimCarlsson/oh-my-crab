//! Tests of the tools that talk to the reader, driven through the agent loop.

use std::sync::Arc;

use omc_providers::Message;
use omc_tools::{Ask, ContextNotes, ExitPlanMode, NewContext, Notebook, Registry, Todo};
use omc_wire::{Progress, Reply as Answer};
use serde_json::json;

use crate::testkit::{Reader, Router, call, deny_all, say};
use crate::{AgentEvent, Context, run_turn};

/// Runs one turn of `go` on `registry` against `model` and returns the history.
async fn drive(model: &Router, registry: &Registry) -> Vec<Message> {
    let mut history = Vec::new();
    run_turn(
        &Context {
            model,
            registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    history
}

/// The text of the first result of the tool results at `index` of `history`.
fn result(history: &[Message], index: usize) -> String {
    let Message::ToolResults(results) = &history[index] else {
        panic!("no results");
    };
    results[0].text.clone()
}

/// A `todo` call shows its list to the reader.
#[tokio::test]
async fn todo_reaches_the_reader() {
    let reader = Arc::new(Reader::default());
    let mut registry = Registry::new();
    registry.register(Todo::new(reader.clone()));
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call(
                "t",
                "todo",
                json!({"todos": [{"content": "a", "status": "in_progress"}, {"content": "b", "status": "pending"}]}),
            )
        } else {
            say("ok")
        }
    });
    let history = drive(&model, &registry).await;
    assert!(result(&history, 2).contains("2 tasks"));
    let plans = reader.plans.lock().unwrap();
    assert_eq!(plans[0][0].progress, Progress::Doing);
    assert_eq!(plans[0][1].text, "b");
}

/// An `ask` call waits for the reader and hands the model their answers.
#[tokio::test]
async fn ask_returns_the_answers() {
    let reader = Arc::new(Reader::default());
    *reader.replies.lock().unwrap() = Some(vec![Answer {
        picks: vec!["Blue".into()],
        other: "but darker".into(),
    }]);
    let mut registry = Registry::new();
    registry.register(Ask::new(reader.clone()));
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call(
                "a",
                "ask",
                json!({"questions": [{"id": "c", "question": "Colour?", "options": [{"label": "Blue"}, {"label": "Red"}]}]}),
            )
        } else {
            say("ok")
        }
    });
    let history = drive(&model, &registry).await;
    assert_eq!(result(&history, 2), "Colour?: Blue, but darker");
    assert_eq!(reader.forms.lock().unwrap()[0][0].alternatives.len(), 2);
}

/// A rejected plan is an error the model reads; an approved one is not.
#[tokio::test]
async fn plan_verdicts_reach_the_model() {
    let reader = Arc::new(Reader::default());
    let mut registry = Registry::new();
    registry.register(ExitPlanMode::new(reader.clone(), false));
    let script = || {
        Router::new(|seen| {
            if seen.messages == 1 {
                call("p", "exit_plan_mode", json!({"plan": "# Plan\n1. x"}))
            } else {
                say("ok")
            }
        })
    };
    let history = drive(&script(), &registry).await;
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results");
    };
    assert!(results[0].is_error);
    assert!(results[0].text.contains("not approve"));
    *reader.approve.lock().unwrap() = true;
    let history = drive(&script(), &registry).await;
    assert!(result(&history, 2).contains("Plan approved"));
}

/// A registry holding the two notebook tools over `notebook`.
fn notebook_registry(notebook: &Arc<Notebook>) -> Registry {
    let mut registry = Registry::new();
    registry.register(ContextNotes::new(notebook.clone()));
    registry.register(NewContext::new(notebook.clone()));
    registry
}

/// Runs one turn of `go` against `model` with a notebook and returns the history.
async fn drive_with_notebook(
    model: &Router,
    registry: &Registry,
    notebook: &Notebook,
) -> Vec<Message> {
    let mut history = Vec::new();
    run_turn(
        &Context {
            model,
            registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: Some(notebook),
            window: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    history
}

/// Notes are written, the window is reset, and the same turn goes on with only the notes in the history.
#[tokio::test]
async fn a_reset_continues_the_turn_with_only_the_notes() {
    let notebook = Arc::new(Notebook::new());
    let registry = notebook_registry(&notebook);
    let model = Router::new(|seen| {
        let fresh = seen
            .last_user
            .as_ref()
            .is_some_and(|text| text.contains("<context-notes>"));
        match seen.messages {
            _ if fresh => say("done"),
            1 => call("n", "context_notes", json!({"text": "fact one"})),
            3 => call("r", "new_context", json!({})),
            _ => say("done"),
        }
    });
    let history = drive_with_notebook(&model, &registry, &notebook).await;
    let sizes: Vec<usize> = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.messages)
        .collect();
    assert_eq!(sizes, vec![1, 3, 1]);
    assert!(
        model.seen.lock().unwrap()[2]
            .last_user
            .as_ref()
            .unwrap()
            .contains("fact one")
    );
    assert_eq!(history.len(), 2);
    assert!(matches!(&history[0], Message::User(text) if text.contains("fact one")));
    assert!(matches!(&history[1], Message::Assistant(_)));
}

/// A reset with an empty notebook is an error the model reads and the history is left alone.
#[tokio::test]
async fn a_reset_without_notes_changes_nothing() {
    let notebook = Arc::new(Notebook::new());
    let registry = notebook_registry(&notebook);
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call("r", "new_context", json!({}))
        } else {
            say("done")
        }
    });
    let history = drive_with_notebook(&model, &registry, &notebook).await;
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results");
    };
    assert!(results[0].is_error);
    assert!(results[0].text.contains("context_notes"));
    assert_eq!(history.len(), 4);
    let sizes: Vec<usize> = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.messages)
        .collect();
    assert_eq!(sizes, vec![1, 3]);
}

/// Every change to the notebook during a turn is written to the journal, so a reopened conversation has it.
#[tokio::test]
async fn the_notebook_is_saved_to_the_journal() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-agent/journal/tool-notes.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
    let (journal, mut history) = crate::Journal::open(&path, None).unwrap();
    let notebook = Arc::new(Notebook::new());
    let registry = notebook_registry(&notebook);
    let model = Router::new(|seen| {
        if seen.messages == 1 {
            call("n", "context_notes", json!({"text": "kept"}))
        } else {
            say("done")
        }
    });
    crate::run_turn_recorded(
        &Context {
            model: &*model,
            registry: &registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: Some(&notebook),
            window: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
        Some(&journal.scribe()),
    )
    .await
    .unwrap();
    drop(journal);
    let (journal, _) = crate::Journal::open(&path, None).unwrap();
    assert_eq!(journal.notes(), "kept");
}

/// A request that fills most of the window gets a reminder on the tool results that follow, once.
#[tokio::test]
async fn a_full_window_brings_one_reminder() {
    let notebook = Arc::new(Notebook::new());
    let registry = notebook_registry(&notebook);
    let model = Router::new(|seen| {
        let mut reply = if seen.messages < 6 {
            call("n", "context_notes", json!({}))
        } else {
            say("done")
        };
        reply.tokens = Some(omc_providers::Tokens {
            input: 90,
            ..Default::default()
        });
        reply
    });
    let mut history = Vec::new();
    run_turn(
        &Context {
            model: &*model,
            registry: &registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: Some(&notebook),
            window: Some(100),
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    let reminders = history
        .iter()
        .filter_map(|message| match message {
            Message::ToolResults(results) => Some(results),
            _ => None,
        })
        .flatten()
        .filter(|result| result.text.contains("<context-window-reminder>"))
        .count();
    assert_eq!(reminders, 1);
}
