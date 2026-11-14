Work with GitHub through the `gh` command line tool, signed in as the user. Pick one `action`:
- `pr_view`: a pull request's state, description, checks, changed files, reviews and comments. Needs `number`.
- `pr_diff`: a pull request's unified diff; `name_only` lists the changed files instead. Needs `number`.
- `pr_checkout`: checks a pull request's branch out in the working directory; `branch` names the local branch, `force` resets an existing one. Needs `number`.
- `issue_view`: an issue with its labels, assignees and comments. Needs `number`.
- `search`: searches GitHub for `kind` `issues`, `prs` or `code` matching `query`, at most `limit` results (10 by default, 50 at most).
- `run_watch`: waits for a GitHub Actions run to finish, then reports each job and the log tail of the failed ones. `run` is the run's id; without it, the latest run of the current branch. `timeout` is how many seconds to wait at most (600 by default), `interval` how often to look (10 by default), `tail` how many log lines to show per failed job (60 by default).
`repo` names the repository as `owner/name`; without it, the one of the working directory. To only read a pull request or issue, `read` with `pr://123` or `issue://45` also works.
