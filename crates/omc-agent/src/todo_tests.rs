//! Tests for keeping the model's todo list current, driven through the agent loop.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_providers::{ChatModel, Message, ProviderError, Reply, Request, Sink};
use omc_tools::{Registry, Todo};
use serde_json::json;

use crate::testkit::{DenyAll, Reader, call, say, scratch};
use crate::todos::CHANGES;
use crate::{Approvals, Context, run_turn};

/// A model that gives scripted replies in order, then says `done` once they run out, and keeps the messages of every request.
struct Replies {
    /// The replies still to give.
    left: Mutex<VecDeque<Reply>>,
    /// The messages of every request, in order.
    seen: Mutex<Vec<Vec<Message>>>,
}

impl Replies {
    /// A model giving `replies` in order.
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            left: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChatModel for Replies {
    /// Records the request and gives the next reply.
    async fn complete(
        &self,
        request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.seen.lock().unwrap().push(request.messages.to_vec());
        Ok(self
            .left
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| say("done")))
    }
}

/// Runs one turn of `go` against `model` with the built-in tools and `todo` in `dir`, everything allowed, and returns the history.
async fn turn(model: &Replies, dir: &Path) -> Vec<Message> {
    let mut registry = Registry::builtin(dir).unwrap();
    registry.register(Todo::new(Arc::new(Reader::default())));
    let approvals = Approvals::new(Arc::new(DenyAll), true);
    let mut history = Vec::new();
    run_turn(
        &Context {
            model,
            registry: &registry,
            approvals: &approvals,
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: None,
        },
        &mut history,
        "go".into(),
        &|_| {},
    )
    .await
    .unwrap();
    history
}

/// A reply that sets a list with one item in progress and one pending.
fn plan() -> Reply {
    call(
        "t",
        "todo",
        json!({"todos": [
            {"content": "fix the parser", "status": "in_progress"},
            {"content": "add a test", "status": "pending"}
        ]}),
    )
}

/// The `round`th reply that writes a file.
fn writing(round: u32) -> Reply {
    call(
        &format!("w{round}"),
        "write",
        json!({"path": "out.txt", "content": format!("{round}\n")}),
    )
}

/// The text of the last result of each batch of tool results in `history`.
fn last_results(history: &[Message]) -> Vec<String> {
    history
        .iter()
        .filter_map(|message| match message {
            Message::ToolResults(results) => results.last().map(|result| result.text.clone()),
            _ => None,
        })
        .collect()
}

/// After enough writes with the list untouched, the results of the batch that reached the count carry a reminder listing the open items, which is saved with them.
#[tokio::test]
async fn a_stale_list_earns_a_reminder() {
    let dir = scratch("todo-reminder");
    let mut replies = vec![plan()];
    replies.extend((1..=CHANGES).map(writing));
    replies.push(say("Shall I also update the docs?"));
    let model = Replies::new(replies);
    let history = turn(&model, &dir).await;
    let texts = last_results(&history);
    assert_eq!(texts.len(), CHANGES as usize + 1);
    assert!(
        texts[..CHANGES as usize]
            .iter()
            .all(|text| !text.contains("harness-notice"))
    );
    let reminder = texts.last().unwrap();
    assert!(
        reminder.contains(&format!("{CHANGES} file-changing tool calls")),
        "{reminder}"
    );
    assert!(reminder.contains("- [in progress] fix the parser\n- [pending] add a test"));
    assert_eq!(
        model.seen.lock().unwrap().len(),
        CHANGES as usize + 2,
        "a closing question ends the turn without a nudge"
    );
}

/// A turn about to end with open items after changing files is nudged once, with a notice only the model sees, and the reply that carries on is kept as one with the first.
#[tokio::test]
async fn ending_with_open_items_is_nudged_once() {
    let dir = scratch("todo-finish");
    let model = Replies::new(vec![
        plan(),
        writing(1),
        say("Fixed the parser."),
        say("The test is left for later."),
    ]);
    let history = turn(&model, &dir).await;
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 4);
    let Some(Message::User(notice)) = seen[3].last() else {
        panic!("no notice: {:?}", seen[3].last());
    };
    assert!(notice.contains("still open"), "{notice}");
    assert!(notice.contains("- [pending] add a test"), "{notice}");
    let Some(Message::Assistant(answer)) = history.last() else {
        panic!("no answer");
    };
    assert_eq!(
        answer.text,
        "Fixed the parser.\n\nThe test is left for later."
    );
    assert!(
        !history
            .iter()
            .any(|message| matches!(message, Message::User(text) if text.contains("still open")))
    );
}
