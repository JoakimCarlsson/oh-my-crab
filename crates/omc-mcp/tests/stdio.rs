//! A tool server that is a program: its tools and resources, and starting it again after it stopped. Skipped when Python is not installed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omc_mcp::config::{Origin, Server, Transport};
use omc_mcp::{Hub, RESOURCES_TOOL};
use omc_tools::{Concurrency, Spill, Tool};
use omc_wire::Reach;
use serde_json::json;

/// The Python found on this machine, when there is one.
fn python() -> Option<&'static str> {
    ["python3", "python"].into_iter().find(|program| {
        std::process::Command::new(program)
            .arg("--version")
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/// The memo server, run with `python`.
fn memo(python: &str) -> Server {
    let script: PathBuf =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/memo_server.py");
    Server {
        name: "memo".into(),
        reach: Reach::Command {
            program: python.into(),
            args: vec!["-I".into(), script.display().to_string()],
            env: vec![],
        },
        transport: Transport::Streamable,
        folder: None,
        oauth: None,
        origin: Origin::Project,
        file: None,
        summary: "memo".into(),
    }
}

/// The tool called `name` among `tools`.
fn named<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a Arc<dyn Tool> {
    tools
        .iter()
        .find(|tool| tool.name() == name)
        .unwrap_or_else(|| panic!("no tool {name}"))
}

/// The server's tools come with the resources tool, which lists, reads and reports what it cannot read; a server that stops fails the call it stopped in and is started again for the next.
#[tokio::test]
async fn tools_resources_and_restarts() {
    let Some(python) = python() else {
        eprintln!("skipped: Python is not installed");
        return;
    };
    let hub = Hub::new(Spill::disabled(), None, Arc::new(|_| {}));
    let reached = hub.connect(&[memo(python)]).await;
    assert!(reached.failed.is_empty(), "{:?}", reached.failed);
    let mut names: Vec<&str> = reached.tools.iter().map(|tool| tool.name()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["mcp__memo__add", "mcp__memo__crash", RESOURCES_TOOL]
    );
    assert!(hub.tried("memo"));
    assert!(hub.connect(&[memo(python)]).await.tools.is_empty());

    let resources = named(&reached.tools, RESOURCES_TOOL);
    assert!(resources.read_only());
    let listed = resources.call(json!({})).await.unwrap().text;
    assert!(
        listed.contains("memo: memo://greeting (greeting) [text/plain] - A greeting."),
        "{listed}"
    );
    assert!(
        listed.contains("memo: template memo://{name} (memo)"),
        "{listed}"
    );
    let read = resources
        .call(json!({"server": "memo", "uri": "memo://greeting"}))
        .await
        .unwrap();
    assert_eq!(read.text, "Hello from the memo server.");
    let missing = resources
        .call(json!({"uri": "memo://nothing"}))
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("no memo nothing"), "{missing}");
    let elsewhere = resources
        .call(json!({"server": "other"}))
        .await
        .unwrap_err();
    assert!(
        elsewhere.to_string().contains("these do: memo"),
        "{elsewhere}"
    );

    let add = named(&reached.tools, "mcp__memo__add");
    assert_eq!(add.concurrency(&json!({})), Concurrency::Shared);
    assert!(add.needs_approval() && !add.read_only());
    let crash = named(&reached.tools, "mcp__memo__crash");
    assert_eq!(crash.concurrency(&json!({})), Concurrency::Exclusive);
    assert_eq!(add.call(json!({"a": 2, "b": 3})).await.unwrap().text, "5");
    let first = resources
        .call(json!({"uri": "memo://pid"}))
        .await
        .unwrap()
        .text;
    let stopped = named(&reached.tools, "mcp__memo__crash")
        .call(json!({}))
        .await
        .unwrap_err();
    assert!(stopped.to_string().contains("started again"), "{stopped}");
    assert_eq!(add.call(json!({"a": 1, "b": 1})).await.unwrap().text, "2");
    let second = resources
        .call(json!({"uri": "memo://pid"}))
        .await
        .unwrap()
        .text;
    assert_ne!(first, second);
}
