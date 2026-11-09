//! Tests for compacting the history inside a turn and on request.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use omc_providers::{ChatModel, Message, ProviderError, Reply, Request, Sink, Tokens};
use omc_tools::Registry;

use crate::testkit::deny_all;
use crate::{AgentError, AgentEvent, Context, compact, context, run_turn, run_turn_recorded};

/// What a [`Script`] answers with: a summary request is told apart from a turn's request by its system prompt.
struct Script {
    /// The answers to the turn's requests, in order.
    turns: Mutex<VecDeque<Result<Reply, ProviderError>>>,
    /// The answers to summary requests, in order; a summary is `summary N` when they run out.
    summaries: Mutex<VecDeque<Result<Reply, ProviderError>>>,
    /// Every request seen, as whether it asked for a summary and the messages it carried.
    seen: Mutex<Vec<(bool, Vec<Message>)>>,
}

impl Script {
    /// A model that answers the turn with `turns` and summaries with `summaries`.
    fn new(
        turns: Vec<Result<Reply, ProviderError>>,
        summaries: Vec<Result<Reply, ProviderError>>,
    ) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            summaries: Mutex::new(summaries.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// The requests that were not summary requests, as the messages they carried.
    fn turn_requests(&self) -> Vec<Vec<Message>> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(summary, _)| !summary)
            .map(|(_, messages)| messages.clone())
            .collect()
    }

    /// How many summary requests were made.
    fn summary_requests(&self) -> usize {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter(|(summary, _)| *summary)
            .count()
    }
}

#[async_trait]
impl ChatModel for Script {
    /// Records the request and returns the next scripted answer of its kind.
    async fn complete(
        &self,
        request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let summary = request.system.contains("checkpoint summaries");
        self.seen
            .lock()
            .unwrap()
            .push((summary, request.messages.to_vec()));
        if summary {
            let count = self.summary_requests();
            return self
                .summaries
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Ok(Reply {
                        text: format!("summary {count}"),
                        ..Reply::default()
                    })
                });
        }
        self.turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("an answer for every request of the turn")
    }
}

/// A reply that says `text`.
fn say(text: &str) -> Result<Reply, ProviderError> {
    Ok(Reply {
        text: text.to_owned(),
        ..Reply::default()
    })
}

/// The refusal a provider gives a request longer than the window.
fn too_long() -> Result<Reply, ProviderError> {
    Err(ProviderError::Status {
        status: "400 Bad Request".to_owned(),
        body: "prompt is too long: 250000 tokens > 200000 maximum".to_owned(),
        retry_after: None,
    })
}

/// Two earlier turns whose prompts are about `tokens` tokens each, the last reply counting the request it answered as `counted` tokens.
fn earlier_turns(tokens: usize, counted: u64) -> Vec<Message> {
    vec![
        Message::User("a".repeat(tokens * 4)),
        Message::Assistant(Reply {
            text: "first done".into(),
            ..Reply::default()
        }),
        Message::User("b".repeat(tokens * 4)),
        Message::Assistant(Reply {
            text: "second done".into(),
            tokens: Some(Tokens {
                input: counted,
                ..Tokens::default()
            }),
            ..Reply::default()
        }),
    ]
}

/// A turn context on `model` with no tools and a window of `window` tokens.
fn context_on<'a>(
    model: &'a Script,
    registry: &'a Registry,
    approvals: &'a crate::Approvals,
    window: Option<u64>,
) -> Context<'a> {
    Context {
        model,
        registry,
        approvals,
        system: "",
        effort: None,
        refresh: None,
        notebook: None,
        window,
        checkpoint: None,
        hooks: None,
    }
}

/// The text of the first message of `messages`, which after a compaction is the summary.
fn first_text(messages: &[Message]) -> &str {
    match &messages[0] {
        Message::User(text) => text,
        other => panic!("not a user message: {other:?}"),
    }
}

/// A context past the threshold is compacted before the next request, which carries the summary, the kept turn and the prompt, while the history keeps every message.
#[tokio::test]
async fn compacts_past_the_threshold_before_the_request() {
    let model = Script::new(vec![say("answered")], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = earlier_turns(500, 900);
    let events = Mutex::new(Vec::new());
    run_turn(
        &context_on(&model, &registry, &approvals, Some(1_000)),
        &mut history,
        "next".into(),
        &|event| events.lock().unwrap().push(event),
    )
    .await
    .unwrap();
    assert_eq!(model.summary_requests(), 1);
    let requests = model.turn_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].len(), 4);
    assert!(first_text(&requests[0]).contains("<summary>\nsummary 1\n</summary>"));
    assert_eq!(requests[0][1], history[2]);
    assert_eq!(history.len(), 7);
    let Message::Compaction(compaction) = &history[5] else {
        panic!("no compaction recorded");
    };
    assert_eq!(compaction.first_kept, 2);
    assert_eq!(compaction.tokens_before, 901);
    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolDone { name, is_error: false, text, .. } if name == "compact" && text == "summary 1"
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::Used(tokens) if tokens.context() > 0 && tokens.context() < 850
    )));
}

/// A request the provider refuses as too long is compacted and sent again, once.
#[tokio::test]
async fn an_overflow_is_compacted_and_retried() {
    let model = Script::new(vec![too_long(), say("answered")], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = earlier_turns(25_000, 10);
    run_turn(
        &context_on(&model, &registry, &approvals, None),
        &mut history,
        "next".into(),
        &|_| {},
    )
    .await
    .unwrap();
    let requests = model.turn_requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].len(), 5);
    assert!(first_text(&requests[1]).contains("summary 1"));
    assert!(matches!(history.last(), Some(Message::Assistant(reply)) if reply.text == "answered"));
}

/// A second overflow in the same turn fails it, and the history goes back to what it was before the turn.
#[tokio::test]
async fn a_second_overflow_fails_the_turn() {
    let model = Script::new(vec![too_long(), too_long()], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = earlier_turns(25_000, 10);
    let before = history.clone();
    let error = run_turn(
        &context_on(&model, &registry, &approvals, None),
        &mut history,
        "next".into(),
        &|_| {},
    )
    .await
    .unwrap_err();
    assert!(matches!(error, AgentError::Provider(_)));
    assert_eq!(history, before);
}

/// A summary that cannot be made when the provider refused the request fails the turn with both reasons.
#[tokio::test]
async fn a_failed_summary_fails_an_overflowed_turn() {
    let model = Script::new(vec![too_long()], vec![Err(ProviderError::NoModels)]);
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = earlier_turns(25_000, 10);
    let error = run_turn(
        &context_on(&model, &registry, &approvals, None),
        &mut history,
        "next".into(),
        &|_| {},
    )
    .await
    .unwrap_err();
    let AgentError::Compaction(reason) = error else {
        panic!("not a compaction failure: {error}");
    };
    assert!(reason.contains("no models") && reason.contains("prompt is too long"));
    assert_eq!(history.len(), 4);
}

/// A summary that cannot be made at the threshold is shown, not tried again, and the turn goes on without it.
#[tokio::test]
async fn a_failed_summary_at_the_threshold_lets_the_turn_go_on() {
    let model = Script::new(
        vec![
            Ok(crate::testkit::call("r", "nothing", serde_json::json!({}))),
            say("answered"),
        ],
        vec![say("")],
    );
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = earlier_turns(500, 900);
    let failures = Mutex::new(0);
    run_turn(
        &context_on(&model, &registry, &approvals, Some(1_000)),
        &mut history,
        "next".into(),
        &|event| {
            if let AgentEvent::ToolDone {
                name,
                is_error: true,
                ..
            } = &event
                && name == "compact"
            {
                *failures.lock().unwrap() += 1;
            }
        },
    )
    .await
    .unwrap();
    assert_eq!(model.summary_requests(), 1);
    assert_eq!(*failures.lock().unwrap(), 1);
    assert!(
        !history
            .iter()
            .any(|message| matches!(message, Message::Compaction(_)))
    );
}

/// A compaction is saved with the conversation, so a reopened conversation gives the model the summary again.
#[tokio::test]
async fn a_compaction_is_saved_in_the_journal() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-agent/journal/compaction.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
    let (journal, mut history) = crate::Journal::open(&path, None).unwrap();
    let scribe = journal.scribe();
    for message in earlier_turns(500, 900) {
        scribe.append(&message).unwrap();
        history.push(message);
    }
    let model = Script::new(vec![say("answered")], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    run_turn_recorded(
        &context_on(&model, &registry, &approvals, Some(1_000)),
        &mut history,
        "next".into(),
        &|_| {},
        Some(&scribe),
    )
    .await
    .unwrap();
    drop(scribe);
    drop(journal);
    let (_, reopened) = crate::Journal::open(&path, None).unwrap();
    assert_eq!(reopened, history);
    let given = context(&reopened);
    assert!(first_text(&given).contains("summary 1"));
    assert_eq!(given.len(), 5);
}

/// A compaction asked for that fails is saved as a lost turn with its reason, so its mark does not stay behind for the next turn, and one that succeeds records how full the context is after it.
#[tokio::test]
async fn a_failed_compaction_is_saved_as_a_lost_turn() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-agent/journal/compaction-lost.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
    let (journal, mut history) = crate::Journal::open(&path, None).unwrap();
    let scribe = journal.scribe();
    for message in earlier_turns(500, 900) {
        scribe.append(&message).unwrap();
        history.push(message);
    }
    let model = Script::new(Vec::new(), vec![say("")]);
    let registry = Registry::new();
    let approvals = deny_all();
    let context = context_on(&model, &registry, &approvals, Some(1_000));
    let mark = |id: &str| crate::Mark {
        id: id.to_owned(),
        text: "/compact".to_owned(),
        files: Vec::new(),
    };
    journal.mark(history.len(), &mark("failed")).unwrap();
    compact(&context, &mut history, None, &|_| {}, Some(&scribe))
        .await
        .unwrap_err();
    assert!(journal.marks().is_empty());
    let lost = journal.lost();
    assert_eq!(lost.len(), 1);
    assert!(lost[0].reason.contains("empty summary"));
    let scribe = journal.scribe();
    journal.mark(history.len(), &mark("done")).unwrap();
    compact(&context, &mut history, None, &|_| {}, Some(&scribe))
        .await
        .unwrap();
    let Some(Message::Compaction(done)) = history.last() else {
        panic!("no compaction recorded");
    };
    assert!(done.tokens_after > 0 && done.tokens_after < done.tokens_before);
    assert_eq!(journal.marks(), vec![(4, mark("done"))]);
}

/// Compacting on request summarizes all but the recent messages, and says so when there is too little to compact.
#[tokio::test]
async fn compacts_on_request() {
    let model = Script::new(Vec::new(), Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let context = context_on(&model, &registry, &approvals, Some(1_000));
    let mut short = vec![Message::User("hi".into())];
    let error = compact(&context, &mut short, None, &|_| {}, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("nothing to compact"));
    assert_eq!(short.len(), 1);
    let mut history = earlier_turns(500, 10);
    compact(&context, &mut history, Some("the API"), &|_| {}, None)
        .await
        .unwrap();
    assert!(matches!(history.last(), Some(Message::Compaction(c)) if c.summary == "summary 1"));
    let seen = model.seen.lock().unwrap();
    assert!(first_text(&seen[0].1).contains("Give particular attention to: the API"));
}

/// A reply that calls the tool `name` with `arguments` under the id `id`.
fn calling(id: &str, name: &str, arguments: serde_json::Value) -> Message {
    Message::Assistant(Reply {
        tool_calls: vec![omc_providers::ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments,
        }],
        ..Reply::default()
    })
}

/// The output of the call `id` to `bash`: `fill` repeated to about `tokens` tokens.
fn bash_output(id: &str, fill: char, tokens: usize) -> Message {
    Message::ToolResults(vec![omc_providers::ToolResult {
        call_id: id.to_owned(),
        name: "bash".to_owned(),
        text: fill.to_string().repeat(tokens * 4),
        is_error: false,
        images: Vec::new(),
    }])
}

/// Past the threshold, old tool outputs are left out first, and when that makes enough room no summary is asked for; the model can read a left-out output again with `recall`.
#[tokio::test]
async fn old_outputs_are_pruned_before_a_summary() {
    let model = Script::new(
        vec![
            Ok(Reply {
                tool_calls: vec![omc_providers::ToolCall {
                    id: "r".into(),
                    name: crate::RECALL.into(),
                    arguments: serde_json::json!({"call_id": "a"}),
                }],
                ..Reply::default()
            }),
            say("answered"),
        ],
        Vec::new(),
    );
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = vec![
        Message::User("look".into()),
        calling("a", "bash", serde_json::json!({"command": "one"})),
        bash_output("a", 'a', 60_000),
        calling("b", "bash", serde_json::json!({"command": "two"})),
        bash_output("b", 'b', 60_000),
        calling("c", "bash", serde_json::json!({"command": "three"})),
        bash_output("c", 'c', 50_000),
        Message::Assistant(Reply {
            text: "looked".into(),
            tokens: Some(Tokens {
                input: 175_000,
                ..Tokens::default()
            }),
            ..Reply::default()
        }),
    ];
    run_turn(
        &context_on(&model, &registry, &approvals, Some(200_000)),
        &mut history,
        "next".into(),
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(model.summary_requests(), 0);
    let requests = model.turn_requests();
    let Message::ToolResults(first) = &requests[0][2] else {
        panic!("not tool results");
    };
    assert!(
        first[0].text.starts_with("[Output elided") && first[0].text.contains("\"a\""),
        "{}",
        first[0].text
    );
    assert_eq!(requests[0][6], history[6], "the recent output is kept");
    let Some(Message::ToolResults(recalled)) = requests[1].last() else {
        panic!("no recall result");
    };
    assert!(!recalled[0].is_error, "{}", recalled[0].text);
    assert!(recalled[0].text.contains(&"a".repeat(1_000)));
    assert!(
        history
            .iter()
            .any(|message| matches!(message, Message::Pruning(p) if p.elided.len() == 2 && p.tokens_after < 170_000))
    );
}

/// Past the threshold, when leaving out old outputs is not enough, a large code block in an older reply is shaken out instead of asking for a summary; the shake is saved, so a reopened conversation gives the model the same, and `recall` still reads the block whole.
#[tokio::test]
async fn an_old_code_block_is_shaken_out_before_a_summary() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-agent/journal/shake.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let _ = std::fs::remove_file(&path);
    let (journal, mut history) = crate::Journal::open(&path, None).unwrap();
    let scribe = journal.scribe();
    let code = format!(
        "Here it is:\n```rust\n{}```",
        "let value = compute(input, 0123456789);\n".repeat(10_000)
    );
    for message in [
        Message::User("write it".into()),
        Message::Assistant(Reply {
            text: code.clone(),
            ..Reply::default()
        }),
        Message::User("now run it".into()),
        calling("c", "bash", serde_json::json!({"command": "run"})),
        bash_output("c", 'c', 30_000),
        Message::Assistant(Reply {
            text: "ran".into(),
            tokens: Some(Tokens {
                input: 175_000,
                ..Tokens::default()
            }),
            ..Reply::default()
        }),
    ] {
        scribe.append(&message).unwrap();
        history.push(message);
    }
    let model = Script::new(
        vec![
            Ok(Reply {
                tool_calls: vec![omc_providers::ToolCall {
                    id: "r".into(),
                    name: crate::RECALL.into(),
                    arguments: serde_json::json!({"messages": "1"}),
                }],
                ..Reply::default()
            }),
            say("answered"),
        ],
        Vec::new(),
    );
    let registry = Registry::new();
    let approvals = deny_all();
    run_turn_recorded(
        &context_on(&model, &registry, &approvals, Some(200_000)),
        &mut history,
        "next".into(),
        &|_| {},
        Some(&scribe),
    )
    .await
    .unwrap();
    assert_eq!(model.summary_requests(), 0);
    let requests = model.turn_requests();
    let Message::Assistant(shaken) = &requests[0][1] else {
        panic!("not a reply");
    };
    assert_eq!(
        shaken.text,
        "Here it is:\n[10000 lines of rust block removed to save room. Call recall with messages \"1\" to read them again.]"
    );
    assert_eq!(requests[0][4], history[4], "the recent output is kept");
    let Some(Message::ToolResults(recalled)) = requests[1].last() else {
        panic!("no recall result");
    };
    assert!(recalled[0].text.contains("let value = compute"));
    assert!(
        history
            .iter()
            .any(|message| matches!(message, Message::Pruning(p) if p.excised.len() == 1 && p.tokens_after < 170_000))
    );
    drop(scribe);
    drop(journal);
    let (_, reopened) = crate::Journal::open(&path, None).unwrap();
    assert_eq!(reopened, history);
    assert_eq!(context(&reopened), context(&history));
}

/// Checks that answer every compaction as scripted and keep what they were asked.
struct Guarding {
    /// What every compaction comes to.
    answer: crate::Compacting,
    /// What each compaction was started by and the reader's focus.
    asked: Mutex<Vec<(crate::CompactTrigger, Option<String>)>>,
}

impl Guarding {
    /// Checks that answer every compaction with `answer`.
    fn new(answer: crate::Compacting) -> Self {
        Self {
            answer,
            asked: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl crate::Hooks for Guarding {
    /// Lets every call pass.
    async fn before_tool(
        &self,
        _call: &omc_providers::ToolCall,
        _arguments: &serde_json::Value,
    ) -> crate::Screened {
        crate::Screened::default()
    }

    /// Adds no note.
    async fn after_tool(
        &self,
        _call: &omc_providers::ToolCall,
        _arguments: &serde_json::Value,
        _output: &str,
    ) -> crate::Followed {
        crate::Followed::default()
    }

    /// Lets the turn end.
    async fn stop(&self, _answer: &str, _again: bool) -> Option<String> {
        None
    }

    /// Records what started the compaction and answers as scripted.
    async fn before_compact(
        &self,
        trigger: crate::CompactTrigger,
        focus: Option<&str>,
    ) -> crate::Compacting {
        self.asked
            .lock()
            .unwrap()
            .push((trigger, focus.map(str::to_owned)));
        self.answer.clone()
    }

    /// The same checks.
    fn for_subagent(&self, _id: &str, _kind: &str) -> std::sync::Arc<dyn crate::Hooks> {
        std::sync::Arc::new(Self::new(self.answer.clone()))
    }
}

/// A compaction on request asks the hooks with the reader's focus, and the instructions they add reach the summary after the focus; a hook that blocks it leaves the history as it was and says why.
#[tokio::test]
async fn hooks_shape_or_block_a_compaction_on_request() {
    let model = Script::new(Vec::new(), Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let adding = Guarding::new(crate::Compacting::Go(Some("keep the todo list".into())));
    let context = Context {
        hooks: Some(&adding),
        ..context_on(&model, &registry, &approvals, Some(1_000))
    };
    let mut history = earlier_turns(500, 10);
    compact(&context, &mut history, Some("the API"), &|_| {}, None)
        .await
        .unwrap();
    assert_eq!(
        *adding.asked.lock().unwrap(),
        vec![(crate::CompactTrigger::Manual, Some("the API".to_owned()))]
    );
    let summary = first_text(&model.seen.lock().unwrap()[0].1).to_owned();
    assert!(
        summary.contains("Give particular attention to: the API\n\nkeep the todo list"),
        "{summary}"
    );
    let blocking = Guarding::new(crate::Compacting::Blocked("saving first".into()));
    let context = Context {
        hooks: Some(&blocking),
        ..context_on(&model, &registry, &approvals, Some(1_000))
    };
    let mut history = earlier_turns(500, 10);
    let error = compact(&context, &mut history, None, &|_| {}, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("saving first"), "{error}");
    assert_eq!(history.len(), 4);
    assert_eq!(model.summary_requests(), 1);
}

/// A compaction past the threshold asks the hooks as an automatic one; when they block it, the turn goes on uncompacted and they are not asked again in that turn.
#[tokio::test]
async fn a_blocked_automatic_compaction_lets_the_turn_go_on() {
    let model = Script::new(vec![say("answered")], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let blocking = Guarding::new(crate::Compacting::Blocked("not now".into()));
    let context = Context {
        hooks: Some(&blocking),
        ..context_on(&model, &registry, &approvals, Some(1_000))
    };
    let mut history = earlier_turns(500, 900);
    run_turn(&context, &mut history, "next".into(), &|_| {})
        .await
        .unwrap();
    assert_eq!(model.summary_requests(), 0);
    assert_eq!(
        *blocking.asked.lock().unwrap(),
        vec![(crate::CompactTrigger::Auto, None)]
    );
    assert!(
        !history
            .iter()
            .any(|message| matches!(message, Message::Compaction(_)))
    );
}

/// A plan approved before the part a compaction summarizes is restated whole after the summary, so the model keeps working from it.
#[tokio::test]
async fn an_approved_plan_survives_a_compaction() {
    let model = Script::new(vec![say("answered")], Vec::new());
    let registry = Registry::new();
    let approvals = deny_all();
    let mut history = vec![
        Message::User("plan it".into()),
        calling(
            "p",
            "exit_plan_mode",
            serde_json::json!({"plan": "# Plan\n1. the step to keep"}),
        ),
        Message::ToolResults(vec![omc_providers::ToolResult {
            call_id: "p".into(),
            name: "exit_plan_mode".into(),
            text: "Plan approved.".into(),
            is_error: false,
            images: Vec::new(),
        }]),
    ];
    history.extend(earlier_turns(500, 900));
    run_turn(
        &context_on(&model, &registry, &approvals, Some(1_000)),
        &mut history,
        "next".into(),
        &|_| {},
    )
    .await
    .unwrap();
    assert_eq!(model.summary_requests(), 1);
    let requests = model.turn_requests();
    let summary = first_text(&requests[0]);
    assert!(
        summary.contains("<summary>\nsummary 1\n</summary>"),
        "{summary}"
    );
    assert!(
        summary.contains("<active-plan>") && summary.contains("1. the step to keep"),
        "{summary}"
    );
}
