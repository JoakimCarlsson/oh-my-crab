//! Tests of scripts driven through the agent loop: the tools a script calls are checked, approved, hooked and reported like the model's own calls.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_context::builtin_agents;
use omc_providers::{Message, ToolCall};
use omc_script::Script;
use omc_tools::{Jobs, Registry, Spill};
use serde_json::{Value, json};

use crate::testkit::{Router, call, deny_all, say, scratch};
use crate::{
    AgentEvent, ApprovalRequest, Approvals, Context, Decision, EventSink, Gate, Hooks, Prompter,
    Screened, Task, TaskSettings, run_turn,
};

/// A prompter that records what it is asked and answers with a fixed decision.
struct Recorder {
    /// What it answers.
    decision: Decision,
    /// What it was asked, in order.
    asked: Mutex<Vec<ApprovalRequest>>,
}

#[async_trait]
impl Prompter for Recorder {
    /// Records the request and answers with the fixed decision.
    async fn ask(&self, request: &ApprovalRequest) -> Decision {
        self.asked.lock().unwrap().push(request.clone());
        self.decision
    }
}

/// Hooks that block reading `secret.txt`, add a note after every `grep`, and record every call they see as `before:<tool>` or `after:<tool>`.
#[derive(Default)]
struct Guard {
    /// What the hooks saw.
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Hooks for Guard {
    /// Blocks reading `secret.txt` and passes everything else.
    async fn before_tool(&self, call: &ToolCall, arguments: &Value) -> Screened {
        self.seen
            .lock()
            .unwrap()
            .push(format!("before:{}", call.name));
        let secret = call.name == "read" && arguments["path"] == "secret.txt";
        Screened {
            gate: if secret {
                Gate::Deny("secrets stay secret".to_owned())
            } else {
                Gate::Pass
            },
            ..Screened::default()
        }
    }

    /// Adds a note after every `grep`.
    async fn after_tool(
        &self,
        call: &ToolCall,
        _arguments: &Value,
        _output: &str,
    ) -> crate::Followed {
        self.seen
            .lock()
            .unwrap()
            .push(format!("after:{}", call.name));
        crate::Followed {
            note: (call.name == "grep").then(|| "grep was watched".to_owned()),
            ..crate::Followed::default()
        }
    }

    /// Lets every turn end.
    async fn stop(&self, _answer: &str, _again: bool) -> Option<String> {
        None
    }

    /// The same hooks, which the tests here never ask for.
    fn for_subagent(&self, _id: &str, _kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Guard::default())
    }
}

/// The built-in tools of `dir` with a `script` tool fitted to them.
fn tools(dir: &std::path::Path) -> Registry {
    let mut registry = Registry::builtin(dir).unwrap();
    registry.register(Script::new(Spill::disabled()));
    registry.fitted()
}

/// A model that runs `code` as a script once, then answers.
fn scripter(code: &str) -> Arc<Router> {
    let code = code.to_owned();
    Router::new(move |seen| match seen.messages {
        1 => call("s1", "script", json!({ "code": code })),
        _ => say("done"),
    })
}

/// Runs one turn of `model` with `registry`, `approvals` and `hooks`, and returns the history and the events.
async fn drive(
    model: &Router,
    registry: &Registry,
    approvals: &Approvals,
    hooks: Option<&dyn Hooks>,
) -> (Vec<Message>, Vec<AgentEvent>) {
    let log = Mutex::new(Vec::new());
    let mut history = Vec::new();
    run_turn(
        &Context {
            model,
            registry,
            approvals,
            system: "MAIN",
            effort: None,
            refresh: None,
            notebook: None,
            window: None,
            checkpoint: None,
            hooks,
            routing: None,
        },
        &mut history,
        "go".into(),
        &|event| log.lock().unwrap().push(event),
    )
    .await
    .unwrap();
    (history, log.into_inner().unwrap())
}

/// The text and failure of the script's result in `history`.
fn outcome(history: &[Message]) -> (String, bool) {
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results in {history:?}");
    };
    (results[0].text.clone(), results[0].is_error)
}

/// The events filed under the script call `s1`, as the nested event and the kind of the call filing it.
fn nested(events: &[AgentEvent]) -> Vec<AgentEvent> {
    events
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Nested {
                parent,
                agent,
                event,
            } if parent == "s1" && agent == "script" => Some(*event.clone()),
            _ => None,
        })
        .collect()
}

/// The script's calls are reported as its children, numbered in order, and only its result reaches the model's history.
#[tokio::test]
async fn calls_are_reported_under_the_script() {
    let dir = scratch("script-nested");
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let model = scripter(
        "const text = await tools.read({ path: 'a.txt' });
         const { matches } = await tools.grep({ pattern: 'alpha' });
         return [text.includes('alpha'), matches[0].path];",
    );
    let (history, events) = drive(&model, &tools(&dir), &deny_all(), None).await;
    assert_eq!(
        outcome(&history),
        ("[\n  true,\n  \"a.txt\"\n]".to_owned(), false)
    );
    assert_eq!(history.len(), 4);
    let children = nested(&events);
    assert_eq!(children.len(), 4, "{children:?}");
    assert!(
        matches!(&children[0], AgentEvent::ToolStart { id, name, summary } if id == "1" && name == "read" && summary == "a.txt")
    );
    assert!(matches!(&children[1], AgentEvent::ToolDone { id, is_error: false, .. } if id == "1"));
    assert!(
        matches!(&children[2], AgentEvent::ToolStart { id, name, .. } if id == "2" && name == "grep")
    );
    assert!(matches!(
        events.first(),
        Some(AgentEvent::ToolStart { id, name, .. }) if id == "s1" && name == "script"
    ));
    assert!(matches!(
        events.iter().rev().find(|event| matches!(event, AgentEvent::ToolDone { .. })),
        Some(AgentEvent::ToolDone { id, is_error: false, .. }) if id == "s1"
    ));
}

/// Arguments are put right against the tool's schema, or turned away naming what is wrong, as for a direct call.
#[tokio::test]
async fn arguments_are_checked_like_direct_calls() {
    let dir = scratch("script-arguments");
    std::fs::write(dir.join("a.txt"), "one\n").unwrap();
    let model = scripter(
        "const found = await tools.grep({ pattern: 'one', limit: '5', case_insensitive: 'true' });
         let refused;
         try { await tools.grep({ limit: 'lots' }); } catch (error) { refused = error.message; }
         return { found: found.matches.length, refused };",
    );
    let (history, _) = drive(&model, &tools(&dir), &deny_all(), None).await;
    let (text, failed) = outcome(&history);
    assert!(!failed, "{text}");
    assert!(text.contains("\"found\": 1"), "{text}");
    assert!(
        text.contains("missing required property `pattern`"),
        "{text}"
    );
    assert!(
        text.contains("args.limit: expected integer, got string"),
        "{text}"
    );
}

/// A call that needs approval asks the reader, naming the script, and a denied one fails inside the script without running.
#[tokio::test]
async fn calls_are_approved_like_direct_calls() {
    let dir = scratch("script-approval");
    let model = scripter(
        "try { await tools.write({ path: 'out.txt', content: 'x' }); } catch (error) { return error.message; }
         return 'written';",
    );
    let recorder = Arc::new(Recorder {
        decision: Decision::Deny,
        asked: Mutex::new(Vec::new()),
    });
    let approvals = Approvals::new(recorder.clone(), false);
    let (history, events) = drive(&model, &tools(&dir), &approvals, None).await;
    assert_eq!(outcome(&history).0, "the user denied this tool call");
    assert!(!dir.join("out.txt").exists());
    let asked = recorder.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].tool, "write");
    assert!(
        asked[0].summary.ends_with("(called by script)"),
        "{}",
        asked[0].summary
    );
    assert!(nested(&events).iter().any(
        |event| matches!(event, AgentEvent::ToolDone { name, is_error: true, .. } if name == "write")
    ));
    let yolo = Approvals::new(recorder.clone(), true);
    let (history, _) = drive(&model, &tools(&dir), &yolo, None).await;
    assert_eq!(outcome(&history).0, "written");
    assert_eq!(std::fs::read_to_string(dir.join("out.txt")).unwrap(), "x");
}

/// The hooks see every call the script makes, can block one, which the script can catch, and their notes reach the script with the text.
#[tokio::test]
async fn calls_go_through_the_hooks() {
    let dir = scratch("script-hooks");
    std::fs::write(dir.join("secret.txt"), "hush\n").unwrap();
    let model = scripter(
        "let blocked;
         try { await tools.read({ path: 'secret.txt' }); } catch (error) { blocked = error.message; }
         const found = await tools.grep({ pattern: 'hush' });
         return { blocked, noted: typeof found === 'string' && found.includes('grep was watched') };",
    );
    let guard = Guard::default();
    let (history, _) = drive(&model, &tools(&dir), &deny_all(), Some(&guard)).await;
    let (text, failed) = outcome(&history);
    assert!(!failed, "{text}");
    assert!(
        text.contains("a hook blocked this tool call: secrets stay secret"),
        "{text}"
    );
    assert!(text.contains("\"noted\": true"), "{text}");
    assert_eq!(
        *guard.seen.lock().unwrap(),
        [
            "before:script",
            "before:read",
            "before:grep",
            "after:grep",
            "after:script"
        ]
    );
}

/// A script that fails, here on a tool error it does not catch, fails its call with the error.
#[tokio::test]
async fn an_uncaught_tool_error_fails_the_call() {
    let dir = scratch("script-failure");
    let model = scripter("print('looking'); await tools.read({ path: 'missing.txt' });");
    let (history, _) = drive(&model, &tools(&dir), &deny_all(), None).await;
    let (text, failed) = outcome(&history);
    assert!(failed);
    assert!(text.contains("ToolError"), "{text}");
    assert!(
        text.contains("Printed before it failed:\nlooking"),
        "{text}"
    );
}

/// A subagent with every tool gets its own script, whose calls are filed under the script and then under the subagent; a scout, whose tools are listed, gets none.
#[tokio::test]
async fn subagents_script_with_their_own_tools() {
    let dir = scratch("script-subagent");
    std::fs::write(dir.join("a.txt"), "alpha\n").unwrap();
    let model = Router::new(|seen| {
        if seen.system.starts_with("SUB:") {
            match seen.messages {
                1 => call(
                    "s1",
                    "script",
                    json!({ "code": "return await tools.read({ path: 'a.txt' });" }),
                ),
                _ => say("sub done"),
            }
        } else if seen.messages == 1 {
            let mut reply = call("t1", "task", json!({ "task": "look" }));
            reply.tool_calls.push(ToolCall {
                id: "t2".into(),
                name: "task".into(),
                arguments: json!({ "agent": "scout", "task": "look" }),
            });
            reply
        } else {
            say("done")
        }
    });
    let log: Arc<Mutex<Vec<AgentEvent>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let base = tools(&dir);
    let mut registry = base.clone();
    registry.register(Task::new(TaskSettings {
        models: crate::Models::default(),
        nesting: crate::Nesting::default(),
        model: model.clone(),
        registry: Arc::new(base),
        approvals: deny_all(),
        events,
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        hooks: None,
        isolation: None,
        jobs: Arc::new(Jobs::new(|_| {})),
    }));
    drive(&model, &registry, &deny_all(), None).await;
    let seen = model.seen.lock().unwrap();
    let tools_of = |system: &str| {
        seen.iter()
            .find(|request| request.system == system)
            .map(|request| request.tools.clone())
            .unwrap()
    };
    assert!(tools_of("SUB:task").contains(&"script".to_owned()));
    assert!(!tools_of("SUB:scout").contains(&"script".to_owned()));
    let log = log.lock().unwrap();
    let under_script = log.iter().any(|event| match event {
        AgentEvent::Nested { parent, event, .. } if parent == "t1" => matches!(
            event.as_ref(),
            AgentEvent::Nested { parent, agent, event }
                if parent == "s1" && agent == "script"
                    && matches!(event.as_ref(), AgentEvent::ToolStart { name, .. } if name == "read")
        ),
        _ => false,
    });
    assert!(under_script, "{log:?}");
}
