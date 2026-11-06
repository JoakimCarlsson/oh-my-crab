//! Tests for getting a turn past failed requests, replies that stop short and calls that go round in circles.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use omc_providers::{
    ChatModel, Message, ProviderError, Reply, Request, Sink, Stop, ToolCall, ToolResult,
};
use omc_tools::Registry;
use serde_json::{Value, json};

use crate::recovery::{MAX_RECOVERIES, MAX_RETRIES};
use crate::testkit::{DenyAll, call, say, scratch};
use crate::{AgentError, AgentEvent, Approvals, Context, run_turn};

/// A model that gives scripted outcomes in order, then says `done` once they run out, and keeps the messages of every request.
struct Outcomes {
    /// The outcomes still to give.
    left: Mutex<VecDeque<Result<Reply, ProviderError>>>,
    /// The messages of every request, in order.
    seen: Mutex<Vec<Vec<Message>>>,
}

impl Outcomes {
    /// A model giving `outcomes` in order.
    fn new(outcomes: Vec<Result<Reply, ProviderError>>) -> Self {
        Self {
            left: Mutex::new(outcomes.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The messages of every request so far.
    fn requests(&self) -> Vec<Vec<Message>> {
        self.seen.lock().unwrap().clone()
    }
}

#[async_trait]
impl ChatModel for Outcomes {
    /// Records the request and gives the next outcome, streaming its text to `sink`.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.seen.lock().unwrap().push(request.messages.to_vec());
        let outcome = self
            .left
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(say("done")));
        if let Ok(reply) = &outcome
            && !reply.text.is_empty()
        {
            sink.text(&reply.text);
        }
        outcome
    }
}

/// A status error with `status` and `body`, asking for `retry_after` when it is given.
fn failure(status: &str, body: &str, retry_after: Option<Duration>) -> ProviderError {
    ProviderError::Status {
        status: status.to_owned(),
        body: body.to_owned(),
        retry_after,
    }
}

/// The overloaded refusal some providers give.
fn overloaded() -> Result<Reply, ProviderError> {
    Err(failure(
        "529 <unknown status code>",
        r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
        None,
    ))
}

/// `reply` marked as cut off by the output limit.
fn cut_off(reply: Reply) -> Reply {
    Reply {
        stop: Stop::Length,
        ..reply
    }
}

/// Runs one turn of `go` against `model` with the built-in tools in `dir`, everything allowed, and returns how it ended, the history and the events it reported.
async fn turn(
    model: &Outcomes,
    dir: &Path,
) -> (Result<(), AgentError>, Vec<Message>, Vec<AgentEvent>) {
    let registry = Registry::builtin(dir).unwrap();
    let approvals = Approvals::new(std::sync::Arc::new(DenyAll), true);
    let events = Mutex::new(Vec::new());
    let mut history = Vec::new();
    let ended = run_turn(
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
        &|event| events.lock().unwrap().push(event),
    )
    .await;
    (ended, history, events.into_inner().unwrap())
}

/// The tool results at `index` of `history`.
fn results(history: &[Message], index: usize) -> &[ToolResult] {
    let Message::ToolResults(results) = &history[index] else {
        panic!("expected tool results at {index}, got {:?}", history[index]);
    };
    results
}

/// The text of the last message of `messages`, a prompt or the last of a batch of tool results.
fn last_text(messages: &[Message]) -> String {
    match messages.last() {
        Some(Message::User(text)) => text.clone(),
        Some(Message::ToolResults(results)) => results.last().unwrap().text.clone(),
        other => panic!("expected a prompt or results, got {other:?}"),
    }
}

/// An overloaded provider is asked again after a wait shown as a `retry` call, and the turn goes on as if nothing happened.
#[tokio::test(start_paused = true)]
async fn an_overloaded_provider_is_asked_again() {
    let model = Outcomes::new(vec![overloaded(), overloaded(), Ok(say("fine"))]);
    let (ended, history, events) = turn(&model, &scratch("retry-overloaded")).await;
    ended.unwrap();
    assert_eq!(
        history,
        [Message::User("go".into()), Message::Assistant(say("fine"))]
    );
    assert_eq!(model.requests().len(), 3);
    let retries: Vec<&AgentEvent> = events
        .iter()
        .filter(|event| matches!(event, AgentEvent::ToolStart { name, .. } | AgentEvent::ToolDone { name, .. } if name == "retry"))
        .collect();
    assert_eq!(retries.len(), 4);
    let AgentEvent::ToolStart { summary, .. } = retries[0] else {
        panic!("expected the retry to start first, got {:?}", retries[0]);
    };
    assert!(
        summary.starts_with(&format!("attempt 1 of {MAX_RETRIES}")),
        "{summary}"
    );
    assert!(summary.contains("529"), "{summary}");
    let ids: Vec<&String> = retries
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolStart { id, .. } => Some(id),
            _ => None,
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_ne!(ids[0], ids[1]);
}

/// The wait the provider asks for is the wait taken.
#[tokio::test(start_paused = true)]
async fn the_wait_asked_for_is_kept_to() {
    let model = Outcomes::new(vec![
        Err(failure(
            "429 Too Many Requests",
            "slow down",
            Some(Duration::from_secs(7)),
        )),
        Ok(say("fine")),
    ]);
    let started = tokio::time::Instant::now();
    let (ended, _, _) = turn(&model, &scratch("retry-after")).await;
    ended.unwrap();
    assert_eq!(started.elapsed(), Duration::from_secs(7));
}

/// A refusal that waiting does not cure fails the turn at once.
#[tokio::test(start_paused = true)]
async fn a_lasting_refusal_is_not_retried() {
    let model = Outcomes::new(vec![Err(failure("401 Unauthorized", "invalid key", None))]);
    let (ended, history, _) = turn(&model, &scratch("retry-auth")).await;
    assert!(matches!(ended, Err(AgentError::Provider(_))));
    assert!(history.is_empty());
    assert_eq!(model.requests().len(), 1);
}

/// A provider that stays overloaded fails the turn once the attempts are used up.
#[tokio::test(start_paused = true)]
async fn retries_are_limited() {
    let outcomes = (0..=MAX_RETRIES + 1).map(|_| overloaded()).collect();
    let model = Outcomes::new(outcomes);
    let (ended, _, _) = turn(&model, &scratch("retry-limit")).await;
    assert!(matches!(ended, Err(AgentError::Provider(_))));
    assert_eq!(model.requests().len(), MAX_RETRIES as usize + 1);
}

/// A turn stopped while it waits to ask again does not ask again.
#[tokio::test(start_paused = true)]
async fn a_stopped_turn_stops_waiting() {
    let model = Outcomes::new(vec![overloaded(), Ok(say("fine"))]);
    let dir = scratch("retry-stopped");
    let stopped = tokio::time::timeout(Duration::from_millis(100), turn(&model, &dir)).await;
    assert!(stopped.is_err());
    assert_eq!(model.requests().len(), 1);
}

/// A dropped stream is asked again, without the part of the reply that came before it reaching the history.
#[tokio::test(start_paused = true)]
async fn a_dropped_stream_is_asked_again() {
    let model = Outcomes::new(vec![Err(ProviderError::Unfinished), Ok(say("whole"))]);
    let (ended, history, _) = turn(&model, &scratch("retry-dropped")).await;
    ended.unwrap();
    assert_eq!(history.len(), 2);
}

/// An empty reply is answered with a notice the model sees but the history does not keep, and the model's next reply is taken.
#[tokio::test]
async fn an_empty_reply_is_nudged() {
    let model = Outcomes::new(vec![Ok(Reply::default()), Ok(say("here it is"))]);
    let (ended, history, _) = turn(&model, &scratch("nudge-empty")).await;
    ended.unwrap();
    assert_eq!(
        history,
        [
            Message::User("go".into()),
            Message::Assistant(say("here it is"))
        ]
    );
    let requests = model.requests();
    assert_eq!(last_text(&requests[0]), "go");
    let nudged = last_text(&requests[1]);
    assert!(nudged.starts_with("go\n\n<harness-notice>"), "{nudged}");
    assert!(nudged.contains("empty"), "{nudged}");
}

/// A model that keeps answering with nothing is nudged a few times, then the turn fails.
#[tokio::test]
async fn nudges_are_limited() {
    let outcomes = (0..=MAX_RECOVERIES).map(|_| Ok(Reply::default())).collect();
    let model = Outcomes::new(outcomes);
    let (ended, history, _) = turn(&model, &scratch("nudge-limit")).await;
    assert!(matches!(ended, Err(AgentError::Empty)));
    assert!(history.is_empty());
    assert_eq!(model.requests().len(), MAX_RECOVERIES as usize + 1);
}

/// A reply the output limit cut off in its text is carried on, and the two are kept as one.
#[tokio::test]
async fn a_reply_cut_off_in_its_text_is_carried_on() {
    let model = Outcomes::new(vec![Ok(cut_off(say("Hello, wo"))), Ok(say("rld."))]);
    let (ended, history, _) = turn(&model, &scratch("nudge-length-text")).await;
    ended.unwrap();
    assert_eq!(
        history,
        [
            Message::User("go".into()),
            Message::Assistant(say("Hello, world."))
        ]
    );
    let carried = &model.requests()[1];
    assert_eq!(carried.len(), 3);
    assert_eq!(carried[1], Message::Assistant(cut_off(say("Hello, wo"))));
    assert!(last_text(carried).contains("output limit"));
}

/// The calls of a reply the output limit cut off are not run; the model is told so and to take a smaller step.
#[tokio::test]
async fn calls_cut_off_by_the_output_limit_are_not_run() {
    let dir = scratch("nudge-length-calls");
    let model = Outcomes::new(vec![
        Ok(cut_off(call(
            "w",
            "write",
            json!({ "path": "big.txt", "content": "half of it" }),
        ))),
        Ok(say("smaller then")),
    ]);
    let (ended, history, _) = turn(&model, &dir).await;
    ended.unwrap();
    assert!(!dir.join("big.txt").exists());
    let refused = results(&history, 2);
    assert!(refused[0].is_error);
    assert!(
        refused[0].text.starts_with("Not run"),
        "{}",
        refused[0].text
    );
    assert!(
        refused[0].text.contains("smaller step"),
        "{}",
        refused[0].text
    );
    assert_eq!(history.len(), 4);
}

/// Replies that keep running into the output limit in their calls fail the turn.
#[tokio::test]
async fn calls_that_keep_being_cut_off_fail_the_turn() {
    let outcomes = (0..=MAX_RECOVERIES)
        .map(|_| Ok(cut_off(call("w", "write", json!({ "path": "a" })))))
        .collect();
    let model = Outcomes::new(outcomes);
    let (ended, history, _) = turn(&model, &scratch("nudge-length-limit")).await;
    assert!(matches!(ended, Err(AgentError::OutputLimit)));
    assert!(history.is_empty());
}

/// A call whose arguments are not valid JSON is not run, and the model is told what is wrong with them.
#[tokio::test]
async fn unreadable_arguments_are_explained() {
    let dir = scratch("malformed");
    let model = Outcomes::new(vec![Ok(Reply {
        tool_calls: vec![ToolCall {
            id: "w".into(),
            name: "write".into(),
            arguments: Value::String("{\"path\": \"a.txt\", \"content\": \"x".into()),
        }],
        ..Reply::default()
    })]);
    let (ended, history, _) = turn(&model, &dir).await;
    ended.unwrap();
    assert!(!dir.join("a.txt").exists());
    let refused = &results(&history, 2)[0];
    assert!(refused.is_error);
    assert!(refused.text.contains("not valid JSON"), "{}", refused.text);
}

/// The same call made again and again earns a notice with its results telling the model to change course.
#[tokio::test]
async fn a_repeated_call_is_redirected() {
    let dir = scratch("loop-repeat");
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    let outcomes = (0..crate::loop_guard::REPEATS)
        .map(|_| Ok(call("r", "read", json!({ "path": "a.txt" }))))
        .collect();
    let model = Outcomes::new(outcomes);
    let (ended, history, _) = turn(&model, &dir).await;
    ended.unwrap();
    let before = &results(&history, history.len() - 4)[0];
    assert!(!before.text.contains("<harness-notice>"), "{}", before.text);
    let last = &results(&history, history.len() - 2)[0];
    assert!(last.text.contains("<harness-notice>"), "{}", last.text);
    assert!(last.text.contains("`read`"), "{}", last.text);
}

/// Edits that leave a file as it was earn a notice once they repeat.
#[tokio::test]
async fn edits_that_change_nothing_are_redirected() {
    let dir = scratch("loop-noop");
    std::fs::write(dir.join("f.txt"), "same\n").unwrap();
    let edit = |round: usize| {
        call(
            &format!("e{round}"),
            "edit",
            json!({ "files": [{ "path": "f.txt", "tag": omc_tools::snapshot_tag(b"same\n"), "ops": [{ "op": "replace", "line": 1, "text": "same" }] }] }),
        )
    };
    let outcomes = (0..crate::loop_guard::NO_OPS as usize)
        .map(|round| Ok(edit(round)))
        .collect();
    let model = Outcomes::new(outcomes);
    let (ended, history, _) = turn(&model, &dir).await;
    ended.unwrap();
    let last = &results(&history, history.len() - 2)[0];
    assert!(!last.is_error, "{}", last.text);
    assert!(last.text.contains("changed nothing"), "{}", last.text);
}
