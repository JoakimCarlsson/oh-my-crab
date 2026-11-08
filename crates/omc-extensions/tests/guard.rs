//! The example extension in `examples/extensions/guard.py`, run through crab's side of the protocol.
//!
//! These tests need `python3`; without it they pass without checking anything.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use omc_agent::{Gate, Hooks};
use omc_extensions::{Extension, ExtensionHooks, Extensions, Origin, Prompted, Setting};
use omc_providers::ToolCall;
use serde_json::{Value, json};

/// Whether `python3` can be run.
fn has_python() -> bool {
    std::process::Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

/// An empty scratch folder named `name` under the workspace's `build/tests`.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-extensions")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The example extension, logging to `log`, with `env` besides, answering events within `timeout`.
fn guard(log: &Path, env: &[(&str, &str)], timeout: Duration) -> Extension {
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/extensions/guard.py")
        .canonicalize()
        .unwrap();
    let mut vars = vec![("GUARD_LOG".to_owned(), log.display().to_string())];
    vars.extend(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())));
    Extension {
        name: "guard".into(),
        program: "python3".into(),
        args: vec![script.display().to_string()],
        env: vars,
        folder: None,
        timeout,
        tool_timeout: Duration::from_secs(20),
        origin: Origin::User,
        file: PathBuf::from("/extensions.json"),
        summary: "python3 guard.py".into(),
    }
}

/// The extensions of a conversation in `folder`, with `extension` started; the tools it gives.
async fn started(
    folder: &Path,
    extension: Extension,
) -> (Arc<Extensions>, Vec<Arc<dyn omc_tools::Tool>>) {
    let host = Extensions::new(Setting {
        session: Some("c-1".into()),
        folder: folder.to_path_buf(),
        mode: Arc::new(|| "default"),
    });
    let tools = host.start(&[extension]).await;
    (host, tools)
}

/// A `bash` call of `command`.
fn bash(command: &str) -> (ToolCall, Value) {
    let arguments = json!({"command": command});
    (
        ToolCall {
            id: "t1".into(),
            name: "bash".into(),
            arguments: arguments.clone(),
        },
        arguments,
    )
}

/// The events the extension logged, in order.
fn logged(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

/// The extension says what it wants and gives its tool; it refuses `rm`, rewrites `date`, lets other calls pass, adds context to prompts and runs its tool.
#[tokio::test]
async fn the_guard_vetoes_rewrites_and_counts() {
    if !has_python() {
        return;
    }
    let dir = scratch("guard");
    let log = dir.join("log.jsonl");
    let (host, tools) = started(&dir, guard(&log, &[], Duration::from_secs(10))).await;
    assert_eq!(host.names(), vec!["guard"]);
    assert!(host.wants("tool_call") && !host.wants("tool_batch"));
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name(), "count_words");
    assert!(tools[0].read_only());
    let hooks = ExtensionHooks::new(host.clone());
    let (call, arguments) = bash("rm -rf build");
    let screened = hooks.before_tool(&call, &arguments).await;
    assert_eq!(
        screened.gate,
        Gate::Deny("the guard extension does not allow rm".into())
    );
    let (call, arguments) = bash("date +%H");
    let screened = hooks.before_tool(&call, &arguments).await;
    assert_eq!(screened.gate, Gate::Pass);
    assert_eq!(
        screened.arguments,
        Some(json!({"command": "TZ=UTC date +%H"}))
    );
    assert_eq!(
        screened.note.as_deref(),
        Some("the guard extension pinned date to UTC")
    );
    let (call, arguments) = bash("echo hi");
    assert_eq!(hooks.before_tool(&call, &arguments).await.arguments, None);
    assert_eq!(
        hooks.turn_start("hello").await,
        Prompted::Changed {
            prompt: None,
            context: Some("The guard extension is watching shell commands.".into())
        }
    );
    let output = tools[0]
        .call_as("t2", json!({"text": "one two three"}))
        .await
        .unwrap();
    assert_eq!(output.text, "3 words");
    let events: Vec<String> = logged(&log)
        .iter()
        .filter_map(|entry| entry["event"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(
        events,
        vec!["tool_call", "tool_call", "tool_call", "turn_start"]
    );
    host.shutdown().await;
}

/// An extension that dies while answering gives no answer, and is started again for the next event; one that stops too often stays off.
#[tokio::test]
async fn a_crashed_extension_is_restarted_then_given_up() {
    if !has_python() {
        return;
    }
    let dir = scratch("guard-crash");
    let log = dir.join("log.jsonl");
    let (host, _) = started(
        &dir,
        guard(
            &log,
            &[("GUARD_CRASH", "turn_start")],
            Duration::from_secs(10),
        ),
    )
    .await;
    let hooks = ExtensionHooks::new(host.clone());
    assert_eq!(hooks.turn_start("hello").await, Prompted::Go);
    let (call, arguments) = bash("rm x");
    assert!(matches!(
        hooks.before_tool(&call, &arguments).await.gate,
        Gate::Deny(reason) if reason.contains("does not allow rm")
    ));
    for _ in 0..4 {
        assert_eq!(hooks.turn_start("hello").await, Prompted::Go);
    }
    assert!(!host.wants("tool_call"));
    assert_eq!(hooks.before_tool(&call, &arguments).await.gate, Gate::Pass);
}

/// An extension that does not answer a tool call in time has the call refused, without the turn waiting past its timeout, and answers the next one.
#[tokio::test]
async fn a_slow_extension_refuses_the_call() {
    if !has_python() {
        return;
    }
    let dir = scratch("guard-slow");
    let log = dir.join("log.jsonl");
    let (host, _) = started(
        &dir,
        guard(&log, &[("GUARD_SLOW", "3")], Duration::from_millis(500)),
    )
    .await;
    let hooks = ExtensionHooks::new(host.clone());
    let (call, arguments) = bash("echo hi");
    let began = std::time::Instant::now();
    let Gate::Deny(reason) = hooks.before_tool(&call, &arguments).await.gate else {
        panic!("a slow extension let the call through");
    };
    assert!(reason.contains("gave no answer"), "{reason}");
    assert!(began.elapsed() < Duration::from_secs(3));
    host.shutdown().await;
}
