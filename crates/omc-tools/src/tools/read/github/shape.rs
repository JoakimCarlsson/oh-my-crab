//! Turning GitHub's JSON answers into text a model reads: a pull request with
//! its reviews and comments, an issue with its comments, a repository with its
//! README, and the entries of a directory.

use serde_json::Value;

/// How many items one page of a list asks GitHub for; a list this long may
/// have more.
pub(super) const PAGE: usize = 100;

/// The text of `value` when it is a string, or empty.
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

/// The login of the user in `value`, or `ghost` for a deleted account.
fn login(value: &Value) -> &str {
    value["login"].as_str().unwrap_or("ghost")
}

/// The date and minute of the ISO 8601 time in `value`, such as
/// `2024-05-01 12:30`, or empty when there is none.
fn when(value: &Value) -> String {
    let time = text(value);
    time.get(..16).unwrap_or(time).replace('T', " ")
}

/// The names of the labels in `value`, joined with commas.
fn labels(value: &Value) -> String {
    value
        .as_array()
        .map(|labels| {
            labels
                .iter()
                .filter_map(|label| label["name"].as_str())
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// The body in `value` trimmed, or `fallback` when it is empty.
fn body<'a>(value: &'a Value, fallback: &'a str) -> &'a str {
    let body = text(value).trim();
    if body.is_empty() { fallback } else { body }
}

/// The items of the list `value`, or none when it is not a list.
fn items(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

/// A section heading naming `title` and how many `count` items it has, saying
/// the list may go on when it filled a whole page.
fn heading(title: &str, count: usize) -> String {
    if count >= PAGE {
        format!("## {title} (first {count})\n")
    } else {
        format!("## {title} ({count})\n")
    }
}

/// The lifecycle of the pull request `pull`: draft, open, merged or closed.
fn pull_state(pull: &Value) -> &'static str {
    if pull["merged"].as_bool() == Some(true) || !pull["merged_at"].is_null() {
        "merged"
    } else if text(&pull["state"]) == "closed" {
        "closed"
    } else if pull["draft"].as_bool() == Some(true) {
        "draft"
    } else {
        "open"
    }
}

/// The parts of a pull request as GitHub gives them.
pub(super) struct Pull<'a> {
    /// The pull request itself.
    pub(super) pull: &'a Value,
    /// Its changed files.
    pub(super) files: &'a Value,
    /// Its reviews.
    pub(super) reviews: &'a Value,
    /// The comments on lines of its diff.
    pub(super) review_comments: &'a Value,
    /// The comments on the conversation.
    pub(super) comments: &'a Value,
}

/// The text of a pull request: what it is, its description, the files it
/// changes, its reviews, the comments on its lines and its conversation, and
/// how to read its diff, which `diff` names.
pub(super) fn pull(parts: &Pull<'_>, diff: &str) -> String {
    let pull = parts.pull;
    let mut out = format!("# {}\n\n", text(&pull["title"]).trim());
    out.push_str(&format!(
        "{} by {} · {} → {}\n",
        pull_state(pull),
        login(&pull["user"]),
        text(&pull["head"]["label"]),
        text(&pull["base"]["label"]),
    ));
    let mut dates = format!("opened {}", when(&pull["created_at"]));
    if !pull["merged_at"].is_null() {
        dates.push_str(&format!(", merged {}", when(&pull["merged_at"])));
    } else if !pull["closed_at"].is_null() {
        dates.push_str(&format!(", closed {}", when(&pull["closed_at"])));
    }
    dates.push_str(&format!(", updated {}", when(&pull["updated_at"])));
    out.push_str(&dates);
    out.push('\n');
    out.push_str(&format!(
        "{} commits, {} files changed, +{} −{}\n",
        pull["commits"].as_u64().unwrap_or(0),
        pull["changed_files"].as_u64().unwrap_or(0),
        pull["additions"].as_u64().unwrap_or(0),
        pull["deletions"].as_u64().unwrap_or(0),
    ));
    let labels = labels(&pull["labels"]);
    if !labels.is_empty() {
        out.push_str(&format!("labels: {labels}\n"));
    }
    out.push_str(&format!("{}\n\n", text(&pull["html_url"])));
    out.push_str("## Description\n\n");
    out.push_str(body(&pull["body"], "(no description)"));
    out.push_str("\n\n");
    let files = items(parts.files);
    out.push_str(&heading("Files changed", files.len()));
    for file in files {
        out.push_str(&format!(
            "{} {} +{} −{}\n",
            status_letter(text(&file["status"])),
            text(&file["filename"]),
            file["additions"].as_u64().unwrap_or(0),
            file["deletions"].as_u64().unwrap_or(0),
        ));
    }
    out.push('\n');
    let reviews: Vec<&Value> = items(parts.reviews)
        .iter()
        .filter(|review| {
            text(&review["state"]) != "COMMENTED" || !text(&review["body"]).trim().is_empty()
        })
        .collect();
    if !reviews.is_empty() {
        out.push_str(&heading("Reviews", reviews.len()));
        for review in reviews {
            out.push_str(&format!(
                "\n### {} · {} · {}\n",
                login(&review["user"]),
                text(&review["state"])
                    .to_ascii_lowercase()
                    .replace('_', " "),
                when(&review["submitted_at"]),
            ));
            let said = text(&review["body"]).trim();
            if !said.is_empty() {
                out.push_str(said);
                out.push('\n');
            }
        }
        out.push('\n');
    }
    let line_comments = items(parts.review_comments);
    if !line_comments.is_empty() {
        out.push_str(&heading("Comments on lines", line_comments.len()));
        for comment in line_comments {
            let line = comment["line"]
                .as_u64()
                .or_else(|| comment["original_line"].as_u64())
                .map(|line| format!(":{line}"))
                .unwrap_or_default();
            out.push_str(&format!(
                "\n### {} on {}{line} · {}\n{}\n",
                login(&comment["user"]),
                text(&comment["path"]),
                when(&comment["created_at"]),
                body(&comment["body"], "(empty)"),
            ));
        }
        out.push('\n');
    }
    out.push_str(&conversation(parts.comments));
    out.push_str(&format!("\nThe diff: read {diff}\n"));
    out
}

/// The one-letter mark of a file's change `status`, as `git` shows it.
fn status_letter(status: &str) -> &'static str {
    match status {
        "added" => "A",
        "removed" => "D",
        "renamed" => "R",
        "copied" => "C",
        _ => "M",
    }
}

/// The comments of a conversation, `comments`, under a heading, each with its
/// author and time.
fn conversation(comments: &Value) -> String {
    let comments = items(comments);
    let mut out = heading("Comments", comments.len());
    for comment in comments {
        out.push_str(&format!(
            "\n### {} · {}\n{}\n",
            login(&comment["user"]),
            when(&comment["created_at"]),
            body(&comment["body"], "(empty)"),
        ));
    }
    out
}

/// The text of the issue `issue` with its `comments`; an issue that is a pull
/// request says how to read it as one, through `as_pull`.
pub(super) fn issue(issue: &Value, comments: &Value, as_pull: &str) -> String {
    let mut out = format!("# {}\n\n", text(&issue["title"]).trim());
    if !issue["pull_request"].is_null() {
        out.push_str(&format!(
            "This issue is a pull request; read {as_pull} for its reviews, files and diff.\n"
        ));
    }
    let reason = text(&issue["state_reason"]);
    let state = if reason.is_empty() || reason == "reopened" {
        text(&issue["state"]).to_owned()
    } else {
        format!("{} ({})", text(&issue["state"]), reason.replace('_', " "))
    };
    out.push_str(&format!(
        "{state} by {} · opened {}, updated {}\n",
        login(&issue["user"]),
        when(&issue["created_at"]),
        when(&issue["updated_at"]),
    ));
    let labels = labels(&issue["labels"]);
    if !labels.is_empty() {
        out.push_str(&format!("labels: {labels}\n"));
    }
    out.push_str(&format!("{}\n\n", text(&issue["html_url"])));
    out.push_str(body(&issue["body"], "(no description)"));
    out.push_str("\n\n");
    out.push_str(&conversation(comments));
    out
}

/// The text of the repository `repo`: what it is, then the entries at its
/// root, `root`, and its README, `readme`, when it has one.
pub(super) fn repo(repo: &Value, root: Option<&Value>, readme: Option<&str>) -> String {
    let mut out = format!("# {}\n\n", text(&repo["full_name"]));
    let description = text(&repo["description"]).trim();
    if !description.is_empty() {
        out.push_str(description);
        out.push_str("\n\n");
    }
    let mut facts = vec![
        format!("{} stars", repo["stargazers_count"].as_u64().unwrap_or(0)),
        format!("{} forks", repo["forks_count"].as_u64().unwrap_or(0)),
        format!(
            "{} open issues and pull requests",
            repo["open_issues_count"].as_u64().unwrap_or(0)
        ),
        format!("default branch {}", text(&repo["default_branch"])),
    ];
    for (key, label) in [("language", "language"), ("homepage", "homepage")] {
        let value = text(&repo[key]).trim();
        if !value.is_empty() {
            facts.push(format!("{label} {value}"));
        }
    }
    let license = text(&repo["license"]["spdx_id"]);
    if !license.is_empty() && license != "NOASSERTION" {
        facts.push(format!("license {license}"));
    }
    if repo["archived"].as_bool() == Some(true) {
        facts.push("archived".to_owned());
    }
    facts.push(format!("last push {}", when(&repo["pushed_at"])));
    out.push_str(&facts.join(" · "));
    out.push('\n');
    let topics: Vec<&str> = items(&repo["topics"])
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !topics.is_empty() {
        out.push_str(&format!("topics: {}\n", topics.join(", ")));
    }
    out.push_str(&format!("{}\n", text(&repo["html_url"])));
    if let Some(root) = root {
        out.push_str("\n## Files\n\n");
        out.push_str(&listing(root));
    }
    if let Some(readme) = readme {
        out.push_str("\n## README\n\n");
        out.push_str(readme.trim_end());
        out.push('\n');
    }
    out
}

/// Whether `value` is the list GitHub gives for a directory's contents.
pub(super) fn is_listing(value: &Value) -> bool {
    value.as_array().is_some_and(|entries| {
        entries
            .iter()
            .all(|entry| entry["type"].is_string() && entry["path"].is_string())
    })
}

/// The entries of a directory listing `entries`, directories first and ending
/// in `/`, files with their sizes.
pub(super) fn listing(entries: &Value) -> String {
    let mut entries: Vec<&Value> = items(entries).iter().collect();
    entries.sort_by_key(|entry| (text(&entry["type"]) != "dir", text(&entry["name"])));
    let mut out = String::new();
    for entry in entries {
        let name = text(&entry["name"]);
        match text(&entry["type"]) {
            "dir" => out.push_str(&format!("{name}/\n")),
            "file" => out.push_str(&format!(
                "{name} ({} bytes)\n",
                entry["size"].as_u64().unwrap_or(0)
            )),
            kind => out.push_str(&format!("{name} ({kind})\n")),
        }
    }
    if out.is_empty() {
        out.push_str("(empty)\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the fixture `json`.
    fn fixture(json: &str) -> Value {
        serde_json::from_str(json).unwrap()
    }

    /// A pull request reads as its summary, description, files, reviews,
    /// comments on lines and conversation, ending with how to get its diff.
    #[test]
    fn shapes_pull_requests() {
        let pull = fixture(include_str!("fixtures/pull.json"));
        let files = fixture(include_str!("fixtures/pull_files.json"));
        let reviews = fixture(include_str!("fixtures/pull_reviews.json"));
        let review_comments = fixture(include_str!("fixtures/pull_review_comments.json"));
        let comments = fixture(include_str!("fixtures/pull_comments.json"));
        let text = pull_text(&pull, &files, &reviews, &review_comments, &comments);
        assert!(
            text.starts_with("# ci: install lz4 instead of liblz4-tool\n\n"),
            "{text}"
        );
        assert!(
            text.contains("merged by manoj-1407 · manoj-1407:ci-install-lz4 → BurntSushi:master\n"),
            "{text}"
        );
        assert!(
            text.contains("opened 2026-10-20 09:58, merged 2026-10-23 13:43"),
            "{text}"
        );
        assert!(
            text.contains("1 commits, 2 files changed, +11 −1\n"),
            "{text}"
        );
        assert!(text.contains("## Description\n\n`ubuntu-latest`"), "{text}");
        assert!(
            text.contains("## Files changed (2)\nM .github/workflows/ci.yml +10 −0\n"),
            "{text}"
        );
        assert!(text.contains("## Reviews (2)\n"), "{text}");
        assert!(
            text.contains("### BurntSushi · changes requested · 2026-10-21 08:00\nPlease pin"),
            "{text}"
        );
        assert!(
            text.contains("### BurntSushi · approved · 2026-10-23 13:40\n"),
            "{text}"
        );
        assert!(
            text.contains("### BurntSushi on ci/ubuntu-install-packages:12 · 2026-10-21 08:05\n"),
            "{text}"
        );
        assert!(text.contains("## Comments (2)\n"), "{text}");
        assert!(
            text.contains("### manoj-1407 · 2026-10-21 05:45\nWith this change"),
            "{text}"
        );
        assert!(
            text.ends_with("\nThe diff: read gh://BurntSushi/ripgrep/pull/3565:diff\n"),
            "{text}"
        );
    }

    /// Shapes the pull request fixtures with the diff named as the tests expect.
    fn pull_text(
        pull: &Value,
        files: &Value,
        reviews: &Value,
        review_comments: &Value,
        comments: &Value,
    ) -> String {
        super::pull(
            &Pull {
                pull,
                files,
                reviews,
                review_comments,
                comments,
            },
            "gh://BurntSushi/ripgrep/pull/3565:diff",
        )
    }

    /// An issue that is a pull request says so, and its comments follow.
    #[test]
    fn shapes_issues() {
        let issue = fixture(include_str!("fixtures/issue_is_pull.json"));
        let comments = fixture(include_str!("fixtures/pull_comments.json"));
        let text = super::issue(&issue, &comments, "pr://3565");
        assert!(text.starts_with("# ci: install lz4"), "{text}");
        assert!(text.contains("is a pull request; read pr://3565"), "{text}");
        assert!(
            text.contains("closed by manoj-1407 · opened 2026-10-20"),
            "{text}"
        );
        assert!(text.contains("## Comments (2)\n"), "{text}");
    }

    /// A repository reads as its facts, root entries and README.
    #[test]
    fn shapes_repositories() {
        let repo = fixture(include_str!("fixtures/repo.json"));
        let root = fixture(include_str!("fixtures/contents.json"));
        let text = super::repo(&repo, Some(&root), Some("# ripgrep\n\nFast search.\n"));
        assert!(text.starts_with("# BurntSushi/ripgrep\n\n"), "{text}");
        assert!(text.contains("default branch master"), "{text}");
        assert!(text.contains("license Unlicense"), "{text}");
        assert!(text.contains("## Files\n\n"), "{text}");
        assert!(
            text.ends_with("## README\n\n# ripgrep\n\nFast search.\n"),
            "{text}"
        );
    }

    /// Listings put directories first and give file sizes.
    #[test]
    fn shapes_listings() {
        let entries = serde_json::json!([
            {"name": "b.rs", "path": "src/b.rs", "type": "file", "size": 10},
            {"name": "a", "path": "src/a", "type": "dir", "size": 0},
            {"name": "link", "path": "src/link", "type": "symlink", "size": 3}
        ]);
        assert!(is_listing(&entries));
        assert!(!is_listing(&serde_json::json!({"type": "file"})));
        assert_eq!(listing(&entries), "a/\nb.rs (10 bytes)\nlink (symlink)\n");
    }
}
