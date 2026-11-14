//! The `github` tool: pull requests, issues, searches and Actions runs through
//! the `gh` command line tool, signed in as the user.
//!
//! Each action becomes one or more `gh` command lines, built by
//! [`command::Request`]; what `gh` prints as JSON is turned into text for the
//! model by [`render`].

mod command;
mod render;
#[cfg(test)]
mod tests;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Sandbox, Tool, ToolError, ToolOutput};
use command::{Request, SearchKind};

/// Longest text of one `gh` result kept, in characters, before it is cut.
const MAX_TEXT: usize = 60_000;

/// How long one `gh` command may take.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);

/// How long `run_watch` waits when the call does not say, in seconds.
const DEFAULT_WATCH: u64 = 600;

/// The longest `run_watch` may wait, in seconds.
const MAX_WATCH: u64 = 3600;

/// How often `run_watch` looks when the call does not say, in seconds.
const DEFAULT_INTERVAL: u64 = 10;

/// How many log lines of each failed job `run_watch` shows when the call does not say.
const DEFAULT_TAIL: usize = 60;

/// How many search results come back when the call does not say.
const DEFAULT_LIMIT: u32 = 10;

/// The most search results that come back.
const MAX_LIMIT: u32 = 50;

/// Works with GitHub through the `gh` command line tool.
pub struct GitHub {
    /// The working directory `gh` runs in, whose repository it uses by default.
    root: PathBuf,
    /// Whether `pr_checkout` is allowed; false in the copy that only looks.
    checkout: bool,
}

impl GitHub {
    /// The tool, running `gh` in the root of `sandbox`.
    pub fn new(sandbox: &Sandbox) -> Self {
        Self {
            root: sandbox.root().to_path_buf(),
            checkout: true,
        }
    }
}

/// Arguments of a `github` call.
#[derive(Deserialize)]
struct Args {
    /// What to do: `pr_view`, `pr_diff`, `pr_checkout`, `issue_view`, `search` or `run_watch`.
    action: String,
    /// The repository as `owner/name`.
    repo: Option<String>,
    /// The pull request or issue number.
    number: Option<u64>,
    /// For `search`: what to look for.
    query: Option<String>,
    /// For `search`: `issues`, `prs` or `code`.
    kind: Option<String>,
    /// For `search`: how many results.
    limit: Option<u32>,
    /// For `pr_diff`: list changed files only.
    #[serde(default)]
    name_only: bool,
    /// For `pr_checkout`: the local branch name.
    branch: Option<String>,
    /// For `pr_checkout`: reset an existing local branch.
    #[serde(default)]
    force: bool,
    /// For `run_watch`: the run id.
    run: Option<u64>,
    /// For `run_watch`: seconds to wait at most.
    timeout: Option<u64>,
    /// For `run_watch`: seconds between looks.
    interval: Option<u64>,
    /// For `run_watch`: log lines per failed job.
    tail: Option<usize>,
}

/// Returns the `number` of `args`, or an error naming `action` when it is missing.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when there is no number.
fn number(args: &Args) -> Result<u64, ToolError> {
    args.number
        .ok_or_else(|| ToolError::InvalidArguments(format!("{} needs `number`", args.action)))
}

/// Checks that `repo`, when given, has the form `owner/name`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for any other form.
fn check_repo(repo: Option<&str>) -> Result<(), ToolError> {
    let Some(repo) = repo else {
        return Ok(());
    };
    let mut parts = repo.split('/');
    let valid = matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(owner), Some(name), None)
            if !owner.is_empty() && !name.is_empty() && !repo.starts_with('-')
    );
    if valid {
        Ok(())
    } else {
        Err(ToolError::InvalidArguments(format!(
            "repo is {repo}; write it as owner/name"
        )))
    }
}

#[async_trait]
impl Tool for GitHub {
    /// Returns `github`.
    fn name(&self) -> &'static str {
        "github"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../../prompts/github.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["pr_view", "pr_diff", "pr_checkout", "issue_view", "search", "run_watch"],
                    "description": "What to do."
                },
                "repo": { "type": "string", "description": "The repository as owner/name; the working directory's when left out." },
                "number": { "type": "integer", "minimum": 1, "description": "The pull request or issue number." },
                "query": { "type": "string", "description": "For search: what to look for, in GitHub search syntax." },
                "kind": { "type": "string", "enum": ["issues", "prs", "code"], "description": "For search: what to search; issues when left out." },
                "limit": { "type": "integer", "minimum": 1, "maximum": 50, "description": "For search: how many results, 10 by default." },
                "name_only": { "type": "boolean", "description": "For pr_diff: list the changed files instead of the diff." },
                "branch": { "type": "string", "description": "For pr_checkout: the local branch name." },
                "force": { "type": "boolean", "description": "For pr_checkout: reset an existing local branch to the pull request's." },
                "run": { "type": "integer", "minimum": 1, "description": "For run_watch: the run id; the latest run of the current branch when left out." },
                "timeout": { "type": "integer", "minimum": 1, "maximum": 3600, "description": "For run_watch: seconds to wait at most, 600 by default." },
                "interval": { "type": "integer", "minimum": 3, "description": "For run_watch: seconds between looks, 10 by default." },
                "tail": { "type": "integer", "minimum": 1, "description": "For run_watch: log lines shown per failed job, 60 by default." }
            },
            "required": ["action"]
        })
    }

    /// The copy that refuses `pr_checkout` only looks; the full tool can change the working directory.
    fn read_only(&self) -> bool {
        !self.checkout
    }

    /// Checking a branch out changes the working directory; every other action only looks.
    fn concurrency(&self, args: &Value) -> Concurrency {
        if args["action"] == "pr_checkout" {
            Concurrency::Exclusive
        } else {
            Concurrency::Shared
        }
    }

    /// Every call reaches GitHub as the user, so the user approves it.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Checking out is approved apart from the actions that only look.
    fn approval_scope(&self, args: &Value) -> Option<String> {
        Some(if args["action"] == "pr_checkout" {
            "github:pr_checkout".to_owned()
        } else {
            "github".to_owned()
        })
    }

    /// Shows the action and what it is about.
    fn describe(&self, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or("?");
        let mut text = action.to_owned();
        if let Some(repo) = args["repo"].as_str() {
            text.push_str(&format!(" {repo}"));
        }
        if let Some(number) = args["number"].as_u64() {
            text.push_str(&format!(" #{number}"));
        }
        if let Some(run) = args["run"].as_u64() {
            text.push_str(&format!(" run {run}"));
        }
        if let Some(query) = args["query"].as_str() {
            text.push_str(&format!(" {query}"));
        }
        text
    }

    /// A copy that refuses `pr_checkout`, for plan mode and read-only agents.
    fn looking(&self) -> Option<Arc<dyn Tool>> {
        Some(Arc::new(Self {
            root: self.root.clone(),
            checkout: false,
        }))
    }

    /// Runs the action through `gh`.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        check_repo(args.repo.as_deref())?;
        let repo = args.repo.clone();
        let text = match args.action.as_str() {
            "pr_view" => {
                let request = Request::PrView {
                    repo,
                    number: number(&args)?,
                };
                render::pull_request(&self.json(&request).await?)
            }
            "issue_view" => {
                let request = Request::IssueView {
                    repo,
                    number: number(&args)?,
                };
                render::issue(&self.json(&request).await?)
            }
            "pr_diff" => {
                let request = Request::PrDiff {
                    repo,
                    number: number(&args)?,
                    name_only: args.name_only,
                };
                let diff = self.run(&request.args()).await?;
                if diff.trim().is_empty() {
                    "The pull request changes nothing.".to_owned()
                } else {
                    diff
                }
            }
            "pr_checkout" => {
                if !self.checkout {
                    return Err(ToolError::Failed(
                        "pr_checkout changes the working directory, which this mode does not allow"
                            .to_owned(),
                    ));
                }
                let number = number(&args)?;
                let request = Request::PrCheckout {
                    repo,
                    number,
                    branch: args.branch.clone(),
                    force: args.force,
                };
                let printed = self.run(&request.args()).await?;
                let branch = self
                    .run_program("git", &["branch".into(), "--show-current".into()])
                    .await
                    .unwrap_or_default();
                render::checkout(number, branch.trim(), &printed)
            }
            "search" => {
                let query = args.query.as_deref().map(str::trim).unwrap_or_default();
                if query.is_empty() {
                    return Err(ToolError::InvalidArguments(
                        "search needs `query`".to_owned(),
                    ));
                }
                let kind = SearchKind::named(args.kind.as_deref().unwrap_or("issues"))?;
                let request = Request::Search {
                    repo,
                    kind,
                    query: query.to_owned(),
                    limit: args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
                };
                render::search(kind, query, &self.json(&request).await?)
            }
            "run_watch" => self.watch(&args).await?,
            other => {
                return Err(ToolError::InvalidArguments(format!(
                    "action is {other}; it takes pr_view, pr_diff, pr_checkout, issue_view, search or run_watch"
                )));
            }
        };
        Ok(ToolOutput::new(cut(text)))
    }
}

impl GitHub {
    /// Waits for the run `args` names, or the latest run of the current branch, to finish, and reports its jobs and the log tails of the failed ones.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when `gh` fails or there is no run to watch.
    async fn watch(&self, args: &Args) -> Result<String, ToolError> {
        let repo = args.repo.clone();
        let run = match args.run {
            Some(run) => run,
            None => {
                let branch = self
                    .run_program("git", &["branch".into(), "--show-current".into()])
                    .await?;
                let branch = branch.trim();
                if branch.is_empty() {
                    return Err(ToolError::Failed(
                        "no branch is checked out; give `run`".to_owned(),
                    ));
                }
                let request = Request::LatestRun {
                    repo: repo.clone(),
                    branch: branch.to_owned(),
                };
                render::latest_run(&self.json(&request).await?).ok_or_else(|| {
                    ToolError::Failed(format!("branch {branch} has no Actions runs"))
                })?
            }
        };
        let limit = Duration::from_secs(args.timeout.unwrap_or(DEFAULT_WATCH).clamp(1, MAX_WATCH));
        let interval = Duration::from_secs(args.interval.unwrap_or(DEFAULT_INTERVAL).max(3));
        let started = Instant::now();
        let view = Request::RunView {
            repo: repo.clone(),
            run,
        };
        let mut state = self.json(&view).await?;
        while state["status"] != "completed" && started.elapsed() < limit {
            tokio::time::sleep(interval.min(limit.saturating_sub(started.elapsed()))).await;
            state = self.json(&view).await?;
        }
        let mut text = render::run(&state);
        if state["status"] != "completed" {
            text.push_str(&format!(
                "\nStill running after {} seconds; call run_watch again to keep waiting.",
                started.elapsed().as_secs()
            ));
            return Ok(text);
        }
        let tail = args.tail.unwrap_or(DEFAULT_TAIL).max(1);
        for job in render::failed_jobs(&state) {
            let request = Request::JobLog {
                repo: repo.clone(),
                run,
                job: job.id,
            };
            let log = match self.run(&request.args()).await {
                Ok(log) => render::log_tail(&log, tail),
                Err(error) => format!("(log not available: {error})"),
            };
            text.push_str(&format!("\n\nLog of failed job {}:\n{log}", job.name));
        }
        Ok(text)
    }

    /// Runs `request` and reads what it prints as JSON.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when `gh` fails or prints something that is not JSON.
    async fn json(&self, request: &Request) -> Result<Value, ToolError> {
        let printed = self.run(&request.args()).await?;
        serde_json::from_str(&printed).map_err(|error| {
            ToolError::Failed(format!("gh printed something that is not JSON: {error}"))
        })
    }

    /// Runs `gh` with `args` in the working directory and returns what it printed.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when `gh` is missing, not signed in, takes too long or fails.
    async fn run(&self, args: &[String]) -> Result<String, ToolError> {
        self.run_program("gh", args).await
    }

    /// Runs `program` with `args` in the working directory, without a terminal or prompts, and returns what it printed.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the program is missing, takes too long or fails; for `gh`, the message says how to install it or sign in.
    async fn run_program(&self, program: &str, args: &[String]) -> Result<String, ToolError> {
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .current_dir(&self.root)
            .env("GH_PROMPT_DISABLED", "1")
            .env("GH_NO_UPDATE_NOTIFIER", "1")
            .env("NO_COLOR", "1")
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let output = match tokio::time::timeout(COMMAND_TIMEOUT, command.output()).await {
            Err(_) => {
                return Err(ToolError::Failed(format!(
                    "{program} {} took longer than {} seconds",
                    args.join(" "),
                    COMMAND_TIMEOUT.as_secs()
                )));
            }
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ToolError::Failed(missing(program)));
            }
            Ok(Err(error)) => {
                return Err(ToolError::Failed(format!("cannot run {program}: {error}")));
            }
            Ok(Ok(output)) => output,
        };
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(ToolError::Failed(command::failure(program, args, &stderr)))
    }
}

/// What to tell the model when `program` is not installed.
fn missing(program: &str) -> String {
    if program == "gh" {
        "the GitHub command line tool `gh` is not installed; install it from https://cli.github.com and sign in with `gh auth login`".to_owned()
    } else {
        format!("{program} is not installed")
    }
}

/// Cuts `text` to [`MAX_TEXT`] characters, saying how much was left out.
fn cut(text: String) -> String {
    let total = text.chars().count();
    if total <= MAX_TEXT {
        return text;
    }
    let mut kept: String = text.chars().take(MAX_TEXT).collect();
    kept.push_str(&format!(
        "\n[cut: {} of {total} characters left out; narrow the request, such as pr_diff with name_only]",
        total - MAX_TEXT
    ));
    kept
}
