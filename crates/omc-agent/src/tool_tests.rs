//! Tests of the tools that talk to the reader, driven through the agent loop.

use std::sync::Arc;

use omc_providers::Message;
use omc_tools::{Ask, ExitPlanMode, Registry, Todo};
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
