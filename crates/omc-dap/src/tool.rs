//! The `debug` tool: one tool whose `action` starts, steers, inspects and ends debugging sessions.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{Sandbox, Tool, ToolError, ToolOutput};
use serde_json::{Map, Value, json};

use crate::session::{FunctionBreakpoint, LineBreakpoint, Motion, Session, Spot, Start};
use crate::sessions::Sessions;

/// What the tool tells the model about itself.
const DESCRIPTION: &str = include_str!("prompts/debug.md");

/// How long the program is waited for to stop or end, in seconds, unless the call says.
const DEFAULT_WAIT: f64 = 15.0;

/// The longest the program is waited for in one call, in seconds.
const LONGEST_WAIT: f64 = 300.0;

/// How long ending a session may take.
const DISCONNECT_WAIT: Duration = Duration::from_secs(5);

/// How many levels below the variables asked for are shown unless the call says.
const DEFAULT_DEPTH: usize = 1;

/// The most levels below the variables asked for that are shown.
const DEEPEST: usize = 4;

/// How many children of each variable are shown unless the call says.
const DEFAULT_LIMIT: usize = 50;

/// The most children of each variable that are shown.
const WIDEST: usize = 200;

/// The actions the tool takes.
const ACTIONS: [&str; 20] = [
    "launch",
    "attach",
    "break",
    "clear",
    "breakpoints",
    "exceptions",
    "continue",
    "step_over",
    "step_in",
    "step_out",
    "pause",
    "wait",
    "threads",
    "stack",
    "scopes",
    "variables",
    "evaluate",
    "output",
    "sessions",
    "disconnect",
];

/// Drives debuggers for the model, in sessions that last across calls.
pub struct DebugTool {
    /// The working directory, which programs, folders and source files must be in.
    sandbox: Sandbox,
    /// The conversation's sessions.
    sessions: Arc<Sessions>,
}

impl DebugTool {
    /// The tool, confined to `sandbox`, keeping its sessions in `sessions`.
    pub fn new(sandbox: Sandbox, sessions: Arc<Sessions>) -> Self {
        Self { sandbox, sessions }
    }

    /// Starts a session as the launch or attach call `args` says.
    ///
    /// # Errors
    ///
    /// Returns why when the arguments are wrong or the session does not start.
    async fn start(&self, args: &Value, attach: bool) -> Result<String, ToolError> {
        let start = self.start_of(args, attach)?;
        let id = self.sessions.reserve().map_err(ToolError::Failed)?;
        match Session::start(id, self.sandbox.clone(), start).await {
            Ok((session, report)) => {
                self.sessions.admit(Some(session));
                Ok(report)
            }
            Err(reason) => {
                self.sessions.admit(None);
                Err(ToolError::Failed(reason))
            }
        }
    }

    /// What starts a session, read from the launch or attach call `args`.
    ///
    /// # Errors
    ///
    /// Returns why when a path is outside the working directory or missing, or what to attach to is not given.
    fn start_of(&self, args: &Value, attach: bool) -> Result<Start, ToolError> {
        let config = args["config"].as_object().cloned().unwrap_or_default();
        let program = match text(args, "program") {
            Some(program) => {
                let resolved = self.sandbox.resolve(program)?;
                if !resolved.exists() {
                    return Err(ToolError::Failed(format!(
                        "{program} does not exist; build it first"
                    )));
                }
                Some(resolved)
            }
            None => None,
        };
        let cwd = match text(args, "cwd") {
            Some(cwd) => {
                let resolved = self.sandbox.resolve(cwd)?;
                if !resolved.is_dir() {
                    return Err(ToolError::Failed(format!("{cwd} is not a folder")));
                }
                resolved
            }
            None => self.sandbox.root().to_path_buf(),
        };
        let pid = args["pid"]
            .as_u64()
            .map(u32::try_from)
            .transpose()
            .map_err(|_| ToolError::InvalidArguments("pid is not a process id".to_owned()))?;
        let address = text(args, "address").map(str::to_owned);
        if attach && pid.is_none() && address.is_none() {
            return Err(ToolError::InvalidArguments(
                "attach needs pid or address".to_owned(),
            ));
        }
        if !attach && program.is_none() && config.is_empty() {
            return Err(ToolError::InvalidArguments(
                "launch needs program, or config naming what to run".to_owned(),
            ));
        }
        let mut breakpoints = Vec::new();
        for spec in strings(args, "breakpoints") {
            breakpoints.push(self.spot_of(&spec)?);
        }
        let env: Map<String, Value> = args["env"]
            .as_object()
            .map(|env| {
                env.iter()
                    .map(|(key, value)| {
                        let value = value
                            .as_str()
                            .map_or_else(|| value.to_string(), str::to_owned);
                        (key.clone(), Value::String(value))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Start {
            program,
            args: strings(args, "args"),
            cwd,
            env,
            pid: if attach { pid } else { None },
            address: if attach { address } else { None },
            language: text(args, "language").map(str::to_owned),
            adapter: text(args, "adapter").map(str::to_owned),
            config,
            stop_on_entry: args["stop_on_entry"].as_bool().unwrap_or(false),
            breakpoints,
            exceptions: args.get("filters").map(|_| strings(args, "filters")),
            timeout: wait_of(args),
        })
    }

    /// The breakpoint `spec` names: `file:line`, or a function's name.
    ///
    /// # Errors
    ///
    /// Returns why when the file is outside the working directory or does not exist.
    fn spot_of(&self, spec: &str) -> Result<Spot, ToolError> {
        match split_location(spec) {
            Some((file, line)) => Ok(Spot::line(self.source(file)?, line)),
            None => Ok(Spot::function(spec.trim())),
        }
    }

    /// The source file `file`, resolved inside the working directory.
    ///
    /// # Errors
    ///
    /// Returns why when it is outside the working directory or is not a file.
    fn source(&self, file: &str) -> Result<PathBuf, ToolError> {
        let resolved = self.sandbox.resolve(file)?;
        if !resolved.is_file() {
            return Err(ToolError::Failed(format!("{file} is not a file")));
        }
        Ok(resolved)
    }

    /// The breakpoint the `break` call `args` sets.
    ///
    /// # Errors
    ///
    /// Returns why when neither a place nor a function is given, or the file is not in the working directory.
    fn breakpoint_of(&self, args: &Value) -> Result<Spot, ToolError> {
        let condition = text(args, "condition").map(str::to_owned);
        let hit = text(args, "hit_condition").map(str::to_owned);
        if let Some(name) = text(args, "function") {
            return Ok(Spot::Function(FunctionBreakpoint {
                name: name.to_owned(),
                condition,
                hit,
                ..FunctionBreakpoint::default()
            }));
        }
        let (file, line) = self.place_of(args)?;
        let line = line.ok_or_else(|| {
            ToolError::InvalidArguments("break needs line with file, or function".to_owned())
        })?;
        Ok(Spot::Line(
            file,
            LineBreakpoint {
                line,
                condition,
                hit,
                log: text(args, "log_message").map(str::to_owned),
                ..LineBreakpoint::default()
            },
        ))
    }

    /// The file and line of a call, taking a `file` written as `file:line` when `line` is not given.
    ///
    /// # Errors
    ///
    /// Returns why when there is no file or it is not in the working directory.
    fn place_of(&self, args: &Value) -> Result<(PathBuf, Option<i64>), ToolError> {
        let file = text(args, "file").ok_or_else(|| {
            ToolError::InvalidArguments("file is required, or function".to_owned())
        })?;
        let line = args["line"].as_i64();
        match (line, split_location(file)) {
            (None, Some((file, line))) => Ok((self.source(file)?, Some(line))),
            _ => Ok((self.source(file)?, line)),
        }
    }

    /// Runs `action` with `args` on the session they name or the one used last.
    ///
    /// # Errors
    ///
    /// Returns why when there is no such session or the action fails.
    async fn on_session(&self, action: &str, args: &Value) -> Result<String, ToolError> {
        let shared = self
            .sessions
            .get(text(args, "session"))
            .map_err(ToolError::Failed)?;
        let mut session = shared.lock().await;
        let thread = args["thread"].as_i64();
        let frame = args["frame"]
            .as_u64()
            .and_then(|frame| usize::try_from(frame).ok())
            .unwrap_or(0);
        let depth = bounded(args, "depth", DEFAULT_DEPTH, DEEPEST);
        let limit = bounded(args, "limit", DEFAULT_LIMIT, WIDEST).max(1);
        let motion = match action {
            "continue" => Some(Motion::Continue),
            "step_over" => Some(Motion::Over),
            "step_in" => Some(Motion::In),
            "step_out" => Some(Motion::Out),
            _ => None,
        };
        if action == "disconnect" {
            let report = session
                .disconnect(args["terminate"].as_bool(), DISCONNECT_WAIT)
                .await;
            self.sessions.remove(&session.id);
            return Ok(report);
        }
        let notice = session.notices().await;
        if let Some(motion) = motion {
            if let Some(notice) = notice {
                return Ok(format!(
                    "Not resumed, since you had not seen this yet. {notice}"
                ));
            }
            return session
                .resume(motion, thread, wait_of(args))
                .await
                .map_err(ToolError::Failed);
        }
        let result = match action {
            "pause" => session.pause(thread, wait_of(args)).await,
            "wait" => match notice {
                Some(notice) => return Ok(notice),
                None => Ok(session.wait(wait_of(args)).await),
            },
            "threads" => session.threads().await,
            "stack" => {
                let count = args["count"]
                    .as_u64()
                    .and_then(|count| usize::try_from(count).ok());
                session.stack(thread, count).await
            }
            "scopes" => session.scopes(thread, frame).await,
            "variables" => {
                session
                    .variables(
                        thread,
                        frame,
                        text(args, "scope"),
                        args["ref"].as_i64(),
                        depth,
                        limit,
                    )
                    .await
            }
            "evaluate" => {
                let expression = text(args, "expression").ok_or_else(|| {
                    ToolError::InvalidArguments("evaluate needs expression".to_owned())
                })?;
                session
                    .evaluate(expression, thread, frame, depth, limit)
                    .await
            }
            "break" => {
                let spot = self.breakpoint_of(args)?;
                session
                    .add(spot)
                    .await
                    .map(|placed| format!("Breakpoint set: {placed}"))
            }
            "clear" => {
                let all = args["all"].as_bool().unwrap_or(false);
                let function = text(args, "function");
                let (file, line) = if all || function.is_some() {
                    (None, None)
                } else {
                    let (file, line) = self.place_of(args)?;
                    (Some(file), line)
                };
                session.clear(file, line, function, all).await
            }
            "breakpoints" => Ok(session
                .breakpoint_listing()
                .unwrap_or_else(|| "No breakpoints are set.".to_owned())),
            "exceptions" => session.set_exceptions(strings(args, "filters")).await,
            "output" => Ok(session.output(args["all"].as_bool().unwrap_or(false))),
            other => Err(format!("unknown action {other}")),
        };
        let report = result.map_err(ToolError::Failed)?;
        Ok(match notice {
            Some(notice) => format!("{notice}\n\n{report}"),
            None => report,
        })
    }

    /// Lists the open sessions.
    fn listing(&self) -> String {
        let all = self.sessions.all();
        if all.is_empty() {
            return "No debugging session is open.".to_owned();
        }
        let lines: Vec<String> = all
            .iter()
            .map(|shared| match shared.try_lock() {
                Ok(session) => session.describe(),
                Err(_) => "a session busy with another call".to_owned(),
            })
            .collect();
        format!("Sessions:\n{}", lines.join("\n"))
    }
}

#[async_trait]
impl Tool for DebugTool {
    /// Returns `debug`.
    fn name(&self) -> &'static str {
        "debug"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        DESCRIPTION
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        schema()
    }

    /// Launching and attaching run code, so calls may need the user's approval.
    fn needs_approval(&self) -> bool {
        true
    }

    /// `debug` for launching and attaching, which run code or reach a process; nothing for the actions on a session that was already allowed.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        matches!(text(args, "action"), Some("launch" | "attach")).then(|| "debug".to_owned())
    }

    /// Shows the action and what it is about.
    fn describe(&self, args: &Value) -> String {
        describe(args)
    }

    /// Takes the action the call asks for.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let action = text(&args, "action").unwrap_or_default();
        let report = match action {
            "launch" => self.start(&args, false).await?,
            "attach" => self.start(&args, true).await?,
            "sessions" => self.listing(),
            known if ACTIONS.contains(&known) => self.on_session(known, &args).await?,
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "unknown action {other}; the actions are {}",
                    ACTIONS.join(", ")
                )));
            }
        };
        Ok(ToolOutput::new(report))
    }
}

/// The one-line account of the call `args` shown to the user.
fn describe(args: &Value) -> String {
    let action = text(args, "action").unwrap_or("?");
    let mut line = action.to_owned();
    match action {
        "launch" => {
            line.push(' ');
            line.push_str(text(args, "program").unwrap_or("(configured program)"));
            for argument in strings(args, "args") {
                line.push(' ');
                line.push_str(&argument);
            }
            if args["config"]
                .as_object()
                .is_some_and(|config| !config.is_empty())
            {
                line.push_str(&format!(" with settings {}", args["config"]));
            }
        }
        "attach" => match (args["pid"].as_u64(), text(args, "address")) {
            (Some(pid), _) => line.push_str(&format!(" to process {pid}")),
            (None, Some(address)) => line.push_str(&format!(" to {address}")),
            (None, None) => {}
        },
        "break" | "clear" => {
            if let Some(function) = text(args, "function") {
                line.push_str(&format!(" function {function}"));
            } else if let Some(file) = text(args, "file") {
                line.push_str(&format!(" {file}"));
                if let Some(number) = args["line"].as_i64() {
                    line.push_str(&format!(":{number}"));
                }
            } else if args["all"].as_bool().unwrap_or(false) {
                line.push_str(" all");
            }
            if let Some(condition) = text(args, "condition") {
                line.push_str(&format!(" if {condition}"));
            }
        }
        "evaluate" => {
            line.push(' ');
            line.push_str(text(args, "expression").unwrap_or_default());
        }
        "variables" => match args["ref"].as_i64() {
            Some(reference) => line.push_str(&format!(" ref {reference}")),
            None => line.push_str(&format!(" frame {}", args["frame"].as_u64().unwrap_or(0))),
        },
        _ => {}
    }
    if let Some(session) = text(args, "session") {
        line.push_str(&format!(" [{session}]"));
    }
    line
}

/// The arguments the tool takes.
fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ACTIONS, "description": "What to do."},
            "session": {"type": "string", "description": "The session to act on, such as dbg-1; the one used last when left out."},
            "program": {"type": "string", "description": "launch: the program, script or Go package folder, inside the working directory."},
            "args": {"type": "array", "items": {"type": "string"}, "description": "launch: the program's arguments."},
            "cwd": {"type": "string", "description": "launch: the folder it runs in; the working directory when left out."},
            "env": {"type": "object", "additionalProperties": {"type": "string"}, "description": "launch: variables added to its environment."},
            "language": {"type": "string", "description": "launch, attach: c, cpp, rust, go, python or another language, when the program does not say."},
            "adapter": {"type": "string", "description": "launch, attach: the debug adapter to use, such as lldb-dap, gdb, codelldb, dlv or debugpy."},
            "config": {"type": "object", "description": "launch, attach: adapter-specific settings added to the request, such as {\"module\": \"pkg\"} for debugpy."},
            "stop_on_entry": {"type": "boolean", "description": "launch: stop when the program starts."},
            "breakpoints": {"type": "array", "items": {"type": "string"}, "description": "launch, attach: breakpoints set before the program runs, each \"file:line\" or a function name."},
            "pid": {"type": "integer", "minimum": 1, "description": "attach: the process to attach to."},
            "address": {"type": "string", "description": "attach: host:port of a debug adapter already listening."},
            "file": {"type": "string", "description": "break, clear: the source file, or \"file:line\"."},
            "line": {"type": "integer", "minimum": 1, "description": "break, clear: the line."},
            "function": {"type": "string", "description": "break, clear: the function."},
            "condition": {"type": "string", "description": "break: stop only when this expression is true."},
            "hit_condition": {"type": "string", "description": "break: stop only on these hits, such as \"3\" or \">= 3\"."},
            "log_message": {"type": "string", "description": "break: log this instead of stopping; expressions go in braces."},
            "filters": {"type": "array", "items": {"type": "string"}, "description": "exceptions, launch: the exception filters to turn on."},
            "thread": {"type": "integer", "description": "The thread; the one that stopped when left out."},
            "frame": {"type": "integer", "minimum": 0, "description": "The frame number, 0 the innermost."},
            "scope": {"type": "string", "description": "variables: only this scope, such as Locals or Registers."},
            "ref": {"type": "integer", "minimum": 1, "description": "variables: list the children of this [ref N]."},
            "depth": {"type": "integer", "minimum": 0, "maximum": DEEPEST, "description": "variables, evaluate: how many levels of children to expand."},
            "limit": {"type": "integer", "minimum": 1, "maximum": WIDEST, "description": "variables, evaluate: the most children shown per level."},
            "count": {"type": "integer", "minimum": 1, "description": "stack: the most frames shown."},
            "expression": {"type": "string", "description": "evaluate: the expression."},
            "timeout": {"type": "number", "minimum": 0, "maximum": LONGEST_WAIT, "description": "launch, continue, steps, pause, wait: seconds to wait for a stop before reporting it is still running."},
            "terminate": {"type": "boolean", "description": "disconnect: end the program too; by default a launched one is ended and an attached one left running."},
            "all": {"type": "boolean", "description": "clear: every breakpoint; output: everything kept, not only what is new."},
        },
        "required": ["action"],
    })
}

/// The non-empty string at `key` in `args`.
fn text<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key]
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// The strings of the list at `key` in `args`.
fn strings(args: &Value, key: &str) -> Vec<String> {
    args[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The number at `key` in `args`, or `default`, at most `most`.
fn bounded(args: &Value, key: &str, default: usize, most: usize) -> usize {
    args[key]
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(default)
        .min(most)
}

/// How long the call `args` waits for the program to stop.
fn wait_of(args: &Value) -> Duration {
    let seconds = args["timeout"].as_f64().unwrap_or(DEFAULT_WAIT);
    Duration::from_secs_f64(seconds.clamp(0.0, LONGEST_WAIT))
}

/// `spec` split into a file and a line when it is written `file:line`.
fn split_location(spec: &str) -> Option<(&str, i64)> {
    let (file, line) = spec.trim().rsplit_once(':')?;
    let line = line.trim().parse::<i64>().ok().filter(|line| *line > 0)?;
    (!file.is_empty()).then_some((file, line))
}

#[cfg(test)]
/// Tests for the tool's arguments.
mod tests {
    use super::*;

    /// Places are read from `file:line`, and anything else is a function.
    #[test]
    fn locations_split() {
        assert_eq!(split_location("src/a.c:12"), Some(("src/a.c", 12)));
        assert_eq!(split_location("pkg::module::run"), None);
        assert_eq!(split_location("main"), None);
        assert_eq!(split_location("a.c:0"), None);
    }

    /// Launching and attaching need approval, under one scope; every other action does not.
    #[test]
    fn only_starting_needs_approval() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let tool = DebugTool::new(Sandbox::new(&dir).unwrap(), Arc::new(Sessions::new()));
        for action in ["launch", "attach"] {
            assert_eq!(
                tool.approval_scope(&json!({"action": action})).as_deref(),
                Some("debug")
            );
        }
        for action in ["break", "continue", "evaluate", "variables", "disconnect"] {
            assert_eq!(tool.approval_scope(&json!({"action": action})), None);
        }
        assert!(!tool.read_only());
    }

    /// The account of a call shows what it reaches.
    #[test]
    fn calls_are_described() {
        assert_eq!(
            describe(&json!({"action": "launch", "program": "build/app", "args": ["-n", "3"]})),
            "launch build/app -n 3"
        );
        assert_eq!(
            describe(&json!({"action": "attach", "pid": 42})),
            "attach to process 42"
        );
        assert_eq!(
            describe(
                &json!({"action": "break", "file": "a.c", "line": 3, "condition": "i > 2", "session": "dbg-2"})
            ),
            "break a.c:3 if i > 2 [dbg-2]"
        );
        assert_eq!(
            describe(&json!({"action": "launch", "config": {"module": "app"}})),
            "launch (configured program) with settings {\"module\":\"app\"}"
        );
    }

    /// Waits default to fifteen seconds and are held to the longest allowed.
    #[test]
    fn waits_are_bounded() {
        assert_eq!(wait_of(&json!({})), Duration::from_secs(15));
        assert_eq!(wait_of(&json!({"timeout": 9999})), Duration::from_secs(300));
        assert_eq!(bounded(&json!({"depth": 9}), "depth", 1, DEEPEST), DEEPEST);
    }

    /// Paths outside the working directory are refused before anything starts, and so is a launch without a program.
    #[tokio::test]
    async fn paths_are_confined() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let tool = DebugTool::new(Sandbox::new(&dir).unwrap(), Arc::new(Sessions::new()));
        let outside = tool
            .call(json!({"action": "launch", "program": "/bin/sh"}))
            .await;
        assert!(matches!(outside, Err(ToolError::OutsidePath(_))));
        let cwd = tool
            .call(json!({"action": "launch", "program": "Cargo.toml", "cwd": "../.."}))
            .await;
        assert!(matches!(cwd, Err(ToolError::OutsidePath(_))));
        let bare = tool.call(json!({"action": "launch"})).await;
        assert!(matches!(bare, Err(ToolError::InvalidArguments(_))));
        let no_session = tool.call(json!({"action": "stack"})).await;
        assert!(
            matches!(no_session, Err(ToolError::Failed(reason)) if reason.contains("launch or attach"))
        );
    }
}
