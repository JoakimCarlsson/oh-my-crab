//! The `wait` tool: wait for, list, read and cancel the background runs of the conversation.

use std::sync::Arc;

use async_trait::async_trait;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::jobs::{DEFAULT_LINES, MAX_LINES};
use crate::{Concurrency, Jobs, Look, Tool, ToolError, ToolOutput, parse_args};

/// What the model is told about the tool.
const DESCRIPTION: &str = "Watch and control the background `bash` commands and `task` subagents you started. `action` picks what to do:\n\
- `wait` (the default): block until the next run ends and return its handle, status and output. A run that ended earlier and was not yet reported is returned at once, so wait once per run you started. `completed` means the run ended, not that it succeeded: a command's exit code is the first line of its output. Fails at once when nothing is running.\n\
- `list`: every run, running or ended, with its handle, whether it is a command or a subagent, how it stands and how long it ran.\n\
- `output`: what the command `id` printed so far, stdout and stderr together: its last 50 lines, or the last `lines` (at most 500). `pattern` keeps only the lines matching a regular expression, and `new` only what it printed since you last read it. A subagent's output comes with its result when it ends.\n\
- `cancel`: stop the run `id`: a command is killed with every process it started, a subagent is stopped. Its result is not reported again.\n\
When your turn ends while runs are still going, a new turn tells you when they end, unless you collected them with `wait` first.";

/// The actions the tool takes.
const ACTIONS: [&str; 4] = ["wait", "list", "output", "cancel"];

/// Waits for, lists, reads and cancels the background runs the conversation started.
pub struct Wait {
    /// The runs to watch.
    jobs: Arc<Jobs>,
    /// Whether the tool may cancel runs; the copy that only looks may not.
    cancels: bool,
}

impl Wait {
    /// Creates the tool watching `jobs`, able to cancel them.
    pub fn new(jobs: Arc<Jobs>) -> Self {
        Self {
            jobs,
            cancels: true,
        }
    }
}

/// Arguments of a `wait` call.
#[derive(Deserialize)]
struct Args {
    /// What to do: `wait` when left out.
    #[serde(default)]
    action: Option<String>,
    /// The handle of the run to read or cancel.
    #[serde(default)]
    id: Option<String>,
    /// For `output`: the regular expression the lines shown must match.
    #[serde(default)]
    pattern: Option<String>,
    /// For `output`: only what was printed since the last read.
    #[serde(default)]
    new: bool,
    /// For `output`: how many of the last lines to show.
    #[serde(default)]
    lines: Option<usize>,
}

impl Args {
    /// The handle the action needs.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when no handle was given.
    fn id(&self, action: &str) -> Result<&str, ToolError> {
        self.id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .map(str::trim)
            .ok_or_else(|| {
                ToolError::InvalidArguments(format!("`{action}` needs the `id` of a run"))
            })
    }

    /// What an `output` call asks to see.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the pattern is not a valid regular expression.
    fn look(&self) -> Result<Look, ToolError> {
        let pattern = match &self.pattern {
            Some(pattern) if !pattern.is_empty() => Some(Regex::new(pattern).map_err(|error| {
                ToolError::InvalidArguments(format!("invalid pattern: {error}"))
            })?),
            _ => None,
        };
        Ok(Look {
            fresh: self.new,
            pattern,
            lines: self.lines.unwrap_or(DEFAULT_LINES).clamp(1, MAX_LINES),
        })
    }
}

/// The action `args` names, `wait` when none.
fn action_of(args: &Value) -> &str {
    args["action"].as_str().unwrap_or("wait")
}

#[async_trait]
impl Tool for Wait {
    /// Returns `wait`.
    fn name(&self) -> &'static str {
        "wait"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &str {
        DESCRIPTION
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": ACTIONS, "description": "What to do; `wait` when left out." },
                "id": { "type": "string", "description": "For output and cancel: the handle of the run, such as bg-1." },
                "pattern": { "type": "string", "description": "For output: show only the lines matching this regular expression." },
                "new": { "type": "boolean", "description": "For output: show only what was printed since you last read this run." },
                "lines": { "type": "integer", "minimum": 1, "maximum": MAX_LINES, "description": "For output: how many of the last lines to show, 50 by default." }
            }
        })
    }

    /// The copy that refuses `cancel` only looks; the full tool can stop runs.
    fn read_only(&self) -> bool {
        !self.cancels
    }

    /// Cancelling stops a run and runs alone; every other action only looks.
    fn concurrency(&self, args: &Value) -> Concurrency {
        if action_of(args) == "cancel" {
            Concurrency::Exclusive
        } else {
            Concurrency::Shared
        }
    }

    /// Cancelling kills processes, so the user approves it as they approve commands; the other actions need no approval.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        (action_of(args) == "cancel").then(|| "wait:cancel".to_owned())
    }

    /// Describes a call.
    fn describe(&self, args: &Value) -> String {
        let id = args["id"].as_str().unwrap_or("?");
        match action_of(args) {
            "list" => "list the background runs".to_owned(),
            "output" => format!("read the output of {id}"),
            "cancel" => format!("cancel {id}"),
            _ => "wait for a background run".to_owned(),
        }
    }

    /// A copy that refuses `cancel`, for plan mode and read-only agents.
    fn looking(&self) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            jobs: self.jobs.clone(),
            cancels: false,
        }))
    }

    /// Carries out the action.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an unknown action, a missing handle or an invalid pattern, and [`ToolError::Failed`] when nothing is running to wait for, the handle names no run, the run cannot be cancelled, or this copy may not cancel.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let action = args.action.clone().unwrap_or_else(|| "wait".to_owned());
        let text = match action.as_str() {
            "wait" => self.jobs.wait().await?.report(),
            "list" => {
                let listed = self.jobs.list();
                if listed.is_empty() {
                    "No background runs.".to_owned()
                } else {
                    listed
                        .iter()
                        .map(|run| run.line())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "output" => self.jobs.output(args.id("output")?, &args.look()?)?,
            "cancel" if !self.cancels => {
                return Err(ToolError::Failed(
                    "cancelling a background run is not allowed here".to_owned(),
                ));
            }
            "cancel" => self.jobs.cancel(args.id("cancel")?)?,
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "unknown action `{other}`; use one of {}",
                    ACTIONS.join(", ")
                )));
            }
        };
        Ok(ToolOutput::new(text))
    }
}

#[cfg(test)]
/// Tests for the `wait` tool.
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::{Feed, JobKind, Launch};

    /// The handle, status and output of a run come back, and a second wait with nothing left fails.
    #[tokio::test]
    async fn reports_a_run_and_fails_when_nothing_is_left() {
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Wait::new(jobs.clone());
        let id = jobs.spawn("job".into(), None, async { Ok("fine".to_owned()) });
        let out = tool.call(json!({})).await.unwrap();
        assert_eq!(out.text, format!("{id} completed: job\nfine"));
        assert!(matches!(
            tool.call(json!({})).await,
            Err(ToolError::Failed(_))
        ));
    }

    /// The tool lists runs, reads a command's output with a pattern and only what is new, and cancels it; only cancelling needs approval and runs alone, and the copy that only looks refuses it.
    #[tokio::test]
    async fn lists_reads_and_cancels() {
        let jobs = Arc::new(Jobs::new(|_| {}));
        let tool = Wait::new(jobs.clone());
        assert_eq!(
            tool.call(json!({"action": "list"})).await.unwrap().text,
            "No background runs."
        );
        let feed = Feed::new();
        let id = jobs.spawn_with(
            Launch::new("dev".into(), None, JobKind::Command).fed_by(feed.clone()),
            std::future::pending(),
        );
        feed.push(b"ready\nGET /\nGET /a\n");
        let listed = tool.call(json!({"action": "list"})).await.unwrap().text;
        assert!(listed.starts_with(&format!("{id} command running ")));
        let read = |args: Value| {
            let tool = &tool;
            async move { tool.call(args).await.unwrap().text }
        };
        let got = read(json!({"action": "output", "id": id, "pattern": "^GET", "new": true})).await;
        assert!(got.ends_with("dev\nGET /\nGET /a"));
        feed.push(b"GET /b\n");
        let got = read(json!({"action": "output", "id": id, "new": true})).await;
        assert!(got.ends_with("dev\nGET /b"));
        assert!(matches!(
            tool.call(json!({"action": "output", "id": id, "pattern": "("}))
                .await,
            Err(ToolError::InvalidArguments(_))
        ));
        assert!(matches!(
            tool.call(json!({"action": "output"})).await,
            Err(ToolError::InvalidArguments(_))
        ));
        let cancel = json!({"action": "cancel", "id": id});
        assert_eq!(tool.approval_scope(&cancel).as_deref(), Some("wait:cancel"));
        assert_eq!(tool.approval_scope(&json!({"action": "list"})), None);
        assert_eq!(tool.concurrency(&cancel), Concurrency::Exclusive);
        assert_eq!(tool.concurrency(&json!({})), Concurrency::Shared);
        let looking = tool.looking().unwrap();
        assert!(looking.read_only());
        assert!(matches!(
            looking.call(cancel.clone()).await,
            Err(ToolError::Failed(_))
        ));
        let said = tool.call(cancel).await.unwrap().text;
        assert!(said.contains("Killed with the processes it started"));
        assert!(said.ends_with("GET /b"));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let listed = tool.call(json!({"action": "list"})).await.unwrap().text;
        assert!(listed.starts_with(&format!("{id} command cancelled ")));
        assert!(matches!(
            tool.call(json!({})).await,
            Err(ToolError::Failed(_))
        ));
        assert!(matches!(
            tool.call(json!({"action": "stop"})).await,
            Err(ToolError::InvalidArguments(_))
        ));
    }
}
