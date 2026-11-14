//! Turns what `gh` prints as JSON into text for the model.

use serde_json::Value;

use super::command::SearchKind;

/// A job of an Actions run that failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    /// The job id.
    pub id: u64,
    /// The job's name.
    pub name: String,
}

/// The text of `value`, or nothing when it is not a string.
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// The login of the author `value`, or `ghost` when there is none.
fn login(value: &Value) -> &str {
    value["login"].as_str().unwrap_or("ghost")
}

/// The entries of the array `value`, or none when it is not an array.
fn entries(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Appends `body`, trimmed, under a blank line, or a note that it is empty.
fn push_body(out: &mut String, body: &str) {
    let body = body.trim();
    out.push('\n');
    if body.is_empty() {
        out.push_str("\n(no description)\n");
    } else {
        out.push('\n');
        out.push_str(body);
        out.push('\n');
    }
}

/// Appends the comments `value`, each with its author, under a heading.
fn push_comments(out: &mut String, value: &Value) {
    let comments = entries(value);
    if comments.is_empty() {
        return;
    }
    out.push_str(&format!("\nComments ({}):\n", comments.len()));
    for comment in comments {
        out.push_str(&format!(
            "\n{} ({}):\n{}\n",
            login(&comment["author"]),
            text(&comment["createdAt"]),
            text(&comment["body"]).trim()
        ));
    }
}

/// How a check of a pull request stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    /// It passed, or was skipped.
    Passed,
    /// It failed.
    Failed,
    /// It has not finished.
    Pending,
}

/// How the check `value`, a check run or a commit status, stands.
pub fn check(value: &Value) -> Check {
    let state = value["conclusion"]
        .as_str()
        .filter(|state| !state.is_empty())
        .or_else(|| value["state"].as_str())
        .unwrap_or_default()
        .to_uppercase();
    match state.as_str() {
        "SUCCESS" | "NEUTRAL" | "SKIPPED" => Check::Passed,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => {
            Check::Failed
        }
        _ => Check::Pending,
    }
}

/// The name of the check `value`.
fn check_name(value: &Value) -> &str {
    value["name"]
        .as_str()
        .or_else(|| value["context"].as_str())
        .unwrap_or("?")
}

/// The pull request `value`, as `gh pr view --json` prints it, as text.
pub fn pull_request(value: &Value) -> String {
    let mut out = format!("#{} {}\n", value["number"], text(&value["title"]));
    let draft = if value["isDraft"] == true {
        ", draft"
    } else {
        ""
    };
    out.push_str(&format!(
        "state: {}{draft} · author: {} · {} <- {}\n{}\n",
        text(&value["state"]),
        login(&value["author"]),
        text(&value["baseRefName"]),
        text(&value["headRefName"]),
        text(&value["url"])
    ));
    out.push_str(&format!(
        "+{} -{} in {} files",
        value["additions"], value["deletions"], value["changedFiles"]
    ));
    for (label, field) in [("review", "reviewDecision"), ("mergeable", "mergeable")] {
        let state = text(&value[field]);
        if !state.is_empty() {
            out.push_str(&format!(" · {label}: {state}"));
        }
    }
    out.push('\n');
    let checks = entries(&value["statusCheckRollup"]);
    if !checks.is_empty() {
        let count = |want: Check| checks.iter().filter(|c| check(c) == want).count();
        out.push_str(&format!(
            "checks: {} passed, {} failed, {} pending",
            count(Check::Passed),
            count(Check::Failed),
            count(Check::Pending)
        ));
        let failed: Vec<&str> = checks
            .iter()
            .filter(|c| check(c) == Check::Failed)
            .map(check_name)
            .collect();
        if !failed.is_empty() {
            out.push_str(&format!(" (failed: {})", failed.join(", ")));
        }
        out.push('\n');
    }
    push_body(&mut out, text(&value["body"]));
    let files = entries(&value["files"]);
    if !files.is_empty() {
        out.push_str(&format!("\nFiles ({}):\n", files.len()));
        for file in files {
            out.push_str(&format!(
                "  {} +{} -{}\n",
                text(&file["path"]),
                file["additions"],
                file["deletions"]
            ));
        }
    }
    let reviews = entries(&value["reviews"]);
    if !reviews.is_empty() {
        out.push_str(&format!("\nReviews ({}):\n", reviews.len()));
        for review in reviews {
            let body = text(&review["body"]).trim();
            out.push_str(&format!(
                "\n{} {}{}{}\n",
                login(&review["author"]),
                text(&review["state"]),
                if body.is_empty() { "" } else { ":\n" },
                body
            ));
        }
    }
    push_comments(&mut out, &value["comments"]);
    out
}

/// The issue `value`, as `gh issue view --json` prints it, as text.
pub fn issue(value: &Value) -> String {
    let mut out = format!("#{} {}\n", value["number"], text(&value["title"]));
    out.push_str(&format!(
        "state: {} · author: {} · opened {}\n{}\n",
        text(&value["state"]),
        login(&value["author"]),
        text(&value["createdAt"]),
        text(&value["url"])
    ));
    let labels: Vec<&str> = entries(&value["labels"])
        .iter()
        .map(|label| text(&label["name"]))
        .collect();
    if !labels.is_empty() {
        out.push_str(&format!("labels: {}\n", labels.join(", ")));
    }
    let assignees: Vec<&str> = entries(&value["assignees"]).iter().map(login).collect();
    if !assignees.is_empty() {
        out.push_str(&format!("assignees: {}\n", assignees.join(", ")));
    }
    push_body(&mut out, text(&value["body"]));
    push_comments(&mut out, &value["comments"]);
    out
}

/// What checking out pull request `number` onto `branch` did, with what `gh` printed.
pub fn checkout(number: u64, branch: &str, printed: &str) -> String {
    let mut out = if branch.is_empty() {
        format!("Checked out pull request #{number}.")
    } else {
        format!("Checked out pull request #{number} on branch {branch}.")
    };
    let printed = printed.trim();
    if !printed.is_empty() {
        out.push('\n');
        out.push_str(printed);
    }
    out
}

/// The results `value` of a search of `kind` for `query`, one per line.
pub fn search(kind: SearchKind, query: &str, value: &Value) -> String {
    let results = entries(value);
    if results.is_empty() {
        return format!("No results for {query}.");
    }
    let mut out = format!("{} results for {query}:\n", results.len());
    for result in results {
        let repo = text(&result["repository"]["nameWithOwner"]);
        match kind {
            SearchKind::Code => {
                out.push_str(&format!(
                    "\n{repo}:{}\n{}\n",
                    text(&result["path"]),
                    text(&result["url"])
                ));
                for found in entries(&result["textMatches"]) {
                    for line in text(&found["fragment"])
                        .lines()
                        .filter(|l| !l.trim().is_empty())
                    {
                        out.push_str(&format!("  {}\n", line.trim_end()));
                    }
                }
            }
            SearchKind::Issues | SearchKind::Prs => {
                out.push_str(&format!(
                    "{repo}#{} [{}] {} · {} · updated {}\n  {}\n",
                    result["number"],
                    text(&result["state"]),
                    text(&result["title"]),
                    login(&result["author"]),
                    text(&result["updatedAt"]),
                    text(&result["url"])
                ));
            }
        }
    }
    out
}

/// The id of the first run of `value`, as `gh run list --json` prints it.
pub fn latest_run(value: &Value) -> Option<u64> {
    entries(value).first()?["databaseId"].as_u64()
}

/// The Actions run `value`, as `gh run view --json` prints it, with each job's outcome.
pub fn run(value: &Value) -> String {
    let outcome = match text(&value["conclusion"]) {
        "" => text(&value["status"]).to_owned(),
        conclusion => conclusion.to_owned(),
    };
    let title = match text(&value["displayTitle"]) {
        "" => text(&value["name"]),
        title => title,
    };
    let mut out = format!(
        "Run {} of {}: {title}\nbranch {} · {} · {outcome}\n{}\n",
        value["databaseId"],
        text(&value["workflowName"]),
        text(&value["headBranch"]),
        text(&value["event"]),
        text(&value["url"])
    );
    let jobs = entries(&value["jobs"]);
    if !jobs.is_empty() {
        out.push_str("\nJobs:\n");
        for job in jobs {
            let state = match text(&job["conclusion"]) {
                "" => text(&job["status"]),
                conclusion => conclusion,
            };
            out.push_str(&format!("  {state}: {}", text(&job["name"])));
            let failed: Vec<&str> = entries(&job["steps"])
                .iter()
                .filter(|step| step["conclusion"] == "failure")
                .map(|step| text(&step["name"]))
                .collect();
            if !failed.is_empty() {
                out.push_str(&format!(" (failed step: {})", failed.join(", ")));
            }
            out.push('\n');
        }
    }
    out
}

/// The jobs of the run `value` that failed, timed out or were cancelled.
pub fn failed_jobs(value: &Value) -> Vec<Job> {
    entries(&value["jobs"])
        .iter()
        .filter(|job| {
            matches!(
                text(&job["conclusion"]),
                "failure" | "timed_out" | "cancelled" | "startup_failure"
            )
        })
        .filter_map(|job| {
            Some(Job {
                id: job["databaseId"].as_u64()?,
                name: text(&job["name"]).to_owned(),
            })
        })
        .collect()
}

/// The last `lines` lines of the job log `log`, each without the job and step names and the time stamp `gh` puts before it.
pub fn log_tail(log: &str, lines: usize) -> String {
    let all: Vec<&str> = log.lines().collect();
    let start = all.len().saturating_sub(lines);
    let mut out = String::new();
    if start > 0 {
        out.push_str(&format!("[{start} earlier lines left out]\n"));
    }
    for line in &all[start..] {
        out.push_str(strip_prefix(line));
        out.push('\n');
    }
    if all.is_empty() {
        out.push_str("(empty log)\n");
    }
    out.trim_end().to_owned()
}

/// `line` of a job log without its job name, step name and time stamp, when it has them.
fn strip_prefix(line: &str) -> &str {
    let mut fields = line.splitn(3, '\t');
    let (Some(_job), Some(_step), Some(rest)) = (fields.next(), fields.next(), fields.next())
    else {
        return line;
    };
    match rest.split_once(' ') {
        Some((stamp, message)) if looks_like_stamp(stamp) => message,
        _ => rest,
    }
}

/// Whether `word` looks like an ISO 8601 time stamp, such as `2024-05-01T12:00:00.1234567Z`.
fn looks_like_stamp(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() >= 20
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[10] == b'T'
        && word.ends_with('Z')
}
