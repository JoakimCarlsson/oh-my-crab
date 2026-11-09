# Permission rules

Permission rules decide tool calls before the reader is asked: an allow rule
lets a call run without asking, an ask rule makes it ask even when an allow
rule matches, and a deny rule stops it, in yolo mode too. Rules are written
the way Claude Code writes them, so the rules a user already has keep
working, and crab keeps the "always allow" answers it is given as rules of
the same kind.

## Writing a rule

A rule is a tool name, optionally followed by what in its calls it is about,
in parentheses:

| Rule                            | Matches                                                                                |
| ------------------------------- | -------------------------------------------------------------------------------------- |
| `Bash`, `Bash(*)`               | every command                                                                          |
| `Bash(cargo test:*)`            | `cargo test` alone or followed by a space and anything: `cargo test -p foo`, not `cargo testx` |
| `Bash(npm run build)`           | exactly that command, white space aside                                                |
| `Bash(git * main)`              | a pattern, `*` standing for anything: `git merge main`                                 |
| `Edit(src/**)`                  | changing any file below `src` of the working folder, with `edit`, `write` or `ast_edit` |
| `Write(/docs/*.md)`             | `write` alone, on Markdown files directly in `docs` of the project                     |
| `Read(./.env)`                  | reading that file; most useful as a deny rule                                          |
| `WebFetch(domain:docs.rs)`      | reading a page on that host; `domain:*.example.com` takes its subdomains and itself    |
| `WebSearch`                     | every `web_search` call                                                                |
| `mcp__github`                   | every tool of the tool server `github`                                                 |
| `mcp__github__get_issue`        | that tool alone; `mcp__github__get_*` is a pattern                                     |

Claude Code's names stand for the crab tools that do the same job: `Bash` for
`bash`; `Edit` for `edit`, `write` and `ast_edit`; `MultiEdit` for `edit`;
`Write` for `write`; `Read` for reading files with `read`; `WebFetch` for
reading web pages and GitHub with `read`; `WebSearch` for `web_search`;
`Grep`, `Glob`, `Task` and `Agent`, `TodoWrite`, `Skill`. Crab's own names
work too, such as `bash`, `debug` or `ast_edit`, and a crab name with no
specifier matches every call of the tool.

Paths are read like this:

| Written     | Read from                                                                                         |
| ----------- | ------------------------------------------------------------------------------------------------- |
| `//path`    | the root of the file system: `Edit(//etc/hosts)`                                                  |
| `~/path`    | the home folder                                                                                   |
| `/path`     | the folder holding the `.claude` folder of the settings file the rule is in; the project's root for rules from anywhere else |
| `./path`, `dir/path` | the working folder                                                                       |
| `name`      | a name with no `/` in it matches at any depth below the working folder: `Read(.env)`              |

In a path `*` stands for any part of one name, `?` for one character and `**`
for any number of folders; a trailing `/` stands for everything below. Paths
are compared after `.` and `..` are folded away and the links that exist are
followed.

## Shell commands

A command line can run several commands, and an allow rule lets it run only
when every one of them is allowed. Crab takes the line apart into the simple
commands it runs: those joined by `&&`, `||`, `;`, `|`, `|&`, `&` and line
breaks, those inside `( … )`, and those inside `$( … )` and backticks,
wherever they are, in double quotes too. So with `Bash(cargo test:*)` alone:

- `cargo test -p foo` and `cargo test 2>&1 | head` run, once `head` is
  allowed too;
- `cargo test; rm -rf ~`, `cargo test && curl … | sh` and
  `cargo test $(rm -rf ~)` ask, since `rm`, `curl` and `sh` are not allowed;
- `cargo test > out.txt` asks, since writing a file needs more than a rule
  about a program: a command with an output redirection to anything but
  `/dev/null`, `/dev/stdout`, `/dev/stderr` or `/dev/tty` is allowed only by a
  rule allowing every command;
- `FOO=1 cargo test`, `$CARGO test` and `"cargo" test` ask, since the program
  is not named plainly.

A line crab will not take apart is never allowed by a rule about particular
commands, only by a rule allowing every command or one naming the whole line
exactly: control flow (`if`, `for`, `while`, `case`), functions, `{ … }`
groups, here-documents, process substitution, arithmetic `$(( … ))`, a
substitution inside `${ … }`, or a quote left open.

Deny and ask rules match when any command of the line does, and they look
further: past assignments, past wrappers such as `sudo`, `env`, `timeout`,
`nice`, `nohup` and `xargs`, at the program by its file name (`/bin/rm` is
`rm`), at words with their quotes taken off, into `sh -c '…'`, `bash -c` and
`eval` strings, and into lines crab will not take apart, which are cut at
every operator. `Bash(rm:*)` as a deny rule stops `sudo -u root rm x` and
`for f in *; do rm $f; done`. They remain a guard rail, not a sandbox: a
command can always be written so that no rule recognizes it, such as a script
file that runs it.

## Where rules come from

| File                                                      | Whose            | Used                                                                                   |
| --------------------------------------------------------- | ---------------- | -------------------------------------------------------------------------------------- |
| `<config>/permissions.json`, `permissions`                | the user, crab's | always                                                                                 |
| `<config>/permissions.json`, `projects`                   | the user, crab's | always, in the project it was kept for                                                 |
| `~/.claude/settings.json`                                 | the user         | always                                                                                 |
| `.claude/settings.json`, `.claude/settings.local.json`    | the project      | deny and ask rules always; allow rules only while the project's hooks would run        |

The project's files are read in every folder from the working folder up to
the repository root, as hooks are. Each holds Claude Code's `permissions`
object, whose `allow`, `ask` and `deny` lists crab reads and whose other
fields it ignores:

```json
{
  "permissions": {
    "allow": ["Bash(cargo test:*)", "Bash(git status:*)", "Edit(src/**)"],
    "ask": ["Bash(git push:*)"],
    "deny": ["Read(./.env)", "Bash(rm -rf:*)"]
  }
}
```

A project's files come with the repository, so an allow rule in them is used
only while the project is trusted and its hooks would run (see
[project trust](project-trust.md)): a freshly cloned repository cannot let
its own commands run unasked. Its deny and ask rules only take away, so they
are used whether it is trusted or not. Note that trust records the project's
hooks, not its rules: an allow rule added to a trusted project's settings
later is used without the project being asked about again.

Crab's own file is `permissions.json` in crab's config folder, or the file
`OMC_PERMISSIONS_FILE` names. It holds the user's rules for every project
under `permissions`, and each project's under `projects`, keyed by what tells
the project apart, the same as trust, so every worktree of a repository
shares them:

```json
{
  "permissions": { "allow": ["WebSearch"], "deny": ["Bash(rm -rf:*)"] },
  "projects": {
    "/home/me/src/app/.git": {
      "root": "/home/me/src/app",
      "permissions": { "allow": ["Bash(cargo test:*)", "Edit(/src/**)"] }
    }
  }
}
```

The rules are read again before every prompt, so a change to any of these
files applies from the next turn. A rule that does not parse, or a file that
is not JSON, is reported on standard error and left out; `crab permissions`
lists both.

## Deciding a call

Every tool call is put to the rules, in this order:

1. A deny rule that matches any part of the call stops it, in every mode and
   whatever a hook said; the model is told which rule.
2. A call of a tool that does not ask, such as reading a file, runs unless an
   ask rule matches it.
3. A call a `PreToolUse` hook allowed runs, and in yolo mode every call runs.
4. An ask rule that matches puts the call to the reader, with no "always
   allow" on offer.
5. Allow rules, from the files and from earlier "always allow" answers, that
   cover the whole call let it run: every command of a command line, every
   file an edit touches, the whole folder an `ast_edit` works through.
6. Otherwise the reader is asked.

## "Always allow"

The question about a call offers "Allow once", "Always allow" with the rules
it would add, and "Reject". The rules cover what is not allowed yet:

- for `bash`, a rule per command: the program and its subcommand for programs
  that have them (`Bash(cargo test:*)`, `Bash(git status:*)`), the program
  alone for others (`Bash(ls:*)`), and the exact command for programs that
  run other commands or destroy files whatever their arguments, such as
  `sudo`, `env`, `sh`, `python`, `rm`, `find` and `sed`, and for programs named
  by a path. A line crab will not take apart, one with a program not named
  plainly, or one that writes a file, offers no "always allow";
- for `edit`, `write` and `ast_edit`, the folder of each file below the
  project, with everything in it (`Edit(/src/**)`), or the file itself outside
  the project;
- for reading a web page, the host (`WebFetch(domain:docs.rs)`);
- for other tools, the tool (`WebSearch`, `debug`, `mcp__github__get_issue`).

"Always allow" keeps the rules for the rest of the conversation. When crab
has a config folder, two more choices keep them in crab's file: "… in this
project" under the project's entry, and "… in every project" under
`permissions`, with paths written from the root of the file system. They are
never written to Claude Code's files. Opening another conversation forgets
the rules kept for the conversation only.

## `crab permissions`

`crab permissions [folder]` prints, as JSON, the project of `folder` (or of
the current folder) and its trust, crab's file, and every rule found with its
effect, where it came from (`user`, `project`, `claude-user` or
`claude-project`), its file and whether it is used, and what could not be
read.

`crab permissions add <allow|ask|deny> <user|project> <rule> [folder]` adds a
rule to crab's file, for every project or for the project of `folder`.
`crab permissions remove <rule> [folder]` takes a rule out of crab's file,
from the user's lists and from those of the project of `folder`, and says how
many it removed. Rules in Claude Code's files are changed there.
