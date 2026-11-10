//! Tests of the messages the reader sends while a turn runs, driven through the agent loop.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use omc_providers::Message;
use omc_tools::Registry;
use serde_json::json;

use crate::testkit::{Router, call, deny_all, say, scratch};
use crate::{AgentError, AgentEvent, Context, Journal, Steering, run_turn_steered};

/// A queue the test fills, which keeps the position each message it handed out was given.
#[derive(Default)]
struct Waiting {
    /// The messages still waiting, oldest first.
    messages: Mutex<VecDeque<String>>,
    /// The position in the history each taken message was given, in order.
    taken_at: Mutex<Vec<usize>>,
}

impl Waiting {
    /// Queues `text`.
    fn push(&self, text: &str) {
        self.messages.lock().unwrap().push_back(text.to_owned());
    }
}

impl Steering for Waiting {
    /// Hands out the oldest message and keeps the position it was given.
    fn next(&self, at: usize) -> Option<(String, Vec<omc_providers::Image>)> {
        let text = self.messages.lock().unwrap().pop_front()?;
        self.taken_at.lock().unwrap().push(at);
        Some((text, Vec::new()))
    }
}

/// Runs one turn of `go` against `model` on the built-in tools of the scratch folder `folder`, which is given `a.txt`, taking messages from `steering` and recording into `journal`; each test passes a folder of its own, since tests run at the same time.
async fn steered_turn(
    folder: &str,
    model: &Router,
    history: &mut Vec<Message>,
    steering: &Waiting,
    journal: &Arc<Journal>,
) -> Result<(), AgentError> {
    let dir = scratch(folder);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let registry = Registry::builtin(&dir).unwrap();
    run_turn_steered(
        &Context {
            model,
            registry: &registry,
            approvals: &deny_all(),
            system: "",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: None,
            hooks: None,
        },
        history,
        "go".into(),
        Vec::new(),
        &|_: AgentEvent| {},
        Some(&journal.scribe()),
        Some(steering),
    )
    .await
}

/// A message queued while the tools of a round run goes in after their results and before the next request, is saved with the history, and the turn goes on to answer it; a message queued during the final answer is left waiting.
#[tokio::test]
async fn a_queued_message_is_put_in_after_the_tool_round() {
    let path = scratch("steering-journal").join("c.jsonl");
    let (journal, mut history) = Journal::open(&path, None).unwrap();
    let steering = Arc::new(Waiting::default());
    let queue = steering.clone();
    let model = Router::new(move |seen| match seen.messages {
        1 => {
            queue.push("also check b");
            call("r", "read", json!({ "path": "a.txt" }))
        }
        _ => {
            queue.push("too late");
            say("done")
        }
    });
    steered_turn(
        "steering-queued-tools",
        &model,
        &mut history,
        &steering,
        &journal,
    )
    .await
    .unwrap();
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].messages, 4);
    assert_eq!(seen[1].last_user.as_deref(), Some("also check b"));
    assert!(matches!(history[2], Message::ToolResults(_)));
    assert_eq!(history[3], Message::User("also check b".into()));
    assert_eq!(history.len(), 5);
    assert_eq!(*steering.taken_at.lock().unwrap(), vec![3]);
    assert_eq!(
        steering.messages.lock().unwrap().iter().collect::<Vec<_>>(),
        vec!["too late"]
    );
    drop(journal);
    let (_, reopened) = Journal::open(&path, None).unwrap();
    assert_eq!(reopened, history);
}

/// A turn that fails after a message was put in cuts that message back with the rest of the turn.
#[tokio::test]
async fn a_failed_turn_cuts_back_what_was_put_in() {
    let path = scratch("steering-failed").join("c.jsonl");
    let (journal, mut history) = Journal::open(&path, None).unwrap();
    let steering = Arc::new(Waiting::default());
    let queue = steering.clone();
    let model = Router::new(move |seen| match seen.messages {
        1 => {
            queue.push("and then");
            call("r", "read", json!({ "path": "a.txt" }))
        }
        _ => say(" "),
    });
    let blank = Router::new(|_| say(""));
    steered_turn(
        "steering-failed-tools",
        &model,
        &mut history,
        &steering,
        &journal,
    )
    .await
    .unwrap();
    assert_eq!(history.len(), 5);
    steering.push("never answered");
    let failed = steered_turn(
        "steering-failed-tools",
        &blank,
        &mut history,
        &steering,
        &journal,
    )
    .await;
    assert!(matches!(failed, Err(AgentError::Empty)));
    assert_eq!(history.len(), 5);
    drop(journal);
    let (_, reopened) = Journal::open(&path, None).unwrap();
    assert_eq!(reopened, history);
}
