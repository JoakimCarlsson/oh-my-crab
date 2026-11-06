//! What a debug adapter has said about the program it debugs, kept up to date from its events.

use std::collections::HashMap;

use serde_json::Value;

/// The most program output kept, in bytes; older output is dropped first.
const OUTPUT_LIMIT: usize = 256 * 1024;

/// The most debugger messages kept, in bytes.
const CONSOLE_LIMIT: usize = 16 * 1024;

/// Why and where the program last stopped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Stop {
    /// The reason the adapter gave, such as `breakpoint`, `step`, `exception` or `pause`.
    pub(crate) reason: String,
    /// The adapter's longer account of the reason, such as the signal of a crash.
    pub(crate) description: Option<String>,
    /// Further text the adapter gave, such as an exception's message.
    pub(crate) text: Option<String>,
    /// The thread that stopped, when the adapter said.
    pub(crate) thread: Option<i64>,
    /// The breakpoints that were hit, by id.
    pub(crate) breakpoints: Vec<i64>,
}

/// A stream of text the program or the debugger wrote, of bounded size, with a mark of how much of it was read.
#[derive(Debug, Default)]
pub(crate) struct Output {
    /// The text kept, the newest last.
    text: String,
    /// How many bytes were written before the first one kept.
    start: u64,
    /// How many bytes were written up to the end of what was last read.
    read: u64,
    /// The most bytes kept.
    limit: usize,
}

/// What [`Output::unread`] returns: the text written since the last read, and how many bytes written since were dropped before they could be read.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Unread {
    /// The text.
    pub(crate) text: String,
    /// The bytes dropped unread.
    pub(crate) dropped: u64,
}

impl Output {
    /// An empty stream keeping at most `limit` bytes.
    fn with_limit(limit: usize) -> Self {
        Self {
            limit,
            ..Self::default()
        }
    }

    /// Adds `chunk` to the end, dropping the oldest text past the limit.
    pub(crate) fn push(&mut self, chunk: &str) {
        self.text.push_str(chunk);
        if self.text.len() > self.limit {
            let mut cut = self.text.len() - self.limit;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.start += cut as u64;
        }
    }

    /// The text written since the last read, which is now read.
    pub(crate) fn unread(&mut self) -> Unread {
        let end = self.start + self.text.len() as u64;
        let from = self.read.max(self.start);
        let dropped = from - self.read;
        let skip = usize::try_from(from - self.start).unwrap_or(usize::MAX);
        let text = self.text.get(skip..).unwrap_or_default().to_owned();
        self.read = end;
        Unread { text, dropped }
    }

    /// Every byte kept, read or not.
    pub(crate) fn all(&self) -> &str {
        &self.text
    }
}

/// What the adapter has said so far.
#[derive(Debug)]
pub(crate) struct State {
    /// Whether output can be the program's yet, which it can once the program is being launched or attached to, so that what a debugger prints about itself on starting is not taken for the program's.
    pub(crate) listening: bool,
    /// Whether the adapter has said anything about the program's process or threads, before which what it prints is its own, such as its greeting or its notes on breakpoints it cannot place yet, which are taken as read once the program runs.
    pub(crate) running: bool,
    /// Whether the adapter said it is ready to be told the breakpoints.
    pub(crate) initialized: bool,
    /// How many times the program stopped.
    pub(crate) stops: u64,
    /// Where the program is stopped now; nothing while it runs.
    pub(crate) stop: Option<Stop>,
    /// The code the program exited with, once it has.
    pub(crate) exit_code: Option<i64>,
    /// Whether the debugging session ended.
    pub(crate) terminated: bool,
    /// Whether the adapter stopped talking, having exited or closed the connection.
    pub(crate) gone: bool,
    /// The program's process id, when the adapter said.
    pub(crate) pid: Option<u32>,
    /// What the program printed.
    pub(crate) output: Output,
    /// What the debugger printed about itself.
    pub(crate) console: Output,
    /// What the adapter last said about each breakpoint, by id.
    pub(crate) breakpoints: HashMap<i64, Value>,
}

impl Default for State {
    /// The state before the adapter has said anything.
    fn default() -> Self {
        Self {
            listening: false,
            running: false,
            initialized: false,
            stops: 0,
            stop: None,
            exit_code: None,
            terminated: false,
            gone: false,
            pid: None,
            output: Output::with_limit(OUTPUT_LIMIT),
            console: Output::with_limit(CONSOLE_LIMIT),
            breakpoints: HashMap::new(),
        }
    }
}

impl State {
    /// Takes in the event called `event` with `body`.
    pub(crate) fn apply(&mut self, event: &str, body: &Value) {
        if !self.running
            && matches!(
                event,
                "process" | "thread" | "stopped" | "continued" | "exited"
            )
        {
            self.running = true;
            let _ = self.console.unread();
        }
        match event {
            "initialized" => self.initialized = true,
            "stopped" => {
                self.stops += 1;
                self.stop = Some(Stop {
                    reason: text(body, "reason").unwrap_or_else(|| "unknown".to_owned()),
                    description: text(body, "description"),
                    text: text(body, "text"),
                    thread: body["threadId"].as_i64(),
                    breakpoints: body["hitBreakpointIds"]
                        .as_array()
                        .map(|ids| ids.iter().filter_map(Value::as_i64).collect())
                        .unwrap_or_default(),
                });
            }
            "continued" => {
                let all = body["allThreadsContinued"].as_bool().unwrap_or(true);
                let same = self
                    .stop
                    .as_ref()
                    .is_some_and(|stop| stop.thread == body["threadId"].as_i64());
                if all || same {
                    self.stop = None;
                }
            }
            "exited" => {
                self.exit_code = body["exitCode"].as_i64();
                self.stop = None;
            }
            "terminated" => {
                self.terminated = true;
                self.stop = None;
            }
            "process" => {
                self.pid = body["systemProcessId"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok());
            }
            "output" => self.take_output(body),
            "breakpoint" => {
                let breakpoint = &body["breakpoint"];
                if let Some(id) = breakpoint["id"].as_i64() {
                    if body["reason"] == "removed" {
                        self.breakpoints.remove(&id);
                    } else {
                        self.breakpoints.insert(id, breakpoint.clone());
                    }
                }
            }
            _ => {}
        }
    }

    /// Keeps the text of an `output` event: the program's when it is its standard output or error and the program is being debugged, the debugger's otherwise; telemetry is dropped.
    fn take_output(&mut self, body: &Value) {
        let Some(text) = body["output"].as_str() else {
            return;
        };
        let text = text.replace("\r\n", "\n");
        match body["category"].as_str().unwrap_or("console") {
            "stdout" if is_debugger_note(&text) => self.console.push(&text),
            "stdout" | "stderr" if self.listening && self.running => self.output.push(&text),
            "telemetry" => {}
            _ => self.console.push(&text),
        }
    }

    /// Whether the program can no longer be debugged: the session ended or the adapter is gone.
    pub(crate) fn over(&self) -> bool {
        self.terminated || self.gone
    }
}

/// Whether `text`, written on standard output, is a debugger's note about a breakpoint rather than the program's output, as gdb writes when a breakpoint is set or cannot be placed yet: `Breakpoint 2 at 0x1164: file a.c, line 8.`, `Breakpoint 1 (-source a.c -line 5) pending.` or `No source file named a.c.`.
fn is_debugger_note(text: &str) -> bool {
    let line = text.trim();
    let numbered = line.strip_prefix("Breakpoint ").is_some_and(|rest| {
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        digits > 0
            && (rest[digits..].starts_with(" at 0x")
                || (rest[digits..].starts_with(" (") && line.ends_with(") pending.")))
    });
    numbered || (line.starts_with("No source file named ") && !line.contains('\n'))
}

/// The string at `key` in `body`, when there is a non-empty one.
fn text(body: &Value, key: &str) -> Option<String> {
    body[key]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
/// Tests for keeping track of what the adapter said.
mod tests {
    use serde_json::json;

    use super::*;

    /// A stop is recorded with its reason, thread and breakpoints, and cleared when the program goes on.
    #[test]
    fn stops_and_continues() {
        let mut state = State::default();
        state.apply(
            "stopped",
            &json!({"reason": "breakpoint", "threadId": 7, "hitBreakpointIds": [1], "description": ""}),
        );
        let stop = state.stop.clone().unwrap();
        assert_eq!(
            (
                stop.reason.as_str(),
                stop.thread,
                stop.breakpoints,
                stop.description
            ),
            ("breakpoint", Some(7), vec![1], None)
        );
        assert_eq!(state.stops, 1);
        state.apply(
            "continued",
            &json!({"threadId": 8, "allThreadsContinued": false}),
        );
        assert!(state.stop.is_some());
        state.apply("continued", &json!({"threadId": 7}));
        assert!(state.stop.is_none());
    }

    /// Exiting and ending are recorded, and end the stop.
    #[test]
    fn exits() {
        let mut state = State::default();
        state.apply("stopped", &json!({"reason": "pause"}));
        state.apply("exited", &json!({"exitCode": 3}));
        state.apply("terminated", &json!({}));
        assert_eq!(state.exit_code, Some(3));
        assert!(state.over() && state.stop.is_none());
    }

    /// Output before the program is launched is the debugger's, the program's is kept apart, and telemetry is dropped.
    #[test]
    fn output_is_sorted() {
        let mut state = State::default();
        state.apply(
            "output",
            &json!({"category": "stdout", "output": "GNU gdb\n"}),
        );
        state.listening = true;
        state.apply(
            "output",
            &json!({"category": "stdout", "output": "No source file\n"}),
        );
        state.apply("process", &json!({"systemProcessId": 5}));
        state.apply(
            "output",
            &json!({"category": "stdout", "output": "hello\r\n"}),
        );
        state.apply("output", &json!({"category": "stderr", "output": "oops\n"}));
        state.apply(
            "output",
            &json!({"category": "stdout", "output": "Breakpoint 2 at 0x1164: file a.c, line 8.\n"}),
        );
        state.apply("output", &json!({"category": "telemetry", "output": "x"}));
        state.apply("output", &json!({"output": "note\n"}));
        assert_eq!(state.output.all(), "hello\noops\n");
        assert_eq!(
            state.console.all(),
            "GNU gdb\nNo source file\nBreakpoint 2 at 0x1164: file a.c, line 8.\nnote\n"
        );
        assert_eq!(
            state.console.unread().text,
            "Breakpoint 2 at 0x1164: file a.c, line 8.\nnote\n"
        );
        assert!(is_debugger_note(
            "Breakpoint 1 (-source /a.c -line 5) pending.\n"
        ));
        assert!(!is_debugger_note("Breakpoint 1 reached\n"));
        assert_eq!(state.pid, Some(5));
    }

    /// Unread output is handed out once, and output dropped before it was read is counted.
    #[test]
    fn output_is_read_once_and_bounded() {
        let mut output = Output::with_limit(8);
        output.push("abcd");
        assert_eq!(output.unread().text, "abcd");
        assert_eq!(output.unread(), Unread::default());
        output.push("efghijkl");
        let unread = output.unread();
        assert_eq!((unread.text.as_str(), unread.dropped), ("efghijkl", 0));
        output.push("mnopqrstuvwx");
        let unread = output.unread();
        assert_eq!((unread.text.as_str(), unread.dropped), ("qrstuvwx", 4));
        assert_eq!(output.unread(), Unread::default());
    }

    /// Breakpoint events update what is known of each breakpoint.
    #[test]
    fn breakpoint_events() {
        let mut state = State::default();
        state.apply(
            "breakpoint",
            &json!({"reason": "changed", "breakpoint": {"id": 2, "verified": true, "line": 9}}),
        );
        assert_eq!(state.breakpoints[&2]["line"], 9);
        state.apply(
            "breakpoint",
            &json!({"reason": "removed", "breakpoint": {"id": 2}}),
        );
        assert!(state.breakpoints.is_empty());
    }
}
