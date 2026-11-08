//! Tests of how the calls of one reply are scheduled: shared calls next to each other overlap, exclusive calls run alone, in the model's order; and of hidden tools, which are left off the tool list yet run like the rest.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_providers::{Message, Reply, ToolCall};
use omc_tools::{Concurrency, Registry, Tool, ToolError, ToolOutput};
use serde_json::{Value, json};

use crate::testkit::{Router, deny_all, say};
use crate::{AgentEvent, Approvals, Context, run_turn};

/// A tool that logs when each call starts and ends, sleeping in between.
struct Timed {
    /// The name the model calls it by.
    name: &'static str,
    /// Whether its calls may overlap.
    class: Concurrency,
    /// Whether its calls ask the reader first.
    asks: bool,
    /// Where the starts and ends are logged, as `start <id>` and `end <id>`.
    log: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Tool for Timed {
    /// Returns the tool's name.
    fn name(&self) -> &'static str {
        self.name
    }

    /// Describes the tool.
    fn description(&self) -> &str {
        "logs its calls"
    }

    /// Takes the `id` the log names the call by.
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]})
    }

    /// Asks first when the test says so.
    fn needs_approval(&self) -> bool {
        self.asks
    }

    /// The class the test gave it.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        self.class
    }

    /// Logs the start, sleeps, logs the end.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let id = args["id"].as_str().unwrap_or_default().to_owned();
        self.log.lock().unwrap().push(format!("start {id}"));
        tokio::time::sleep(Duration::from_millis(40)).await;
        self.log.lock().unwrap().push(format!("end {id}"));
        Ok(ToolOutput::new(id))
    }
}

/// A registry with `look`, shared, and `change`, exclusive, both logging to `log`; `change` asks first when `asks`.
fn registry(log: &Arc<Mutex<Vec<String>>>, asks: bool) -> Registry {
    let mut registry = Registry::new();
    for (name, class, asks) in [
        ("look", Concurrency::Shared, false),
        ("change", Concurrency::Exclusive, asks),
    ] {
        registry.register(Timed {
            name,
            class,
            asks,
            log: log.clone(),
        });
    }
    registry
}

/// A reply calling the tools `calls` names, each as `(id, tool)`, with the id as its argument.
fn batch(calls: &[(&str, &str)]) -> Reply {
    Reply {
        tool_calls: calls
            .iter()
            .map(|(id, tool)| ToolCall {
                id: (*id).into(),
                name: (*tool).into(),
                arguments: json!({ "id": id }),
            })
            .collect(),
        ..Reply::default()
    }
}

/// Runs one turn in which the model makes the calls `calls` at once, then answers; returns the history.
async fn drive(
    registry: &Registry,
    approvals: &Approvals,
    calls: Vec<(String, String)>,
) -> Vec<Message> {
    let model = Router::new(move |seen| {
        if seen.messages == 1 {
            let named: Vec<(&str, &str)> = calls
                .iter()
                .map(|(id, tool)| (id.as_str(), tool.as_str()))
                .collect();
            batch(&named)
        } else {
            say("done")
        }
    });
    let mut history = Vec::new();
    run_turn(
        &Context {
            model: model.as_ref(),
            registry,
            approvals,
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

/// Owned `(id, tool)` pairs.
fn owned(calls: &[(&str, &str)]) -> Vec<(String, String)> {
    calls
        .iter()
        .map(|(id, tool)| ((*id).to_owned(), (*tool).to_owned()))
        .collect()
}

/// Shared calls next to each other overlap, and each exclusive call waits for everything before it and holds back everything after it; the results come back in the model's order.
#[tokio::test]
async fn shared_calls_overlap_and_exclusive_calls_keep_their_place() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let calls = owned(&[
        ("r1", "look"),
        ("r2", "look"),
        ("w1", "change"),
        ("r3", "look"),
        ("r4", "look"),
        ("w2", "change"),
    ]);
    let history = drive(&registry(&log, false), &deny_all(), calls).await;
    let log = log.lock().unwrap().clone();
    let mut first: Vec<&str> = log[..2].iter().map(String::as_str).collect();
    first.sort_unstable();
    assert_eq!(first, ["start r1", "start r2"], "{log:?}");
    let mut ends: Vec<&str> = log[2..4].iter().map(String::as_str).collect();
    ends.sort_unstable();
    assert_eq!(ends, ["end r1", "end r2"], "{log:?}");
    assert_eq!(log[4..6], ["start w1", "end w1"], "{log:?}");
    let mut second: Vec<&str> = log[6..8].iter().map(String::as_str).collect();
    second.sort_unstable();
    assert_eq!(second, ["start r3", "start r4"], "{log:?}");
    assert_eq!(log[10..], ["start w2", "end w2"], "{log:?}");
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results");
    };
    let order: Vec<&str> = results.iter().map(|result| result.text.as_str()).collect();
    assert_eq!(order, ["r1", "r2", "w1", "r3", "r4", "w2"]);
}

/// A denied exclusive call never runs and does not hold back the calls around it, which still run in order; every question is answered before anything runs.
#[tokio::test]
async fn denied_exclusive_calls_are_skipped_in_place() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let calls = owned(&[("w1", "change"), ("r1", "look"), ("w2", "change")]);
    let history = drive(&registry(&log, true), &deny_all(), calls).await;
    assert_eq!(*log.lock().unwrap(), ["start r1", "end r1"]);
    let Message::ToolResults(results) = &history[2] else {
        panic!("no results");
    };
    assert!(results[0].is_error && !results[1].is_error && results[2].is_error);
}

/// Approved exclusive calls run one at a time, in order, around the shared ones.
#[tokio::test]
async fn approved_exclusive_calls_run_in_order() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let approvals = Approvals::new(Arc::new(crate::testkit::DenyAll), true);
    let calls = owned(&[
        ("r1", "look"),
        ("w1", "change"),
        ("w2", "change"),
        ("r2", "look"),
    ]);
    drive(&registry(&log, true), &approvals, calls).await;
    assert_eq!(
        *log.lock().unwrap(),
        [
            "start r1", "end r1", "start w1", "end w1", "start w2", "end w2", "start r2", "end r2"
        ]
    );
}

/// A hidden tool is left off every request's tool list, which stays the same and carries `load_tools`; loading it hands over its schema, and calling it asks for approval like any other call.
#[tokio::test]
async fn hidden_tools_are_off_the_list_but_called_like_the_rest() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut registry = registry(&log, true);
    registry.defer_named("change");
    let registry = registry.fitted();
    let model = Router::new(|seen| match seen.messages {
        1 => crate::testkit::call("l", omc_tools::LOAD_TOOLS, json!({"names": ["change"]})),
        3 => batch(&[("w", "change"), ("r", "look")]),
        _ => say("done"),
    });
    let mut history = Vec::new();
    run_turn(
        &Context {
            model: model.as_ref(),
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
        &mut history,
        "go".into(),
        &|_: AgentEvent| {},
    )
    .await
    .unwrap();
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen.len(), 3);
    for request in seen.iter() {
        assert_eq!(request.tools, ["load_tools", "look"]);
    }
    assert_eq!(*log.lock().unwrap(), ["start r", "end r"]);
    let Message::ToolResults(loaded) = &history[2] else {
        panic!("no results");
    };
    assert!(
        loaded[0].text.contains("<tool name=\"change\">"),
        "{}",
        loaded[0].text
    );
    let Message::ToolResults(results) = &history[4] else {
        panic!("no results");
    };
    assert!(
        results[0].is_error && results[0].text.contains("denied"),
        "{}",
        results[0].text
    );
}
