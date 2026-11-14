//! The `gh` command lines each action runs, and how their failures read.

use crate::ToolError;

/// The fields `pr_view` asks for.
const PR_FIELDS: &str = "number,title,state,isDraft,author,url,baseRefName,headRefName,body,\
additions,deletions,changedFiles,mergeable,reviewDecision,files,reviews,comments,statusCheckRollup";

/// The fields `issue_view` asks for.
const ISSUE_FIELDS: &str = "number,title,state,author,url,createdAt,body,labels,assignees,comments";

/// The fields a search of issues or pull requests asks for.
const ISSUE_SEARCH_FIELDS: &str = "number,title,state,repository,url,updatedAt,author";

/// The fields a code search asks for.
const CODE_SEARCH_FIELDS: &str = "path,repository,url,textMatches";

/// The fields of an Actions run that `run_watch` reads.
const RUN_FIELDS: &str =
    "databaseId,name,displayTitle,workflowName,headBranch,event,status,conclusion,url,jobs";

/// What a search looks through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    /// Issues.
    Issues,
    /// Pull requests.
    Prs,
    /// Code.
    Code,
}

impl SearchKind {
    /// The kind called `name`: `issues`, `prs` or `code`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for any other name.
    pub fn named(name: &str) -> Result<Self, ToolError> {
        match name.trim() {
            "issues" | "issue" => Ok(Self::Issues),
            "prs" | "pr" | "pulls" => Ok(Self::Prs),
            "code" => Ok(Self::Code),
            other => Err(ToolError::InvalidArguments(format!(
                "kind is {other}; it takes issues, prs or code"
            ))),
        }
    }
}

/// One `gh` command the tool runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// A pull request's details, as JSON.
    PrView {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The pull request number.
        number: u64,
    },
    /// A pull request's diff, or its changed files.
    PrDiff {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The pull request number.
        number: u64,
        /// Whether only the changed files' names are wanted.
        name_only: bool,
    },
    /// Checks a pull request's branch out.
    PrCheckout {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The pull request number.
        number: u64,
        /// The local branch name.
        branch: Option<String>,
        /// Whether an existing local branch is reset.
        force: bool,
    },
    /// An issue's details, as JSON.
    IssueView {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The issue number.
        number: u64,
    },
    /// A search, as JSON.
    Search {
        /// The repository to search in, or all of GitHub.
        repo: Option<String>,
        /// What to search.
        kind: SearchKind,
        /// The search text.
        query: String,
        /// How many results.
        limit: u32,
    },
    /// The latest Actions run of a branch, as JSON.
    LatestRun {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The branch.
        branch: String,
    },
    /// An Actions run with its jobs, as JSON.
    RunView {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The run id.
        run: u64,
    },
    /// The log of the failed steps of one job.
    JobLog {
        /// The repository as `owner/name`, or the working directory's.
        repo: Option<String>,
        /// The run id.
        run: u64,
        /// The job id.
        job: u64,
    },
}

impl Request {
    /// The arguments `gh` runs with for this request.
    pub fn args(&self) -> Vec<String> {
        let mut args: Vec<String> = Vec::new();
        let repo = match self {
            Self::PrView { repo, number } => {
                push(
                    &mut args,
                    &["pr", "view", &number.to_string(), "--json", PR_FIELDS],
                );
                repo
            }
            Self::PrDiff {
                repo,
                number,
                name_only,
            } => {
                push(
                    &mut args,
                    &["pr", "diff", &number.to_string(), "--color", "never"],
                );
                if *name_only {
                    args.push("--name-only".into());
                }
                repo
            }
            Self::PrCheckout {
                repo,
                number,
                branch,
                force,
            } => {
                push(&mut args, &["pr", "checkout", &number.to_string()]);
                if let Some(branch) = branch {
                    push(&mut args, &["--branch", branch]);
                }
                if *force {
                    args.push("--force".into());
                }
                repo
            }
            Self::IssueView { repo, number } => {
                push(
                    &mut args,
                    &["issue", "view", &number.to_string(), "--json", ISSUE_FIELDS],
                );
                repo
            }
            Self::Search {
                repo,
                kind,
                query,
                limit,
            } => {
                let (noun, fields) = match kind {
                    SearchKind::Issues => ("issues", ISSUE_SEARCH_FIELDS),
                    SearchKind::Prs => ("prs", ISSUE_SEARCH_FIELDS),
                    SearchKind::Code => ("code", CODE_SEARCH_FIELDS),
                };
                push(
                    &mut args,
                    &[
                        "search",
                        noun,
                        "--limit",
                        &limit.to_string(),
                        "--json",
                        fields,
                    ],
                );
                if let Some(repo) = repo {
                    push(&mut args, &["--repo", repo]);
                }
                push(&mut args, &["--", query]);
                return args;
            }
            Self::LatestRun { repo, branch } => {
                push(
                    &mut args,
                    &[
                        "run",
                        "list",
                        "--branch",
                        branch,
                        "--limit",
                        "1",
                        "--json",
                        "databaseId,status",
                    ],
                );
                repo
            }
            Self::RunView { repo, run } => {
                push(
                    &mut args,
                    &["run", "view", &run.to_string(), "--json", RUN_FIELDS],
                );
                repo
            }
            Self::JobLog { repo, run, job } => {
                push(
                    &mut args,
                    &[
                        "run",
                        "view",
                        &run.to_string(),
                        "--job",
                        &job.to_string(),
                        "--log-failed",
                    ],
                );
                repo
            }
        };
        if let Some(repo) = repo {
            push(&mut args, &["--repo", repo]);
        }
        args
    }
}

/// Appends `items` to `args`.
fn push(args: &mut Vec<String>, items: &[&str]) {
    args.extend(items.iter().map(|item| (*item).to_owned()));
}

/// What to tell the model when `program` run with `args` failed and wrote `stderr`, saying how to sign in when that is the trouble.
pub fn failure(program: &str, args: &[String], stderr: &str) -> String {
    let stderr = stderr.trim();
    let lower = stderr.to_lowercase();
    if program == "gh"
        && (lower.contains("gh auth login")
            || lower.contains("not logged in")
            || lower.contains("authentication required")
            || lower.contains("bad credentials")
            || lower.contains("http 401"))
    {
        return format!(
            "gh is not signed in to GitHub; run `gh auth login`, or set GH_TOKEN. gh said: {stderr}"
        );
    }
    let said = if stderr.is_empty() {
        "nothing".to_owned()
    } else {
        stderr.to_owned()
    };
    format!("{program} {} failed: {said}", args.join(" "))
}
