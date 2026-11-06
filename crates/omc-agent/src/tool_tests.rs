//! Tests of the tools that talk to the reader, driven through the agent loop.

use std::sync::Arc;

use omc_providers::Message;
use omc_tools::{
    Ask, Checkpoint, CheckpointTool, ContextNotes, ExitPlanMode, NewContext, Notebook, Registry,
    Rewind, Todo,
};
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
            checkpoint: None,
            hooks: None,
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
            checkpoint: None,
            hooks: None,
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
            checkpoint: None,
            hooks: None,
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
            checkpoint: None,
            hooks: None,
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

/// A registry with the builtin tools in `dir` and the two checkpoint tools over `checkpoint`.
fn checkpoint_registry(dir: &std::path::Path, checkpoint: &Arc<Checkpoint>) -> Registry {
    let mut registry = Registry::builtin(dir).unwrap();
    registry.register(CheckpointTool::new(checkpoint.clone()));
    registry.register(Rewind::new(checkpoint.clone()));
    registry
}

/// Runs one turn of `go` on `registry` with `checkpoint`, letting the tools run without asking, and returns the history.
async fn drive_with_checkpoint(
    model: &Router,
    registry: &Registry,
    checkpoint: &Checkpoint,
) -> Vec<Message> {
    let approvals = Arc::new(crate::Approvals::new(
        Arc::new(crate::testkit::DenyAll),
        true,
    ));
    let mut history = Vec::new();
    run_turn(
        &Context {
            model,
            registry,
            approvals: &approvals,
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: Some(checkpoint),
            hooks: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    history
}

/// After a checkpoint, several tool rounds and a rewind, the next request carries the report and none of the exploration, and files written in between stay as they are.
#[tokio::test]
async fn a_rewind_replaces_the_exploration_with_the_report() {
    let dir = crate::testkit::scratch("checkpoint-rewind");
    let checkpoint = Arc::new(Checkpoint::new());
    let registry = checkpoint_registry(&dir, &checkpoint);
    let step = std::sync::atomic::AtomicUsize::new(0);
    let model =
        Router::new(
            move |_| match step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => call("c", "checkpoint", json!({})),
                1 => call("w", "write", json!({"path": "f.txt", "content": "kept"})),
                2 => call("g", "glob", json!({"pattern": "*"})),
                3 => call(
                    "r",
                    "rewind",
                    json!({"report": "  f.txt holds the answer  "}),
                ),
                _ => say("done"),
            },
        );
    let history = drive_with_checkpoint(&model, &registry, &checkpoint).await;
    let sizes: Vec<usize> = model
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|s| s.messages)
        .collect();
    assert_eq!(sizes, vec![1, 3, 5, 7, 4]);
    assert_eq!(history.len(), 5);
    assert!(matches!(&history[0], Message::User(text) if text == "go"));
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results");
    };
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].name, "checkpoint");
    assert!(!results[0].is_error);
    assert!(
        matches!(&history[3], Message::User(text) if text.ends_with("Report:\nf.txt holds the answer"))
    );
    assert!(matches!(&history[4], Message::Assistant(_)));
    assert!(!checkpoint.active());
    assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "kept");
}

/// Calls that ran in the same batch as `rewind` are kept after the checkpoint, ahead of the report.
#[tokio::test]
async fn calls_beside_a_rewind_are_kept() {
    let dir = crate::testkit::scratch("checkpoint-siblings");
    let checkpoint = Arc::new(Checkpoint::new());
    let registry = checkpoint_registry(&dir, &checkpoint);
    let step = std::sync::atomic::AtomicUsize::new(0);
    let model =
        Router::new(
            move |_| match step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => call("c", "checkpoint", json!({"goal": "look"})),
                1 => call("g", "glob", json!({"pattern": "*"})),
                2 => {
                    let mut reply = call("w", "write", json!({"path": "f.txt", "content": "kept"}));
                    reply
                        .tool_calls
                        .extend(call("r", "rewind", json!({"report": "found"})).tool_calls);
                    reply
                }
                _ => say("done"),
            },
        );
    let history = drive_with_checkpoint(&model, &registry, &checkpoint).await;
    assert_eq!(history.len(), 7);
    let Message::Assistant(reply) = &history[3] else {
        panic!("no calls");
    };
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(reply.tool_calls[0].name, "write");
    let Message::ToolResults(results) = &history[4] else {
        panic!("no results");
    };
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].call_id, "w");
    assert!(matches!(&history[5], Message::User(text) if text.ends_with("found")));
    assert_eq!(std::fs::read_to_string(dir.join("f.txt")).unwrap(), "kept");
}

/// A rewind without a checkpoint, a second checkpoint and an empty report are errors the model reads, and the history is left alone.
#[tokio::test]
async fn misused_checkpoint_tools_change_nothing() {
    let dir = crate::testkit::scratch("checkpoint-errors");
    let checkpoint = Arc::new(Checkpoint::new());
    let registry = checkpoint_registry(&dir, &checkpoint);
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "rewind", json!({"report": "x"})),
        3 => call("c", "checkpoint", json!({})),
        5 => call("c2", "checkpoint", json!({})),
        7 => call("e", "rewind", json!({"report": "   "})),
        _ => say("done"),
    });
    let history = drive_with_checkpoint(&model, &registry, &checkpoint).await;
    assert_eq!(history.len(), 10);
    let errors: Vec<bool> = [2, 4, 6, 8]
        .iter()
        .map(|&at| match &history[at] {
            Message::ToolResults(results) => results[0].is_error,
            _ => panic!("no results"),
        })
        .collect();
    assert_eq!(errors, vec![true, false, true, true]);
    assert!(!checkpoint.active());
}

/// A rewind is written to the journal, so a reopened conversation has the report and not the exploration.
#[tokio::test]
async fn a_rewind_is_saved_to_the_journal() {
    let dir = crate::testkit::scratch("checkpoint-journal");
    let path = dir.join("history.jsonl");
    let (journal, mut history) = crate::Journal::open(&path, None).unwrap();
    let checkpoint = Arc::new(Checkpoint::new());
    let registry = checkpoint_registry(&dir, &checkpoint);
    let step = std::sync::atomic::AtomicUsize::new(0);
    let model =
        Router::new(
            move |_| match step.fetch_add(1, std::sync::atomic::Ordering::SeqCst) {
                0 => call("c", "checkpoint", json!({})),
                1 => call("g", "glob", json!({"pattern": "*"})),
                2 => call("r", "rewind", json!({"report": "summary"})),
                _ => say("done"),
            },
        );
    let approvals = deny_all();
    crate::run_turn_recorded(
        &Context {
            model: &*model,
            registry: &registry,
            approvals: &approvals,
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: Some(&checkpoint),
            hooks: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
        Some(&journal.scribe()),
    )
    .await
    .unwrap();
    drop(journal);
    let (_, reopened) = crate::Journal::open(&path, None).unwrap();
    assert_eq!(reopened, history);
    assert_eq!(reopened.len(), 5);
}

/// A turn that fails leaves no checkpoint behind.
#[tokio::test]
async fn a_failed_turn_clears_the_checkpoint() {
    /// A model whose every request fails.
    struct Failing;
    #[async_trait::async_trait]
    impl omc_providers::ChatModel for Failing {
        /// Marks nothing and fails.
        async fn complete(
            &self,
            _: omc_providers::Request<'_>,
            _: &mut dyn omc_providers::Sink,
        ) -> Result<omc_providers::Reply, omc_providers::ProviderError> {
            Err(omc_providers::ProviderError::Stream("boom".into()))
        }
    }
    let dir = crate::testkit::scratch("checkpoint-failed");
    let checkpoint = Arc::new(Checkpoint::new());
    let registry = checkpoint_registry(&dir, &checkpoint);
    checkpoint.begin().unwrap();
    checkpoint.settle(1);
    let mut history = Vec::new();
    let result = run_turn(
        &Context {
            model: &Failing,
            registry: &registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: Some(&checkpoint),
            hooks: None,
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await;
    assert!(result.is_err());
    assert!(!checkpoint.active());
    assert!(history.is_empty());
}
