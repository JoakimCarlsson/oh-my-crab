//! Baselines: other harnesses given the same tasks, so that crab's numbers have something to stand against.
//!
//! A baseline is a program started once for each run, in the run's folder, that is given the prompt and ends when it is done. What it prints is read a line at a time for the tool calls it made, and the files it leaves are scored like crab's.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command as Process, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::args::PROMPT;
use crate::run::Setup;
use crate::tally::Tally;
use crate::task::Task;

/// The program that is Claude Code.
const CLAUDE_CODE: &str = "claude";

/// The file beside a run's transcript that keeps what the baseline wrote to its error stream.
const ERRORS: &str = "stderr.txt";

/// How many characters of a baseline's error stream are kept to explain why it failed.
const ERROR_TAIL: usize = 400;

/// The setting whose value Claude Code is given as its effort.
const EFFORT: &str = "effort";

/// Crab's names for the tools a model may be given, and what Claude Code calls each.
const CLAUDE_TOOLS: &[(&str, &str)] = &[
    ("read", "Read"),
    ("grep", "Grep"),
    ("glob", "Glob"),
    ("edit", "Edit"),
    ("write", "Write"),
    ("bash", "Bash"),
];

/// Runs `task` in `work` with Claude Code and the model `model`: no settings, hooks or tool servers of the user's, nothing asked before a tool runs, nothing saved, and only the tools of `setup` it has a counterpart of. Every line it prints is kept in the file `transcript`.
///
/// Gives the count of what the model did and, when the run ended badly, why.
pub(crate) fn claude_code(
    setup: &Setup,
    model: &str,
    task: &Task,
    work: &Path,
    transcript: &Path,
) -> (Tally, Option<String>) {
    let mut process = Process::new(CLAUDE_CODE);
    process
        .args(["-p", &task.prompt])
        .args(["--output-format", "stream-json", "--verbose"])
        .args(["--model", model])
        .args(["--tools", &claude_tools(&setup.tools)])
        .args(["--permission-mode", "bypassPermissions"])
        .args(["--setting-sources", ""])
        .args(["--no-session-persistence", "--strict-mcp-config"]);
    if let Some((_, effort)) = setup.settings.iter().find(|(id, _)| id == EFFORT) {
        process.args(["--effort", effort]);
    }
    let mut tally = Tally::default();
    let mut refused = None;
    let ended = stream(process, work, transcript, setup.timeout, |line| {
        if let Some(reason) = claude_line(line, &mut tally) {
            refused = Some(reason);
        }
    });
    (tally, ended.or(refused))
}

/// Runs `task` in `work` with the program `words` name, the word that is [`PROMPT`] replaced by what the model is asked. Every line it prints is kept in the file `transcript`, and lines that report tool calls as [`event_line`] reads them are counted.
///
/// Gives the count of what the model did, which is empty for a program that reports no calls, and, when the run ended badly, why.
pub(crate) fn command(
    setup: &Setup,
    words: &[String],
    task: &Task,
    work: &Path,
    transcript: &Path,
) -> (Tally, Option<String>) {
    let mut tally = Tally::default();
    let Some((program, arguments)) = words.split_first() else {
        return (tally, Some("the command is empty".to_owned()));
    };
    let mut process = Process::new(program);
    for argument in arguments {
        process.arg(if argument == PROMPT {
            &task.prompt
        } else {
            argument
        });
    }
    let mut refused = None;
    let ended = stream(process, work, transcript, setup.timeout, |line| {
        if let Some(reason) = event_line(line, &mut tally) {
            refused = Some(reason);
        }
    });
    (tally, ended.or(refused))
}

/// The tools in `tools`, crab's names, as the comma-separated list Claude Code takes; a tool it has no counterpart of is left out.
fn claude_tools(tools: &[String]) -> String {
    let named: Vec<&str> = tools
        .iter()
        .filter_map(|tool| {
            CLAUDE_TOOLS
                .iter()
                .find(|(crab, _)| crab == tool)
                .map(|(_, claude)| *claude)
        })
        .collect();
    named.join(",")
}

/// Starts `process` in `work` with nothing on its input, hands each line it prints to `each` and keeps it in the file `transcript`, and waits for it to end, for at most `timeout`. What it writes to its error stream is kept in a file beside the transcript.
///
/// Gives why the run ended badly, when it did: the program could not be started, ran out of time, or ended unsuccessfully.
fn stream(
    mut process: Process,
    work: &Path,
    transcript: &Path,
    timeout: Duration,
    mut each: impl FnMut(&str),
) -> Option<String> {
    let errors = transcript.with_file_name(ERRORS);
    let stderr = File::create(&errors).map_or_else(|_| Stdio::null(), Stdio::from);
    let started = process
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn();
    let mut child = match started {
        Ok(child) => child,
        Err(error) => {
            let program = process.get_program().to_string_lossy().into_owned();
            return Some(format!("could not start {program}: {error}"));
        }
    };
    let (sender, lines) = mpsc::channel::<String>();
    if let Some(stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
    }
    let mut kept = File::create(transcript).ok();
    let deadline = Instant::now() + timeout;
    loop {
        match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(line) => {
                if let Some(file) = &mut kept {
                    let _ = writeln!(file, "{line}");
                }
                each(&line);
            }
            Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {
                let _ = child.kill();
                let _ = child.wait();
                return Some(format!("timed out after {}s", timeout.as_secs()));
            }
        }
    }
    match child.wait() {
        Ok(status) if status.success() => None,
        Ok(status) => Some(format!("ended with {status}: {}", tail(&errors))),
        Err(error) => Some(format!("could not wait for it: {error}")),
    }
}

/// The last [`ERROR_TAIL`] characters of the file `path`, on one line, or nothing when it cannot be read.
fn tail(path: &Path) -> String {
    let said = std::fs::read_to_string(path).unwrap_or_default();
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    let skip = said.chars().count().saturating_sub(ERROR_TAIL);
    said.chars().skip(skip).collect()
}

/// The number the field `name` of `usage` holds, or 0.
fn tokens(usage: &Value, name: &str) -> u64 {
    usage.get(name).and_then(Value::as_u64).unwrap_or(0)
}

/// Counts in `tally` what one line of Claude Code's stream says: the tool calls of an assistant message and the request it answers, counted once however many lines carry the message, and the calls a user message reports as failed.
///
/// Gives the reason when the line is a result that says the run failed.
fn claude_line(line: &str, tally: &mut Tally) -> Option<String> {
    let event: Value = serde_json::from_str(line).ok()?;
    let blocks = || event["message"]["content"].as_array().into_iter().flatten();
    match event["type"].as_str()? {
        "assistant" => {
            for block in blocks().filter(|block| block["type"] == "tool_use") {
                if let (Some(id), Some(name)) = (block["id"].as_str(), block["name"].as_str()) {
                    tally.called(id, name);
                }
            }
            let usage = &event["message"]["usage"];
            let sent = tokens(usage, "input_tokens")
                + tokens(usage, "cache_creation_input_tokens")
                + tokens(usage, "cache_read_input_tokens");
            match event["message"]["id"].as_str() {
                Some(id) => tally.request_once(id, sent),
                None => tally.request(sent),
            }
            None
        }
        "user" => {
            for block in blocks().filter(|block| block["is_error"] == true) {
                if let Some(id) = block["tool_use_id"].as_str() {
                    tally.failed(id);
                }
            }
            None
        }
        "result" if event["is_error"] == true => Some(
            event["result"]
                .as_str()
                .or(event["subtype"].as_str())
                .unwrap_or("the run failed")
                .to_owned(),
        ),
        _ => None,
    }
}

/// Counts in `tally` what one line of a baseline's stream says, when it is a JSON object of one of these kinds, named by its `type`: `tool_execution_start` with a `toolCallId` and a `toolName`, `tool_execution_end` with a `toolCallId` and whether it `isError`, and `message_end` with the `usage` of its `message` as `input`, `cacheRead` and `cacheWrite` tokens. Any other line counts for nothing.
///
/// Gives the reason when a `message_end` says its message stopped on an error.
fn event_line(line: &str, tally: &mut Tally) -> Option<String> {
    let event: Value = serde_json::from_str(line).ok()?;
    match event["type"].as_str()? {
        "tool_execution_start" => {
            if let (Some(id), Some(name)) =
                (event["toolCallId"].as_str(), event["toolName"].as_str())
            {
                tally.called(id, name);
            }
            None
        }
        "tool_execution_end" => {
            if let (Some(id), true) = (event["toolCallId"].as_str(), event["isError"] == true) {
                tally.failed(id);
            }
            None
        }
        "message_end" => {
            let message = &event["message"];
            let usage = &message["usage"];
            if usage.is_object() {
                tally.request(
                    tokens(usage, "input")
                        + tokens(usage, "cacheRead")
                        + tokens(usage, "cacheWrite"),
                );
            }
            (message["stopReason"] == "error").then(|| {
                message["errorMessage"]
                    .as_str()
                    .unwrap_or("the model's reply ended on an error")
                    .to_owned()
            })
        }
        _ => None,
    }
}

#[cfg(test)]
/// Tests for reading what a baseline prints.
mod tests {
    use super::*;
    use crate::tally::{Uses, edits};

    /// Claude Code's tool uses are counted by tool, a result flagged as an error fails its call, a message split over two lines is one request, and a failed result gives its reason.
    #[test]
    fn claude_code_lines_are_counted() {
        let mut tally = Tally::default();
        let lines = [
            r#"{"type":"system","subtype":"init"}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"thinking"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":900,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"a","name":"Read"}],"usage":{"input_tokens":10,"cache_creation_input_tokens":900,"cache_read_input_tokens":0}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","content":[{"type":"text","text":"x"},{"type":"tool_use","id":"b","name":"Edit"}],"usage":{"input_tokens":8,"cache_creation_input_tokens":100,"cache_read_input_tokens":900}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","is_error":true}]}}"#,
            r#"{"type":"assistant","message":{"id":"m3","content":[{"type":"tool_use","id":"c","name":"Edit"}],"usage":{"input_tokens":1100}}}"#,
            r#"{"type":"result","subtype":"success","is_error":false,"result":"done"}"#,
            "not json",
        ];
        for line in lines {
            assert_eq!(claude_line(line, &mut tally), None);
        }
        assert_eq!(
            claude_line(
                r#"{"type":"result","subtype":"error_during_execution","is_error":true}"#,
                &mut tally
            ),
            Some("error_during_execution".to_owned())
        );
        assert_eq!(tally.context(), 1100);
        assert_eq!((tally.requests(), tally.sent()), (3, 3018));
        let tools = tally.into_tools();
        assert_eq!(
            tools["read"],
            Uses {
                calls: 1,
                failed: 0
            }
        );
        assert_eq!(
            edits(&tools),
            Uses {
                calls: 2,
                failed: 1
            }
        );
    }

    /// A baseline's tool events are counted by tool, a call that ended in an error is a failed one, and a message that stopped on an error gives its reason.
    #[test]
    fn event_lines_are_counted() {
        let mut tally = Tally::default();
        let lines = [
            r#"{"type":"tool_execution_start","toolCallId":"a","toolName":"edit"}"#,
            r#"{"type":"tool_execution_end","toolCallId":"a","isError":true}"#,
            r#"{"type":"tool_execution_start","toolCallId":"b","toolName":"edit"}"#,
            r#"{"type":"tool_execution_end","toolCallId":"b","isError":false}"#,
            r#"{"type":"message_end","message":{"usage":{"input":10,"cacheRead":500,"cacheWrite":90},"stopReason":"toolUse"}}"#,
            r#"{"type":"message_update"}"#,
            "plain text",
        ];
        for line in lines {
            assert_eq!(event_line(line, &mut tally), None);
        }
        assert_eq!(
            event_line(
                r#"{"type":"message_end","message":{"stopReason":"error","errorMessage":"overloaded"}}"#,
                &mut tally
            ),
            Some("overloaded".to_owned())
        );
        assert_eq!(tally.context(), 600);
        assert_eq!((tally.requests(), tally.sent()), (1, 600));
        assert_eq!(
            edits(&tally.into_tools()),
            Uses {
                calls: 2,
                failed: 1
            }
        );
    }

    /// Crab's tool names become Claude Code's, and one it has no counterpart of is left out.
    #[test]
    fn tools_are_named_as_claude_code_names_them() {
        let tools = ["read", "ast_grep", "edit"].map(str::to_owned);
        assert_eq!(claude_tools(&tools), "Read,Edit");
    }
}
