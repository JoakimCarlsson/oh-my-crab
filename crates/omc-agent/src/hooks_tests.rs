//! Tests of the checks the user sets up around tool calls and the end of a turn, driven through the agent loop.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_context::builtin_agents;
use omc_providers::{Message, ToolCall};
use omc_tools::{Jobs, Registry, Tool};
use serde_json::{Value, json};

use crate::testkit::{Router, call, deny_all, read_args, say, scratch};
use crate::{
    AgentEvent, Batched, Context, EventSink, Followed, Gate, Hooks, MAX_STOP_IN_A_ROW, Permit,
    Screened, Task, TaskSettings, run_turn,
};

/// What scripted checks were shown, as who saw it, the event and the tool or answer.
type Seen = Arc<Mutex<Vec<(Option<String>, &'static str, String)>>>;

/// Checks that answer as a test scripts them and keep what they were shown.
#[derive(Default)]
struct Scripted {
    /// What every call is screened with, by tool name; a tool not listed passes.
    before: Vec<(&'static str, Screened)>,
    /// The note added after every successful call, when there is one.
    after: Option<&'static str>,
    /// The note added after every failed call, when there is one.
    failed: Option<&'static str>,
    /// The text that replaces the result of every successful call, when there is one.
    output: Option<&'static str>,
    /// What every call the reader is about to be asked about is answered with, when the checks answer.
    permit: Option<Permit>,
    /// Why the turn stops after every batch, when it should.
    batch_halt: Option<&'static str>,
    /// The context a subagent starts with, when there is some.
    start: Option<&'static str>,
    /// Why the turn should go on, when it should every time.
    stop: Option<&'static str>,
    /// Who the checks are for: nothing for the main agent, the subagent's id and kind otherwise.
    agent: Option<(String, String)>,
    /// What the checks were shown, as who saw it, the event and the tool or answer.
    seen: Seen,
}

impl Scripted {
    /// Records that `event` was checked for `what`.
    fn saw(&self, event: &'static str, what: &str) {
        let agent = self.agent.as_ref().map(|(id, kind)| format!("{kind}#{id}"));
        self.seen
            .lock()
            .unwrap()
            .push((agent, event, what.to_owned()));
    }
}

#[async_trait]
impl Hooks for Scripted {
    /// Screens the call as scripted for its tool.
    async fn before_tool(&self, call: &ToolCall, _arguments: &Value) -> Screened {
        self.saw("before", &call.name);
        self.before
            .iter()
            .find(|(name, _)| *name == call.name)
            .map(|(_, screened)| screened.clone())
            .unwrap_or_default()
    }

    /// Adds the scripted note.
    async fn after_tool(&self, call: &ToolCall, _arguments: &Value, _output: &str) -> Followed {
        self.saw("after", &call.name);
        Followed {
            note: self.after.map(str::to_owned),
            output: self.output.map(str::to_owned),
            halt: None,
        }
    }

    /// Adds the scripted note to a failure, recording the error.
    async fn after_tool_failed(
        &self,
        call: &ToolCall,
        _arguments: &Value,
        error: &str,
        _took: std::time::Duration,
    ) -> Followed {
        self.saw("failed", &format!("{}: {error}", call.name));
        Followed {
            note: self.failed.map(str::to_owned),
            ..Followed::default()
        }
    }

    /// Answers as scripted, recording the tool.
    async fn permission_request(&self, call: &ToolCall, _arguments: &Value) -> Permit {
        self.saw("permission", &call.name);
        self.permit.clone().unwrap_or_default()
    }

    /// Stops the turn as scripted, recording how many calls the batch had.
    async fn after_batch(
        &self,
        calls: &[ToolCall],
        _results: &[omc_providers::ToolResult],
    ) -> Batched {
        self.saw("batch", &calls.len().to_string());
        Batched {
            note: None,
            halt: self.batch_halt.map(str::to_owned),
        }
    }

    /// The scripted context, for a subagent only.
    async fn subagent_start(&self) -> Option<String> {
        self.agent.as_ref()?;
        self.saw("start", "");
        self.start.map(str::to_owned)
    }

    /// Asks to go on with the scripted reason, whenever there is one.
    async fn stop(&self, answer: &str, again: bool) -> Option<String> {
        self.saw(if again { "stop-again" } else { "stop" }, answer);
        self.stop.map(str::to_owned)
    }

    /// The same script, recording under the subagent's name.
    fn for_subagent(&self, id: &str, kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Scripted {
            before: self.before.clone(),
            after: self.after,
            failed: self.failed,
            output: self.output,
            permit: self.permit.clone(),
            batch_halt: self.batch_halt,
            start: self.start,
            stop: self.stop,
            agent: Some((id.to_owned(), kind.to_owned())),
            seen: self.seen.clone(),
        })
    }
}

/// Runs one turn of `go` with the built-in tools of `dir` against `model`, checked by `hooks`, with every approval denied; the history.
async fn drive(model: &Router, dir: &std::path::Path, hooks: &Scripted) -> Vec<Message> {
    let registry = Registry::builtin(dir).unwrap();
    let mut history = Vec::new();
    run_turn(
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
            hooks: Some(hooks),
        },
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    history
}

/// The text and failure of the first result of the tool results at `index` of `history`.
fn result(history: &[Message], index: usize) -> (String, bool) {
    let Message::ToolResults(results) = &history[index] else {
        panic!("no results at {index}");
    };
    (results[0].text.clone(), results[0].is_error)
}

/// A model that calls `write` with `arguments` once, then answers.
fn writer(arguments: Value) -> Arc<Router> {
    Router::new(move |seen| match seen.messages {
        1 => call("w", "write", arguments.clone()),
        _ => say("done"),
    })
}

/// A denied call never runs, and the model reads the hook's reason as the call's failure.
#[tokio::test]
async fn a_denied_call_does_not_run() {
    let dir = scratch("hooks-deny");
    let hooks = Scripted {
        before: vec![(
            "write",
            Screened {
                gate: Gate::Deny("no writes here".into()),
                ..Screened::default()
            },
        )],
        ..Scripted::default()
    };
    let history = drive(
        &writer(json!({"path": "a.txt", "content": "x"})),
        &dir,
        &hooks,
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(failed && text.contains("a hook blocked this tool call: no writes here"));
    assert!(!dir.join("a.txt").exists());
}

/// A hook that rewrites the arguments and allows the call has it run as rewritten, without the reader being asked, and its note reaches the model.
#[tokio::test]
async fn a_rewritten_call_runs_as_rewritten() {
    let dir = scratch("hooks-rewrite");
    let hooks = Scripted {
        before: vec![(
            "write",
            Screened {
                gate: Gate::Allow,
                arguments: Some(json!({"path": "safe.txt", "content": "x"})),
                note: Some("moved to safe.txt".into()),
                halt: None,
            },
        )],
        ..Scripted::default()
    };
    let history = drive(
        &writer(json!({"path": "a.txt", "content": "x"})),
        &dir,
        &hooks,
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(!failed, "{text}");
    assert!(
        text.ends_with("<hook-note>\nmoved to safe.txt\n</hook-note>"),
        "{text}"
    );
    assert!(dir.join("safe.txt").exists() && !dir.join("a.txt").exists());
}

/// Without a hook's allowance a call that needs approval is still asked about, and the reader's refusal stands.
#[tokio::test]
async fn passing_leaves_the_approval_to_the_reader() {
    let dir = scratch("hooks-pass");
    let history = drive(
        &writer(json!({"path": "a.txt", "content": "x"})),
        &dir,
        &Scripted::default(),
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(failed && text.contains("the user denied"));
}

/// Rewritten arguments that break the tool's schema fail the call instead of running it.
#[tokio::test]
async fn a_wrong_rewrite_fails_the_call() {
    let dir = scratch("hooks-bad-rewrite");
    let hooks = Scripted {
        before: vec![(
            "write",
            Screened {
                gate: Gate::Allow,
                arguments: Some(json!({"nothing": true})),
                note: None,
                halt: None,
            },
        )],
        ..Scripted::default()
    };
    let history = drive(
        &writer(json!({"path": "a.txt", "content": "x"})),
        &dir,
        &hooks,
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(
        failed && text.contains("rewrote the arguments wrongly"),
        "{text}"
    );
    assert!(!dir.join("a.txt").exists());
}

/// A note the hooks give after a call that succeeded is added to its result, which the model reads.
#[tokio::test]
async fn a_note_after_the_call_reaches_the_model() {
    let dir = scratch("hooks-after");
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let hooks = Scripted {
        after: Some("remember to run the tests"),
        ..Scripted::default()
    };
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "read", read_args("a.txt")),
        _ => say("done"),
    });
    let history = drive(&model, &dir, &hooks).await;
    let (text, failed) = result(&history, 2);
    assert!(!failed && text.contains("alpha"));
    assert!(text.ends_with("<hook-note>\nremember to run the tests\n</hook-note>"));
}

/// A stop hook that never lets go makes the turn go on only so many times in a row, and says so to the model each time.
#[tokio::test]
async fn a_stop_hook_is_bounded() {
    let dir = scratch("hooks-stop");
    let hooks = Scripted {
        stop: Some("the tests are not run yet"),
        ..Scripted::default()
    };
    let model = Router::new(|_| say("done"));
    let history = drive(&model, &dir, &hooks).await;
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 1 + MAX_STOP_IN_A_ROW);
    assert!(
        seen[1]
            .last_user
            .as_deref()
            .unwrap()
            .contains("the tests are not run yet")
    );
    let asked: Vec<&'static str> = hooks
        .seen
        .lock()
        .unwrap()
        .iter()
        .map(|(_, event, _)| *event)
        .collect();
    assert_eq!(asked.len(), MAX_STOP_IN_A_ROW);
    assert_eq!(asked[0], "stop");
    assert!(asked[1..].iter().all(|event| *event == "stop-again"));
    assert!(matches!(history.last(), Some(Message::Assistant(_))));
}

/// A subagent runs the same hooks, from its start on, under its own id and kind.
#[tokio::test]
async fn subagents_run_the_hooks_as_themselves() {
    let dir = scratch("hooks-subagent");
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let hooks = Arc::new(Scripted::default());
    let model = Router::new(|seen| match (seen.system.as_str(), seen.messages) {
        ("SUB:scout", 1) => call("s1", "read", read_args("a.txt")),
        _ => say("found alpha"),
    });
    let events: Arc<EventSink> = Arc::new(|_| {});
    let task = Task::new(TaskSettings {
        models: crate::Models::default(),
        nesting: crate::Nesting::default(),
        model,
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: Some(hooks.clone()),
    });
    let out = task
        .call_as("t1", json!({"agent": "scout", "task": "find alpha"}))
        .await
        .unwrap();
    assert!(out.text.contains("found alpha"));
    let seen = hooks.seen.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            (Some("scout#t1".to_owned()), "start", String::new()),
            (Some("scout#t1".to_owned()), "before", "read".to_owned()),
            (Some("scout#t1".to_owned()), "after", "read".to_owned()),
            (Some("scout#t1".to_owned()), "batch", "1".to_owned()),
            (
                Some("scout#t1".to_owned()),
                "stop",
                "found alpha".to_owned()
            ),
        ]
    );
}

/// A note the hooks give after a call that failed is added to the failure, which the model reads, and the hooks after a success are not asked.
#[tokio::test]
async fn a_note_after_a_failure_reaches_the_model() {
    let dir = scratch("hooks-failed");
    let hooks = Scripted {
        after: Some("never"),
        failed: Some("create the file first"),
        ..Scripted::default()
    };
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "read", read_args("missing.txt")),
        _ => say("done"),
    });
    let history = drive(&model, &dir, &hooks).await;
    let (text, failed) = result(&history, 2);
    assert!(failed, "{text}");
    assert!(
        text.ends_with("<hook-note>\ncreate the file first\n</hook-note>"),
        "{text}"
    );
    let seen = hooks.seen.lock().unwrap();
    assert!(
        seen.iter()
            .any(|(_, event, what)| *event == "failed" && what.starts_with("read: "))
    );
    assert!(!seen.iter().any(|(_, event, _)| *event == "after"));
}

/// The context the hooks give when a subagent starts comes before its assignment, in the first message it is sent.
#[tokio::test]
async fn a_subagent_starts_with_the_hooks_context() {
    let dir = scratch("hooks-subagent-start");
    let hooks = Arc::new(Scripted {
        start: Some("stay read-only"),
        ..Scripted::default()
    });
    let model = Router::new(|_| say("ok"));
    let events: Arc<EventSink> = Arc::new(|_| {});
    let task = Task::new(TaskSettings {
        model: model.clone(),
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents: builtin_agents(),
        effort: None,
        models: crate::Models::default(),
        nesting: crate::Nesting::default(),
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: Some(hooks.clone()),
    });
    task.call_as("t1", json!({"agent": "scout", "task": "find alpha"}))
        .await
        .unwrap();
    let seen = model.seen.lock().unwrap();
    let first = seen[0].last_user.as_deref().unwrap();
    assert!(
        first.starts_with("<subagent-start-hook>\nstay read-only\n</subagent-start-hook>\n\n"),
        "{first}"
    );
    assert!(first.contains("find alpha"));
    assert_eq!(hooks.seen.lock().unwrap()[0].1, "start");
}

/// Before the reader is asked, a check that allows with arguments of its own has the call run as rewritten without asking; one that denies has it refused with its reason; and a check that asks has the reader asked about a call that needs no approval.
#[tokio::test]
async fn a_permission_check_decides_for_the_reader() {
    let dir = scratch("hooks-permission");
    let allowing = Scripted {
        permit: Some(Permit::Allow {
            arguments: Some(json!({"path": "b.txt", "content": "y"})),
            rules: Vec::new(),
        }),
        ..Scripted::default()
    };
    let history = drive(
        &writer(json!({"path": "a.txt", "content": "x"})),
        &dir,
        &allowing,
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(!failed, "{text}");
    assert_eq!(std::fs::read_to_string(dir.join("b.txt")).unwrap(), "y");
    assert!(!dir.join("a.txt").exists());
    let denying = Scripted {
        permit: Some(Permit::Deny {
            reason: "not today".into(),
            halt: false,
        }),
        ..Scripted::default()
    };
    let history = drive(
        &writer(json!({"path": "c.txt", "content": "x"})),
        &dir,
        &denying,
    )
    .await;
    let (text, failed) = result(&history, 2);
    assert!(failed && text.contains("a hook denied this tool call: not today"));
    assert!(!dir.join("c.txt").exists());
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let asking = Scripted {
        before: vec![(
            "read",
            Screened {
                gate: Gate::Ask,
                ..Screened::default()
            },
        )],
        ..Scripted::default()
    };
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "read", read_args("a.txt")),
        _ => say("done"),
    });
    let history = drive(&model, &dir, &asking).await;
    let (text, failed) = result(&history, 2);
    assert!(failed && text.contains("the user denied"), "{text}");
}

/// A check after the call can replace what the model reads as its result.
#[tokio::test]
async fn a_check_replaces_the_result() {
    let dir = scratch("hooks-replace");
    std::fs::write(dir.join("a.txt"), "secret\n").unwrap();
    let hooks = Scripted {
        output: Some("[redacted]"),
        ..Scripted::default()
    };
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "read", read_args("a.txt")),
        _ => say("done"),
    });
    let history = drive(&model, &dir, &hooks).await;
    let (text, failed) = result(&history, 2);
    assert!(!failed);
    assert_eq!(text, "[redacted]");
}

/// A check after a batch that stops the turn ends it with the results, telling the model why, without another request.
#[tokio::test]
async fn a_batch_check_stops_the_turn() {
    let dir = scratch("hooks-batch");
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let hooks = Scripted {
        batch_halt: Some("enough for now"),
        ..Scripted::default()
    };
    let model = Router::new(|seen| match seen.messages {
        1 => call("r", "read", read_args("a.txt")),
        _ => say("done"),
    });
    let history = drive(&model, &dir, &hooks).await;
    assert_eq!(model.seen.lock().unwrap().len(), 1);
    assert_eq!(history.len(), 3);
    let (text, _) = result(&history, 2);
    assert!(
        text.contains("A hook stopped the turn: enough for now"),
        "{text}"
    );
    assert!(
        hooks
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|(_, event, what)| *event == "batch" && what == "1")
    );
}

/// Chained checks run in order: a rewrite of the first is what the second sees, the second's denial wins, and both notes after a call are kept.
#[tokio::test]
async fn chained_checks_run_in_order() {
    let first = Scripted {
        before: vec![(
            "write",
            Screened {
                gate: Gate::Allow,
                arguments: Some(json!({"path": "b.txt", "content": "x"})),
                ..Screened::default()
            },
        )],
        after: Some("first"),
        ..Scripted::default()
    };
    let second = Scripted {
        after: Some("second"),
        ..Scripted::default()
    };
    let chained = crate::Chained::of(vec![Arc::new(first), Arc::new(second)]).unwrap();
    let call = ToolCall {
        id: "w".into(),
        name: "write".into(),
        arguments: json!({"path": "a.txt", "content": "x"}),
    };
    let screened = chained.before_tool(&call, &call.arguments).await;
    assert_eq!(screened.gate, Gate::Allow);
    assert_eq!(
        screened.arguments,
        Some(json!({"path": "b.txt", "content": "x"}))
    );
    let followed = chained.after_tool(&call, &call.arguments, "ok").await;
    assert_eq!(followed.note.as_deref(), Some("first\n\nsecond"));
    let denying = Scripted {
        before: vec![(
            "write",
            Screened {
                gate: Gate::Deny("no".into()),
                ..Screened::default()
            },
        )],
        ..Scripted::default()
    };
    let chained =
        crate::Chained::of(vec![Arc::new(Scripted::default()), Arc::new(denying)]).unwrap();
    assert_eq!(
        chained.before_tool(&call, &call.arguments).await.gate,
        Gate::Deny("no".into())
    );
}
