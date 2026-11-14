//! Tests of the `github` tool's command lines and output, without the network.

use serde_json::json;

use super::command::{Request, SearchKind, failure};
use super::render::{self, Check, Job};
use super::{GitHub, check_repo, cut};
use crate::{Concurrency, Sandbox, Tool};

/// Joins the arguments of `request` with spaces, for comparing.
fn line(request: Request) -> String {
    request.args().join(" ")
}

/// Each action builds the `gh` command line it should, with the repository last.
#[test]
fn builds_command_lines() {
    let repo = Some("o/r".to_owned());
    assert!(
        line(Request::PrView {
            repo: repo.clone(),
            number: 7
        })
        .starts_with("pr view 7 --json number,title,")
    );
    assert!(
        line(Request::PrView {
            repo: repo.clone(),
            number: 7
        })
        .ends_with(" --repo o/r")
    );
    assert_eq!(
        line(Request::PrDiff {
            repo: None,
            number: 3,
            name_only: true
        }),
        "pr diff 3 --color never --name-only"
    );
    assert_eq!(
        line(Request::PrCheckout {
            repo: repo.clone(),
            number: 9,
            branch: Some("review".into()),
            force: true
        }),
        "pr checkout 9 --branch review --force --repo o/r"
    );
    assert_eq!(
        line(Request::JobLog {
            repo: None,
            run: 11,
            job: 22
        }),
        "run view 11 --job 22 --log-failed"
    );
    assert_eq!(
        line(Request::LatestRun {
            repo: None,
            branch: "main".into()
        }),
        "run list --branch main --limit 1 --json databaseId,status"
    );
    assert!(
        line(Request::IssueView {
            repo: None,
            number: 4
        })
        .starts_with("issue view 4 --json ")
    );
}

/// A search puts the query after `--`, so a query starting with a dash is not read as a flag.
#[test]
fn search_query_comes_last() {
    let args = Request::Search {
        repo: Some("o/r".into()),
        kind: SearchKind::Code,
        query: "-x lang:rust".into(),
        limit: 5,
    }
    .args();
    assert_eq!(
        args,
        [
            "search",
            "code",
            "--limit",
            "5",
            "--json",
            "path,repository,url,textMatches",
            "--repo",
            "o/r",
            "--",
            "-x lang:rust"
        ]
    );
    assert_eq!(SearchKind::named("prs").unwrap(), SearchKind::Prs);
    assert!(SearchKind::named("wiki").is_err());
}

/// Repositories must be `owner/name`.
#[test]
fn checks_repository_form() {
    assert!(check_repo(None).is_ok());
    assert!(check_repo(Some("owner/name")).is_ok());
    for bad in ["owner", "a/b/c", "/name", "owner/", "--repo/x"] {
        assert!(check_repo(Some(bad)).is_err(), "{bad}");
    }
}

/// Sign-in trouble says how to sign in; other failures quote what `gh` said.
#[test]
fn explains_failures() {
    let args = vec!["pr".to_owned(), "view".to_owned()];
    let message = failure(
        "gh",
        &args,
        "To get started with GitHub CLI, please run:  gh auth login",
    );
    assert!(message.contains("not signed in"), "{message}");
    let message = failure("gh", &args, "no pull requests found\n");
    assert_eq!(message, "gh pr view failed: no pull requests found");
    assert!(super::missing("gh").contains("cli.github.com"));
}

/// A pull request shows its state, checks, files, reviews and comments.
#[test]
fn renders_pull_request() {
    let value = json!({
        "number": 12, "title": "Fix it", "state": "OPEN", "isDraft": true,
        "author": {"login": "ann"}, "url": "https://github.com/o/r/pull/12",
        "baseRefName": "main", "headRefName": "fix", "body": "Does things.",
        "additions": 5, "deletions": 2, "changedFiles": 1,
        "mergeable": "MERGEABLE", "reviewDecision": "",
        "files": [{"path": "src/a.rs", "additions": 5, "deletions": 2}],
        "reviews": [{"author": {"login": "bob"}, "state": "APPROVED", "body": ""}],
        "comments": [{"author": {"login": "cy"}, "createdAt": "2024-01-01", "body": "nice"}],
        "statusCheckRollup": [
            {"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
            {"name": "test", "status": "COMPLETED", "conclusion": "FAILURE"},
            {"context": "ci/other", "state": "PENDING"}
        ]
    });
    let text = render::pull_request(&value);
    assert!(text.starts_with("#12 Fix it\nstate: OPEN, draft · author: ann · main <- fix\n"));
    assert!(text.contains("+5 -2 in 1 files · mergeable: MERGEABLE\n"));
    assert!(text.contains("checks: 1 passed, 1 failed, 1 pending (failed: test)"));
    assert!(text.contains("\nDoes things.\n"));
    assert!(text.contains("  src/a.rs +5 -2"));
    assert!(text.contains("bob APPROVED"));
    assert!(text.contains("cy (2024-01-01):\nnice"));
    assert_eq!(render::check(&json!({"state": "ERROR"})), Check::Failed);
    assert_eq!(
        render::check(&json!({"status": "IN_PROGRESS", "conclusion": ""})),
        Check::Pending
    );
}

/// An issue shows its labels, assignees and body.
#[test]
fn renders_issue() {
    let value = json!({
        "number": 4, "title": "Broken", "state": "OPEN", "author": {"login": "ann"},
        "url": "u", "createdAt": "2024-02-02", "body": "",
        "labels": [{"name": "bug"}], "assignees": [{"login": "bob"}], "comments": []
    });
    let text = render::issue(&value);
    assert!(text.contains("labels: bug\nassignees: bob\n"));
    assert!(text.contains("(no description)"));
}

/// Search results are listed one per item, and code results with their fragments.
#[test]
fn renders_search() {
    let issues = json!([{
        "number": 1, "title": "T", "state": "open", "repository": {"nameWithOwner": "o/r"},
        "url": "u", "updatedAt": "d", "author": {"login": "a"}
    }]);
    assert_eq!(
        render::search(SearchKind::Issues, "q", &issues),
        "1 results for q:\no/r#1 [open] T · a · updated d\n  u\n"
    );
    let code = json!([{
        "path": "src/x.rs", "repository": {"nameWithOwner": "o/r"}, "url": "u",
        "textMatches": [{"fragment": "fn main() {\n\n}"}]
    }]);
    assert!(
        render::search(SearchKind::Code, "q", &code)
            .contains("o/r:src/x.rs\nu\n  fn main() {\n  }\n")
    );
    assert_eq!(
        render::search(SearchKind::Prs, "q", &json!([])),
        "No results for q."
    );
}

/// A run lists its jobs and names the failed ones and their failed steps.
#[test]
fn renders_run_and_failed_jobs() {
    let value = json!({
        "databaseId": 99, "workflowName": "CI", "displayTitle": "Fix", "headBranch": "fix",
        "event": "push", "status": "completed", "conclusion": "failure", "url": "u",
        "jobs": [
            {"databaseId": 1, "name": "lint", "status": "completed", "conclusion": "success", "steps": []},
            {"databaseId": 2, "name": "test", "status": "completed", "conclusion": "failure",
             "steps": [{"name": "cargo test", "conclusion": "failure"}]}
        ]
    });
    let text = render::run(&value);
    assert!(text.starts_with("Run 99 of CI: Fix\nbranch fix · push · failure\n"));
    assert!(text.contains("  success: lint\n  failure: test (failed step: cargo test)\n"));
    assert_eq!(
        render::failed_jobs(&value),
        [Job {
            id: 2,
            name: "test".into()
        }]
    );
    assert_eq!(render::latest_run(&json!([{"databaseId": 5}])), Some(5));
    assert_eq!(render::latest_run(&json!([])), None);
}

/// A log tail keeps the last lines, without the job, step and time stamp before each.
#[test]
fn tails_logs() {
    let log = "test\tcargo test\t2024-05-01T12:00:00.1234567Z one\n\
               test\tcargo test\t2024-05-01T12:00:01.1234567Z two\n\
               plain line";
    assert_eq!(
        render::log_tail(log, 2),
        "[1 earlier lines left out]\ntwo\nplain line"
    );
    assert_eq!(render::log_tail("", 5), "(empty log)");
}

/// Long text is cut with a note.
#[test]
fn cuts_long_text() {
    let long = "x".repeat(super::MAX_TEXT + 10);
    assert!(cut(long).ends_with("narrow the request, such as pr_diff with name_only]"));
    assert_eq!(cut("short".into()), "short");
}

/// Checking out runs alone and is approved apart; the copy that only looks refuses it.
#[tokio::test]
async fn checkout_is_exclusive_and_refused_when_looking() {
    let sandbox = Sandbox::new(crate::testutil::scratch("github-looking")).unwrap();
    let tool = GitHub::new(&sandbox);
    let checkout = json!({"action": "pr_checkout", "number": 1});
    assert_eq!(tool.concurrency(&checkout), Concurrency::Exclusive);
    assert_eq!(
        tool.concurrency(&json!({"action": "pr_view"})),
        Concurrency::Shared
    );
    assert_eq!(
        tool.approval_scope(&checkout).unwrap(),
        "github:pr_checkout"
    );
    assert!(!tool.read_only());
    let looking = tool.looking().unwrap();
    assert!(looking.read_only());
    let error = looking.call(checkout).await.unwrap_err();
    assert!(error.to_string().contains("does not allow"), "{error}");
    let error = tool.call(json!({"action": "pr_view"})).await.unwrap_err();
    assert!(error.to_string().contains("needs `number`"), "{error}");
}
