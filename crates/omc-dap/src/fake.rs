//! A scripted debug adapter for tests: it speaks the protocol over an in-memory stream and pretends to debug a small C program, so sessions can be tested without a real debugger.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omc_tools::Sandbox;
use serde_json::{Value, json};
use tokio::io::{AsyncWrite, BufReader, DuplexStream, ReadHalf, WriteHalf};

use crate::client::Client;
use crate::session::{Session, Spot, Start};
use crate::wire::{read_message, write_message};

/// The source of the pretend program.
const SOURCE: &str = "#include <stdio.h>\nint sum(int *xs, int n) {\n    int total = 0;\n    for (int i = 0; i <= n; i++) {\n        total += xs[i];\n    }\n    return total;\n}\nint main(void) {\n    int xs[4] = {1, 2, 3, 4};\n    printf(\"hello\\n\");\n    int s = sum(xs, 4);\n    return s;\n}\n";

/// How the pretend adapter behaves.
#[derive(Clone, Debug, Default)]
pub(crate) struct Script {
    /// How long after a `continue` the program exits; it never does when not set.
    pub(crate) exit_after: Option<Duration>,
    /// Whether the adapter never answers `threads`, to test waiting.
    pub(crate) silent_threads: bool,
    /// Whether the adapter asks the client to run something in a terminal when launching.
    pub(crate) asks_terminal: bool,
}

/// What the pretend adapter heard: every request and every answer to its own requests, in order.
pub(crate) type Heard = Arc<Mutex<Vec<Value>>>;

/// A scratch folder holding the pretend program's source, under the workspace's git-ignored `build/`.
pub(crate) fn project(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-dap")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("bug.c"), SOURCE).unwrap();
    dir.canonicalize().unwrap()
}

/// A client connected to a pretend adapter following `script` for the program whose source is in `dir`, and what the adapter hears.
pub(crate) fn connect(script: Script, dir: &Path) -> (Client, Heard) {
    let (near, far) = tokio::io::duplex(1 << 16);
    let (near_read, near_write) = tokio::io::split(near);
    let heard: Heard = Arc::default();
    tokio::spawn(serve(far, script, dir.join("bug.c"), heard.clone()));
    (Client::over(near_read, near_write, None), heard)
}

/// Starts a session `id` over a pretend adapter following `script`, launching the pretend program in `dir` with `breakpoints`.
pub(crate) async fn session(
    id: &str,
    script: Script,
    dir: &Path,
    breakpoints: Vec<Spot>,
) -> (Result<(Session, String), String>, Heard) {
    let (client, heard) = connect(script, dir);
    let start = Start {
        program: Some(dir.join("bug")),
        cwd: dir.to_path_buf(),
        breakpoints,
        timeout: Duration::from_secs(2),
        ..Start::default()
    };
    let sandbox = Sandbox::new(dir).unwrap();
    (
        Session::over(id.to_owned(), sandbox, None, client, start).await,
        heard,
    )
}

/// The pretend adapter's side of the stream, and the number of its next message.
struct Far {
    /// Where its messages go.
    writer: WriteHalf<DuplexStream>,
    /// The number of its next message.
    seq: i64,
}

impl Far {
    /// Sends `message` with the next number.
    async fn send(&mut self, mut message: Value) {
        self.seq += 1;
        message["seq"] = json!(self.seq);
        let _ = write_message(&mut self.writer, &message).await;
    }

    /// Answers `request` with success and `body`.
    async fn answer(&mut self, request: &Value, body: Value) {
        self.send(json!({
            "type": "response",
            "request_seq": request["seq"],
            "command": request["command"],
            "success": true,
            "body": body,
        }))
        .await;
    }

    /// Sends the event `event` with `body`.
    async fn event(&mut self, event: &str, body: Value) {
        self.send(json!({"type": "event", "event": event, "body": body}))
            .await;
    }
}

/// Runs the pretend adapter on `stream` following `script`, the program's source at `source`, recording what it hears in `heard`.
async fn serve(stream: DuplexStream, script: Script, source: PathBuf, heard: Heard) {
    let (reader, writer): (ReadHalf<DuplexStream>, WriteHalf<DuplexStream>) =
        tokio::io::split(stream);
    let mut reader = BufReader::new(reader);
    let far = Arc::new(tokio::sync::Mutex::new(Far { writer, seq: 0 }));
    let mut launch: Option<Value> = None;
    let mut breakpoints = 0;
    while let Some(message) = read_message(&mut reader).await {
        heard.lock().unwrap().push(message.clone());
        if message["type"] == "response" {
            continue;
        }
        let command = message["command"].as_str().unwrap_or_default().to_owned();
        let mut out = far.lock().await;
        match command.as_str() {
            "initialize" => {
                out.event(
                    "output",
                    json!({"category": "stdout", "output": "Fake debugger 1.0\n"}),
                )
                .await;
                out.answer(
                    &message,
                    json!({
                        "supportsConfigurationDoneRequest": true,
                        "supportsConditionalBreakpoints": true,
                        "supportsFunctionBreakpoints": true,
                        "supportsLogPoints": true,
                        "exceptionBreakpointFilters": [
                            {"filter": "uncaught", "label": "Uncaught", "default": true},
                            {"filter": "raised", "label": "Raised"},
                        ],
                    }),
                )
                .await;
            }
            "launch" | "attach" => {
                out.event(
                    "output",
                    json!({"category": "stdout", "output": "No source file named x.c.\n"}),
                )
                .await;
                if script.asks_terminal {
                    out.send(json!({
                        "type": "request",
                        "command": "runInTerminal",
                        "arguments": {"args": ["x"]},
                    }))
                    .await;
                }
                out.event("initialized", json!({})).await;
                launch = Some(message.clone());
            }
            "setBreakpoints" => {
                let list = message["arguments"]["breakpoints"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                let placed: Vec<Value> = list
                    .iter()
                    .enumerate()
                    .map(|(index, breakpoint)| {
                        let line = breakpoint["line"].as_i64().unwrap_or(0);
                        json!({"id": index + 1, "verified": line < 100, "line": line, "message": if line < 100 { Value::Null } else { json!("no code at this line") }})
                    })
                    .collect();
                breakpoints = placed.len();
                out.answer(&message, json!({"breakpoints": placed})).await;
            }
            "setFunctionBreakpoints" => {
                let count = message["arguments"]["breakpoints"]
                    .as_array()
                    .map_or(0, Vec::len);
                let placed: Vec<Value> = (0..count)
                    .map(|index| json!({"id": 50 + index, "verified": true}))
                    .collect();
                out.answer(&message, json!({"breakpoints": placed})).await;
            }
            "setExceptionBreakpoints" => out.answer(&message, json!({})).await,
            "configurationDone" => {
                out.answer(&message, json!({})).await;
                if let Some(launch) = launch.take() {
                    out.answer(&launch, json!({})).await;
                }
                out.event(
                    "process",
                    json!({"systemProcessId": 4_000_000, "name": "bug"}),
                )
                .await;
                out.event(
                    "output",
                    json!({"category": "stdout", "output": "hello\r\n"}),
                )
                .await;
                if breakpoints > 0 {
                    out.event(
                        "stopped",
                        json!({"reason": "breakpoint", "threadId": 1, "hitBreakpointIds": [1], "description": "breakpoint 1.1"}),
                    )
                    .await;
                } else {
                    out.event("exited", json!({"exitCode": 0})).await;
                    out.event("terminated", json!({})).await;
                }
            }
            "threads" => {
                if !script.silent_threads {
                    out.answer(&message, json!({"threads": [{"id": 1, "name": "bug"}]}))
                        .await;
                }
            }
            "stackTrace" => {
                out.answer(
                    &message,
                    json!({"stackFrames": [
                        {"id": 100, "name": "sum", "line": 5, "column": 9, "source": {"name": "bug.c", "path": source}},
                        {"id": 101, "name": "main", "line": 12, "column": 13, "source": {"name": "bug.c", "path": source}},
                        {"id": 102, "name": "__libc_start_main", "line": 0, "column": 0, "instructionPointerReference": "0x7fff1234"},
                    ], "totalFrames": 3}),
                )
                .await;
            }
            "scopes" => {
                out.answer(
                    &message,
                    json!({"scopes": [
                        {"name": "Arguments", "presentationHint": "arguments", "variablesReference": 30, "expensive": false},
                        {"name": "Locals", "presentationHint": "locals", "variablesReference": 10, "expensive": false},
                        {"name": "Registers", "variablesReference": 20, "expensive": false},
                    ]}),
                )
                .await;
            }
            "variables" => {
                let variables = match message["arguments"]["variablesReference"].as_i64() {
                    Some(10) => json!([
                        {"name": "xs", "type": "int *", "value": "0x7ffe0010", "variablesReference": 11},
                        {"name": "n", "type": "int", "value": "4", "variablesReference": 0},
                        {"name": "total", "type": "int", "value": "10", "variablesReference": 0},
                        {"name": "i", "type": "int", "value": "4", "variablesReference": 0},
                    ]),
                    Some(11) => {
                        json!([{"name": "*xs", "type": "int", "value": "1", "variablesReference": 0}])
                    }
                    Some(20) => {
                        json!([{"name": "rip", "value": "0x401136", "variablesReference": 0}])
                    }
                    Some(30) => {
                        json!([{"name": "count", "type": "int", "value": "4", "variablesReference": 0}])
                    }
                    _ => json!([]),
                };
                out.answer(&message, json!({"variables": variables})).await;
            }
            "evaluate" => {
                let expression = message["arguments"]["expression"]
                    .as_str()
                    .unwrap_or_default();
                if expression == "xs[i]" {
                    out.send(json!({
                        "type": "response",
                        "request_seq": message["seq"],
                        "command": "evaluate",
                        "success": false,
                        "message": "error",
                        "body": {"error": {"format": "read of {where} failed", "variables": {"where": "xs[4]"}}},
                    }))
                    .await;
                } else {
                    out.answer(
                        &message,
                        json!({"result": "10", "type": "int", "variablesReference": 0}),
                    )
                    .await;
                }
            }
            "continue" | "next" | "stepIn" | "stepOut" => {
                out.answer(&message, json!({"allThreadsContinued": true}))
                    .await;
                out.event(
                    "continued",
                    json!({"threadId": 1, "allThreadsContinued": true}),
                )
                .await;
                if command != "continue" {
                    out.event("stopped", json!({"reason": "step", "threadId": 1}))
                        .await;
                } else if let Some(after) = script.exit_after {
                    let later = far.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(after).await;
                        let mut out = later.lock().await;
                        out.event("output", json!({"category": "stderr", "output": "done\n"}))
                            .await;
                        out.event("exited", json!({"exitCode": 3})).await;
                        out.event("terminated", json!({})).await;
                    });
                }
            }
            "pause" => {
                out.answer(&message, json!({})).await;
                out.event("stopped", json!({"reason": "pause", "threadId": 1}))
                    .await;
            }
            "disconnect" => {
                out.answer(&message, json!({})).await;
                shut(&mut out.writer).await;
                return;
            }
            _ => {
                out.send(json!({
                    "type": "response",
                    "request_seq": message["seq"],
                    "command": command,
                    "success": false,
                    "message": "unsupported",
                }))
                .await;
            }
        }
    }
}

/// Closes the pretend adapter's side of the stream.
async fn shut(writer: &mut (impl AsyncWrite + Unpin)) {
    let _ = tokio::io::AsyncWriteExt::shutdown(writer).await;
}

#[cfg(test)]
/// Sessions against the pretend adapter.
mod tests {
    use super::*;
    use crate::session::Motion;
    use crate::sessions::Sessions;

    /// The commands the adapter was sent, in order.
    fn commands(heard: &Heard) -> Vec<String> {
        heard
            .lock()
            .unwrap()
            .iter()
            .filter(|message| message["type"] == "request")
            .filter_map(|message| message["command"].as_str().map(str::to_owned))
            .collect()
    }

    /// Launching sets the breakpoints once the adapter is ready, before configuration is done, and reports the stop: reason, source line, stack, locals and output, without the debugger's own banner.
    #[tokio::test]
    async fn launch_reports_the_first_stop() {
        let dir = project("fake-launch");
        let spots = vec![Spot::line(dir.join("bug.c"), 5), Spot::function("sum")];
        let (started, heard) = session("dbg-1", Script::default(), &dir, spots).await;
        let (_session, report) = started.unwrap();
        assert_eq!(
            commands(&heard),
            [
                "initialize",
                "launch",
                "setBreakpoints",
                "setFunctionBreakpoints",
                "setExceptionBreakpoints",
                "configurationDone",
                "threads",
                "stackTrace",
                "scopes",
                "variables",
                "variables",
            ]
        );
        let exceptions = heard
            .lock()
            .unwrap()
            .iter()
            .find(|message| message["command"] == "setExceptionBreakpoints")
            .cloned()
            .unwrap();
        assert_eq!(exceptions["arguments"]["filters"], json!(["uncaught"]));
        for expected in [
            "Session dbg-1 started",
            "#1 bug.c:5",
            "#50 function sum",
            "Stopped: breakpoint 1 in thread 1 (bug)",
            "At sum at bug.c:5",
            "  > 5 |         total += xs[i];",
            "  #1 main at bug.c:12",
            "  #2 __libc_start_main at 0x7fff1234 (no source)",
            "Arguments:\n  count: int = 4\nLocals:\n  xs: int * = 0x7ffe0010 [ref 11]",
            "  total: int = 10",
            "Program output:\n  hello",
        ] {
            assert!(
                report.contains(expected),
                "{expected:?} missing from:\n{report}"
            );
        }
        assert!(!report.contains("Fake debugger"), "{report}");
        assert!(!report.contains("No source file named x.c."), "{report}");
        let launch = heard
            .lock()
            .unwrap()
            .iter()
            .find(|message| message["command"] == "launch")
            .cloned()
            .unwrap();
        assert_eq!(launch["arguments"]["program"], json!(dir.join("bug")));
    }

    /// Variables are expanded to the depth asked for with references to go deeper, expensive scopes are left out unless named, and evaluation shows the result or the adapter's reason.
    #[tokio::test]
    async fn inspects_variables_and_expressions() {
        let dir = project("fake-inspect");
        let spots = vec![Spot::line(dir.join("bug.c"), 5)];
        let (started, _) = session("dbg-1", Script::default(), &dir, spots).await;
        let (mut session, _) = started.unwrap();
        let shown = session.variables(None, 0, None, None, 1, 50).await.unwrap();
        assert!(shown.contains("Frame #0 sum at bug.c:5"), "{shown}");
        assert!(
            shown.contains("  xs: int * = 0x7ffe0010 [ref 11]\n    *xs: int = 1"),
            "{shown}"
        );
        assert!(shown.contains("Registers [ref 20]: not listed"), "{shown}");
        let registers = session
            .variables(None, 0, Some("registers"), None, 0, 50)
            .await
            .unwrap();
        assert!(registers.contains("rip = 0x401136"), "{registers}");
        let narrow = session
            .variables(None, 0, None, Some(10), 0, 2)
            .await
            .unwrap();
        assert!(narrow.contains("… 2 more"), "{narrow}");
        let missing = session
            .variables(None, 0, Some("Globals"), None, 0, 5)
            .await;
        assert!(
            missing
                .unwrap_err()
                .contains("it has Arguments, Locals, Registers")
        );
        let value = session.evaluate("total", None, 1, 1, 10).await.unwrap();
        assert!(value.contains("in frame #1 main at bug.c:12"), "{value}");
        assert!(value.contains("total: int = 10"), "{value}");
        let failed = session.evaluate("xs[i]", None, 0, 1, 10).await;
        assert_eq!(failed.unwrap_err(), "read of xs[4] failed");
        let frames = session.stack(None, Some(2)).await.unwrap();
        assert!(frames.contains("  … 1 more frames"), "{frames}");
        assert!(
            session
                .scopes(None, 7)
                .await
                .unwrap_err()
                .contains("frame 7 does not exist")
        );
    }

    /// A step reports the next stop; a run that does not stop in time is reported as still running, and its end is reported on the next call.
    #[tokio::test]
    async fn runs_time_out_and_report_later() {
        let dir = project("fake-run");
        let script = Script {
            exit_after: Some(Duration::from_millis(300)),
            ..Script::default()
        };
        let spots = vec![Spot::line(dir.join("bug.c"), 5)];
        let (started, _) = session("dbg-1", script, &dir, spots).await;
        let (mut session, _) = started.unwrap();
        let step = session
            .resume(Motion::Over, None, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(step.starts_with("Stopped: step"), "{step}");
        let running = session
            .resume(Motion::Continue, None, Duration::from_millis(50))
            .await
            .unwrap();
        assert!(running.starts_with("Still running after"), "{running}");
        assert!(
            session
                .stack(None, None)
                .await
                .unwrap_err()
                .contains("running, not stopped")
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
        let notice = session.notices().await.unwrap();
        assert!(
            notice.contains("The program exited with code 3."),
            "{notice}"
        );
        assert!(notice.contains("Program output:\n  done"), "{notice}");
        assert!(session.notices().await.is_none());
        assert!(
            session.output(true).contains("hello\ndone") || session.output(true).contains("done")
        );
        assert!(
            session
                .resume(Motion::Continue, None, Duration::from_millis(10))
                .await
                .unwrap_err()
                .contains("has ended")
        );
    }

    /// A program with no breakpoints runs to its end, which the launch reports.
    #[tokio::test]
    async fn a_program_without_breakpoints_runs_to_its_end() {
        let dir = project("fake-end");
        let (started, _) = session("dbg-1", Script::default(), &dir, Vec::new()).await;
        let (session, report) = started.unwrap();
        assert!(
            report.contains("The program exited with code 0."),
            "{report}"
        );
        assert!(report.contains("Program output:\n  hello"), "{report}");
        assert!(session.describe().ends_with("exited with code 0"));
    }

    /// Breakpoints are added beside the others, one the adapter cannot place is shown pending with its reason, and clearing sends what is left.
    #[tokio::test]
    async fn breakpoints_are_kept_per_file() {
        let dir = project("fake-breakpoints");
        let spots = vec![Spot::line(dir.join("bug.c"), 5)];
        let (started, heard) = session("dbg-1", Script::default(), &dir, spots).await;
        let (mut session, _) = started.unwrap();
        let far = session
            .add(Spot::line(dir.join("bug.c"), 120))
            .await
            .unwrap();
        assert_eq!(far, "#2 bug.c:120 — pending: no code at this line");
        let listing = session.breakpoint_listing().unwrap();
        assert!(listing.contains("#1 bug.c:5\n  #2 bug.c:120"), "{listing}");
        let left = session
            .clear(Some(dir.join("bug.c")), Some(5), None, false)
            .await
            .unwrap();
        assert!(!left.contains("bug.c:5"), "{left}");
        let last = heard
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|message| message["command"] == "setBreakpoints")
            .cloned()
            .unwrap();
        assert_eq!(last["arguments"]["breakpoints"], json!([{"line": 120}]));
        assert!(
            session
                .clear(None, None, Some("nope"), false)
                .await
                .is_err()
        );
        let cleared = session.clear(None, None, None, true).await.unwrap();
        assert_eq!(cleared, "Breakpoints:\n  exceptions: uncaught");
        let filters = session.set_exceptions(vec!["bogus".into()]).await;
        assert!(
            filters
                .unwrap_err()
                .contains("uncaught (Uncaught), raised (Raised)")
        );
    }

    /// A request the adapter never answers fails after its wait instead of hanging, and the adapter's own requests are turned down.
    #[tokio::test(start_paused = true)]
    async fn unanswered_requests_time_out_and_reverse_requests_are_refused() {
        let dir = project("fake-timeout");
        let script = Script {
            silent_threads: true,
            asks_terminal: true,
            ..Script::default()
        };
        let (client, heard) = connect(script, &dir);
        let answer = client
            .request("threads", json!({}), Duration::from_secs(10))
            .await;
        assert_eq!(
            answer.unwrap_err(),
            "the debug adapter did not answer threads within 10s"
        );
        client
            .request("launch", json!({}), Duration::from_millis(1))
            .await
            .ok();
        tokio::time::sleep(Duration::from_millis(100)).await;
        let refusal = heard
            .lock()
            .unwrap()
            .iter()
            .find(|message| message["type"] == "response")
            .cloned()
            .unwrap();
        assert_eq!(refusal["command"], "runInTerminal");
        assert_eq!(refusal["success"], false);
    }

    /// Closing the conversation's sessions tells each adapter to disconnect and end what it launched, and leaves none open.
    #[tokio::test]
    async fn closing_disconnects_every_session() {
        let dir = project("fake-close");
        let sessions = Sessions::new();
        let mut heards = Vec::new();
        for _ in 0..2 {
            let id = sessions.reserve().unwrap();
            let spots = vec![Spot::line(dir.join("bug.c"), 5)];
            let (started, heard) = session(&id, Script::default(), &dir, spots).await;
            sessions.admit(Some(started.unwrap().0));
            heards.push(heard);
        }
        assert_eq!(sessions.len(), 2);
        assert!(sessions.get(Some("dbg-9")).err().is_some());
        assert_eq!(sessions.get(None).unwrap().lock().await.id, "dbg-2");
        sessions.close().await;
        assert!(sessions.is_empty());
        for heard in heards {
            let disconnect = heard
                .lock()
                .unwrap()
                .iter()
                .find(|message| message["command"] == "disconnect")
                .cloned()
                .unwrap();
            assert_eq!(disconnect["arguments"]["terminateDebuggee"], true);
        }
    }

    /// No more than the allowed number of sessions start, and a failed start frees its place.
    #[test]
    fn sessions_are_limited() {
        let sessions = Sessions::new();
        for _ in 0..crate::sessions::MOST_SESSIONS {
            sessions.reserve().unwrap();
        }
        assert!(
            sessions
                .reserve()
                .unwrap_err()
                .contains("disconnect one first")
        );
        sessions.admit(None);
        assert!(sessions.reserve().is_ok());
        assert!(
            sessions
                .get(None)
                .err()
                .unwrap()
                .contains("no debugging session is open")
        );
    }

    /// An adapter that stops while a request waits fails the request at once.
    #[tokio::test]
    async fn an_adapter_that_stops_fails_what_waits() {
        let (near, far) = tokio::io::duplex(1024);
        let (reader, writer) = tokio::io::split(near);
        let client = Client::over(reader, writer, None);
        let waiting = client.request("threads", json!({}), Duration::from_secs(30));
        let stop = async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(far);
        };
        let (answer, ()) = tokio::join!(waiting, stop);
        assert!(
            answer
                .unwrap_err()
                .contains("stopped before answering threads")
        );
        assert!(client.state().gone);
    }
}
