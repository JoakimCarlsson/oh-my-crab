# oh-my-crab

`crab` is a coding-agent harness with no interface of its own. It runs the
agent loop, the tools and the model providers for one conversation, and talks
to the program that started it with JSON lines on standard input and output.

It is inspired by [oh-my-pi](https://github.com/can1357/oh-my-pi) and is
primarily used by [pandemonium](https://github.com/JoakimCarlsson/pandemonium),
which starts it for each agent session and draws the conversation itself. Any
other front end, such as a terminal UI, can use the same protocol.

## Install

```sh
make install
```

This installs `crab` under `~/.local/bin`. Set `PREFIX` to install elsewhere.

## Usage

```text
crab serve                      hold a conversation over standard input and output
crab tools                      print the tools the agent can have, as a JSON list
crab providers                  print the providers, as a JSON list of id, name and sign-in
crab account <p> [folder]       print who provider <p> is signed in as, or nothing
crab history <id> [folder]      print a saved conversation: title, settings and turns
crab rename <id> <folder> <t>   name a saved conversation nobody has open
crab import <id> <folder>        save a conversation a client kept itself, read as JSON
crab memory [folder]            print the memories a conversation in the folder starts with, as JSON
crab hooks [folder]             print the hooks a conversation in the folder would find and its project's trust, as JSON
crab trust <folder> [on|off]    print whether the folder's project is trusted, or record that it is or is not
```

A conversation opened with an id is saved by `crab`: its messages, its turns,
its title, the settings chosen for it and the list of what can be set, the commands it offers, the context window, its notebook, the files each tool call changed, and the turns that were stopped or failed, which the model never sees but a client still shows. It comes back as it was
when the same id is opened again, so a client does not need to keep a copy. The
agent names an unnamed conversation itself with `set_title`; a name the client
sends with `Rename` always wins.

A prompt sent while a turn is running is not dropped. It is queued, and the
client is told with a `Queued` event in the `Waiting` state, carrying a ticket
that every later event about it repeats. At the next safe point of the running
turn, after its current batch of tool calls and any compaction, and before the
next model request, the waiting prompts are put into the conversation in the
order they came, so the model reads them mid-turn; each is then reported as
`Steered` and saved as a turn of its own under the id the client gave it. When
the model's final answer ends the turn before a prompt could be put in, that
prompt starts the next turn by itself and is reported as `Started`; any others
wait for that turn; when that turn cannot start, the prompt is reported as
`Dropped` after its `Started`. Prompts with files are handled like any other
prompt. When the turn is cancelled, fails or needs signing in again, the
prompts still waiting are never sent: each is reported as `Dropped` so the
client can offer it again, and prompts already put into a cancelled or failed
turn are saved with that turn as stopped or failed. A `/compact` sent while a
turn is running is refused with a `Failed` event. An `Open` sent while a turn
is running cancels that turn as `Cancel` does and drops the prompts waiting for
it before the other conversation is opened; the other commands work as they do
between turns.

The commands and events are defined in `crates/omc-wire`. `crates/omc-client`
is a small blocking client for programs that start `crab` and talk to it.

## Tool calls

Arguments the model gets slightly wrong are put right against the tool's
schema before the tool runs, for built-in tools and tool-server tools alike: a
number or boolean sent as a string, a single value where a list is wanted, an
object or list sent as JSON text, and `null` for an optional argument. When the
arguments still do not fit, the call is turned away with every place that is
wrong and what was expected there.

Long output is not lost. `bash` shows the first 4,000 and the last 26,000 bytes
of each of stdout and stderr, and a tool server's text is cut to its first and
last 10 KB; the whole output is saved to a file the result names, which the
model can read in parts with `read`. The files go under
`oh-my-crab/output/<conversation id>` in the user's cache folder, so they stay
readable when the conversation is opened again. On Unix the folders are made
readable by the user alone (`0700`) and the files too (`0600`). When a
conversation opens, other conversations' folders none of whose files was
written for seven days are removed; the folder of the conversation being
opened is always kept.

`bash` condenses the output of noisy commands before the model reads it: git
(status hints, and push, pull, fetch and clone progress), cargo, npm, pnpm,
yarn and bun, make and ninja, gcc and clang, go, pytest, docker and podman, and
apt and dnf. Build steps, downloads and passing tests are counted on one line
("… 143 Compiling lines"), progress and bookkeeping lines are dropped, colour
codes and progress bars redrawn in place are cleaned up, and a line repeated
straight after itself is counted. Lines that report an error or a warning are
always kept, and every line that is kept is shown byte for byte as printed.
`git log`, `git show` and `git diff` are never condensed, nor are the version
changes `cargo update` and `cargo add` report or compiler include chains. The
command is recognised from the last command of the last pipeline only, past
variable assignments, `sudo`, `env`, `time` and the like: `cd x && cargo build`
is condensed, while `cargo build && ./target/debug/app` and
`cargo test | grep FAILED` are left alone. Whenever condensing leaves a line
out, the raw output is saved whole, however short, and the result ends with a
note such as `[stderr condensed: 412 lines → 38; the whole output is in <path>;
…]`; when it only cleaned up colour codes, redraws or blank lines, the raw
output is shown instead. The rules are TOML files, one per family of tools, in
`crates/omc-tools/src/tools/bash/rules`. Set `OMC_BASH_CONDENSE=0` to show all
output as it is.

`bash` also steers the model to the dedicated tools, turning a command away
only when the tool gives exactly the same result. A command line that is
nothing but `cat`, `less` or `more` of one to four text files, `head` or `tail`
of a number of lines of one text file, or `echo`, `printf` or a `cat`
here-document writing text the shell does not expand over a file (the
delimiter quoted, or no `$`, backtick or backslash in the body) is not run; the
result names the call that does the same (`read` with `path:1-` or a line
range, or `write`). Every word must be literal, every path inside the working
directory, every file text rather than binary, and every option one the tool
reproduces. Everything else runs: searches with `grep` or `rg`, `find`,
`ls -R` and `sed -i` (the dedicated tools skip ignored and binary files, match
only files, or replace exact text, so their results differ), appending with
`>>`, other options, pipelines, lists and commands sent to the background with
`&`. Such a command runs with a one-line tip naming the tool when it starts a
pipeline. Set `OMC_BASH_INTERCEPT=0` to run every command.

## Subagents

The `task` tool hands self-contained assignments to subagents, each running a
loop of its own with the main agent's tools, less the ones that start
subagents or talk to the reader, and less `debug`, whose sessions belong to
the conversation and would be steered by several subagents at once. The kinds come with the harness (`task`,
`scout` and `reviewer`) or from agent definitions in the project's and the
user's agent folders. One call runs one assignment, or up to 16 given as
`tasks`, eight at a time, and their results come back under a heading each; one
failing leaves the others be. With `background` the call returns a handle per
subagent at once and `wait` returns each result as it ends. A call or a
definition can give an `output_schema`: the subagent then hands its result to
`submit_result`, which checks it against the schema, and the main agent gets
the checked JSON instead of prose.

A subagent can work in a git worktree of its own, so that subagents editing at
once never trample each other's files or the main agent's. The call sets
`isolated: true`, for every assignment or for one item of `tasks`, or a
definition sets `isolated: true` in its front matter, which a call can turn off
with `isolated: false`. The worktree is made from the working tree `crab` runs
in, whether that is the repository's own checkout or a linked worktree, such as
the one an editor gives each agent session, and it starts from that tree as it
is: uncommitted changes and new files are copied in, ignored files are not. The
state is written into a commit no branch points at, through an index of its
own so the main agent's index is untouched, and the worktree checks it out on a
detached head, so no branch name is taken. Worktrees are made under
`oh-my-crab/worktrees/<conversation id>` in the user's cache folder, never
inside the working tree, so its status, searches and file watchers never see
them, and they are locked with the reason `oh-my-crab task worktree`, which
tells them apart in `git worktree list --porcelain`. No hook of the repository
runs when one is made. The subagent's tools, `bash` included, are confined to
its worktree; it asks the harness's own language servers, started for the
worktree, when the conversation uses those, and none when the client provides
them, since the client's servers know only the main working tree. Tool-server
tools are shared with the main agent. Submodules are not checked out in a
worktree and changes to them are not handed back; the subagent is told which.

When an isolated subagent ends, what it changed, committed or not, is taken as
a patch against the commit it started from and its worktree is removed. The
patch is applied to the main working tree, uncommitted and without touching the
index, so a client reviewing the session's diff sees it; the subagents of one
call are applied in the order of the list, once all of them ended. A patch that
does not apply cleanly is not applied at all: it is saved in the conversation's
output folder, and the result names the files that conflict and where the
patch is, for the main agent to merge itself. The changes of a subagent that
failed are saved the same way and not applied. Every result ends with a
`# Changes` part listing the files changed or what became of them, and the
`task` call's tool event carries the worktree's folder and the outcome in its
`worktree` field. A worktree whose subagent is stopped is removed at once; one
left behind by a crash is removed when its conversation opens again, and other
conversations' worktrees made more than a day before are removed then too.
Isolation needs `git` and a git working tree; a call asking for it anywhere
else is refused with the reason and starts nothing, rather than running the
subagents unisolated.

## Scripts

The `script` tool lets the model write a short JavaScript program that calls
its other tools, so that results it only needs to filter, count or combine
never enter its context: only what the program prints and returns does. The
program is the body of an async function. It calls a tool with
`await tools.grep({ pattern: "..." })`, with the same arguments as a direct
call, prints with `print` or `console.log`, and returns a value, which comes
back as indented JSON. Most tools resolve to their output text; `grep` and
`glob` resolve to their matches and paths as data. A call that does not
succeed rejects with a `ToolError` the program can catch, and an uncaught error
fails the script with what it printed before. `Promise.all` runs calls at once.
The tool's description lists the tools a script can call with their arguments
as TypeScript-like signatures, made from their schemas in name order, so the
description stays the same from one request to the next.

The program runs in QuickJS, embedded through `rquickjs`, on a thread of its
own. It has no file system, network, process, timer or module access: `tools`,
`print`, `console` and `ToolError` are all it can reach beyond the language
itself. A script runs for at most 60 seconds, or the `timeout` it is given up
to 300, not counting time spent waiting for the reader to approve one of its
calls; it may allocate 64 MiB and make 500 tool calls. A script past its time is
stopped even in a loop that calls nothing, and so is a script whose turn is
cancelled, along with the calls it is waiting on. Output is cut like a tool
server's, to its first and last 10 KB, with the whole saved to a file the
model can `read`.

Every call a script makes goes the way the model's own calls go: its arguments
are put right against the tool's schema or turned away, the hooks see it before
and after, under the tool's own name, and a call that needs approval asks the
reader, one call at a time, with the summary ending in `(called by script)`;
an "always allow" answer covers direct calls and scripted ones alike. A hook's
note after a call reaches the script in the call's text. Each call is reported
as a child of the `script` call: its tool events carry the script call's id as
`parent`, `script` as `agent`, and an id made of the script call's id and the
call's number, as the work of a subagent is reported under its `task` call.

Scripts never get `script` itself, `task`, `ask`, `exit_plan_mode`, `todo`,
`context_notes`, `new_context`, `checkpoint`, `rewind`, `wait`, `set_title`,
`submit_result`, `recall`, `skill`, `remember` or `memory_edit`: the tools that
start subagents, talk to the reader, steer the conversation or only make sense
to the model. A denied tool is not offered to scripts either. In plan mode a
script can only call the tools that only look. Subagents whose kind does not
list its tools get `script` too, calling their own tools, and its calls are
reported under the script and then under the subagent.

## Hooks

Hooks are commands the user sets up to run at four points of a conversation:
before a tool call, after a tool call that succeeded, when a prompt is about to
start a turn, and when the agent is about to end its turn. A hook is a shell
command line, run with `bash -c` (`cmd /C` on Windows) in the conversation's
folder, that reads a JSON payload on standard input and answers on standard
output and with its exit code. The format is Claude Code's, as its hooks
reference at <https://code.claude.com/docs/en/hooks> describes it, so hooks a
user already has keep working:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "~/bin/no-rm.sh", "timeout": 10 }]
      }
    ]
  }
}
```

They are read from crab's own `hooks.json` in `~/.config/oh-my-crab/`, or the
file `OMC_HOOKS_FILE` names; from Claude Code's `settings.json` in `~/.claude`
(or `CLAUDE_CONFIG_DIR`); from `hooks/hooks.json` of each installed Claude Code
plugin, run with `CLAUDE_PLUGIN_ROOT` set to the plugin's folder; and, once the
project is trusted, from `.oh-my-crab/hooks.json`, `.claude/settings.json` and
`.claude/settings.local.json` in the folders from the working folder up to the
repository root. Hooks from every file run; a file that sets
`disableAllHooks` turns them all off. The events are `PreToolUse`,
`PostToolUse`, `UserPromptSubmit`, `Stop` and `SubagentStop`; other events and
other hook types than `command` are skipped, so one file can serve both
programs. A hook may run for `timeout` seconds, 60 by default and 30 for
`UserPromptSubmit`; past that it is killed with everything it started. Hooks
of one event that match run at once, and the same command listed twice runs
once.

A matcher names the tools a hook runs for. Empty or `*` is every tool; names
made of letters, digits, `_` and `-` are exact, several split by `|` or `,`;
a pattern whose only other characters are `*` and `?` is a glob over the whole
name, such as `mcp__*`; anything else is a regular expression that may match
anywhere in the name, such as `Edit.*` or `^mcp__github__`. A tool matches by
crab's name (`bash`, `edit`) and by Claude Code's name for the same tool
(`Bash`, `Edit` or `MultiEdit`). `SubagentStop` matches the kind of subagent,
and the other two events take no matcher.

Every payload holds `session_id` (the conversation's id, or empty), `cwd`,
`hook_event_name` and `permission_mode` (`default`, `plan` or
`bypassPermissions` in yolo mode), and `agent_id` and `agent_type` when a
subagent runs the hook: subagents run the same hooks as the main agent, under
the id of the `task` call that started them, followed by `/` and their place
when the call started several. A tool call adds `tool_name`, `tool_input` (the
arguments, already put right against the tool's schema) and `tool_use_id`, and
after the call `tool_response` with the `output` the model will read. A hook
from one of Claude Code's files is told Claude Code's name for the tool, and
`tool_input.file_path` holds the `path` of the call made absolute. A prompt
adds `prompt`, and the end of a turn `stop_hook_active` and
`last_assistant_message`. The project folder is in `CLAUDE_PROJECT_DIR`.

A hook answers with exit code 0 and, when it has something to say, a JSON
object on standard output, or with exit code 2 and a reason on standard error:

- Before a tool call, exit code 2, `"permissionDecision": "deny"` in
  `hookSpecificOutput` or the older `"decision": "block"` refuses the call, and
  the model reads the reason as the call's failure. `"allow"` (or `"approve"`)
  runs it without asking the reader. `updatedInput` replaces the arguments,
  which are put right against the schema again and fail the call when they
  do not fit, and `additionalContext` is added to the result. Every denial
  wins over every allowance, and the last rewrite wins.
- After a tool call, `additionalContext`, the `reason` of `"decision":
  "block"` and what exit code 2 writes to standard error are added to the
  result as a `<hook-note>`, which the model reads and the client shows.
- When a prompt is submitted, exit code 2, `"decision": "block"` or
  `"continue": false` keeps it from the model; the turn ends with the reason as
  its failure and is saved as one that failed. Plain text on standard output
  and `additionalContext` are told to the model before the prompt. While a
  hook runs on prompts, a prompt sent during a turn waits for the turn to end
  and starts a turn of its own, so it is checked too.
- When the agent is about to end its turn, exit code 2, `"decision": "block"`
  or `additionalContext` has it go on with the reason as a notice; at most 8
  times in a row before the model calls a tool again and 24 times a turn, and
  `"continue": false` always lets the turn end.

A hook that fails with another exit code is ignored, with a line on standard
error, and what it guarded goes on. A hook that times out or cannot be started
gave no answer: before a tool call that refuses the call, since its check never
ran, and the model is told so; at the other three points the conversation goes
on as if it had not been there. A turn never waits on a hook longer than its
timeout, and stopping the turn kills the hooks it runs.

## Project trust

A freshly cloned repository can steer the agent, so what a project folder
brings is not used until the reader trusts the project: its hooks, its
instruction files (`AGENTS.md`, `CLAUDE.md`, `.claude/rules` and the like),
its skills and agent definitions, and the Claude Code plugins its settings
switch on or off. The user's own folders (`~/.claude`, `~/.codex`,
`~/.agents/skills` and crab's config folder) and the plugins the user
installed are always used, and a project that is the user's home folder is the
user's own. Crab reads no tool-server configuration from a project; the tool
servers are the ones the client passes when it opens the conversation. In
yolo mode the language servers of an untrusted project start on the first
write instead of when the conversation opens.

Trust is recorded per repository: the main checkout and every linked worktree
of it share one decision, while another repository with the same folder name
does not. Outside a repository the project is the folder. Decisions are kept
in `trust.json` under `oh-my-crab` in the user's data folder, or in the file
`OMC_TRUST_FILE` names, with the project's hook commands as they were when it
was trusted. When a trusted project later has a hook that is not among them,
added or changed, its hooks are left out until it is trusted again, while its
instruction files, skills and agent definitions stay in use; a hook that is
removed changes nothing.

When something is left out, crab tells the client with a `Trust` event: the
folder trust is recorded for, the standing (`Unknown`, `Changed` or
`Declined`), and each input left out with its kind, name and file. The client
answers with a `Trust` command under the event's ticket: `Always` trusts the
project for good, `Allow` for this conversation only, and `Reject` records
that it is not trusted, which is not asked about again in that conversation.
What a trusted project brings is used from the next turn on. The event is sent
when the conversation opens and again when what is left out changes, since the
files are read again before every prompt. `crab trust <folder> on` and `off`
record the same decisions from a shell, and `crab hooks [folder]` prints every
hook found, whether it runs, and what is left out. Set `OMC_TRUST_ALL=1` to
trust every project for the conversations of the process without recording
anything, for automation that vouches for what it runs in.

## Long conversations

When a conversation's context nears the model's window, `crab` first leaves
old tool outputs out of what the model is sent: every output of at least 50
tokens outside the most recent 40,000 tokens of tool output, as long as that
saves at least 20,000 tokens. Each is replaced with a short note. A read made
again with the same arguments makes the earlier copy out of date, and that copy
is left out as soon as doing so costs little of the provider's prompt cache.
When leaving outputs out is not enough, `crab` shakes the conversation, still
without asking the model anything: outside the most recent messages it would
keep in a summary, every tool output of at least 100 tokens and every fenced
block of at least 400 tokens in an earlier prompt or reply is replaced with a
note such as `[120 lines of bash output removed to save room. Call recall with
call_id "c1" to read it again.]`, as long as that saves at least 4,000 tokens.
Outputs of `skill`, `context_notes` and `todo` are kept. A shake is saved like
the leaving out of outputs, so a reopened conversation gives the model the same
thing. When the context is still over the threshold, `crab` compacts the
conversation before the next request. It asks the model for a structured summary of the
older messages, building on the previous summary when there is one, and keeps
about 20,000 tokens of the most recent messages as they are. The threshold is
the window less 15% of it; on a window over 65,536 tokens it is at least 16,384
tokens below the window. When a provider still refuses a request as too long,
`crab` compacts and sends the request again, once a turn. Sending `/compact`,
optionally followed by what the summary should keep in particular, compacts on
demand; it takes no files.

A compaction deletes nothing. It is recorded at the point it happened, the
summary goes to the model in place of the older messages, and the whole
conversation stays saved and is still shown when it is opened again, with the
compaction as a finished `compact` step holding the summary. When the todo
list the model last wrote is hidden by a compaction and still has open items,
they are listed again after the summary; a list the model wrote after that,
which it still sees, takes its place. A `/compact` the
reader sent comes back as a turn of its own, and one that failed comes back
with its reason.

Nothing left out or summarized is lost to the model either. Once a
conversation has been pruned or compacted, the model can call `recall` to read
the whole history again: the full output of one tool call, the lines that
contain some text, or a range of messages by number. `recall` is on the tool
list from the first turn, so the list does not change and cost the prompt
cache when it becomes useful; called before anything was left out, it says
there is nothing to read yet.

## Memory

The agent keeps a memory that outlives conversations. With `remember` it saves
one durable fact or lesson at a time, such as a build quirk, a convention the
code does not spell out or a preference the user stated, with a few tags if it
likes; `memory_search` finds memories by the words of their text and tags,
forgiving typos and word endings, best match first; and `memory_edit` corrects
a memory or forgets it by its id. Each memory records when it was saved and
the conversation that saved it, and when and by which conversation it was last
changed. Saving text that is already kept, whatever its case and spacing, adds
the new tags to the memory that is there instead of keeping it twice.

Memories belong to a project by default. In a git repository the project is
the repository: the main checkout and every linked worktree of it, such as the
one an editor gives each agent session, share one memory, while a submodule
and an unrelated repository with the same folder name each have their own.
Outside a repository the project is the folder. A memory saved with
`scope: "user"` is kept for the user across every project instead.

The first prompt of a conversation starts with a digest of the memories in a
`<memory>` notice, those that share the most words with the prompt first and
the most recently changed among equals, at most 4,000 bytes with each memory
cut to 300; a last line counts the ones left out, which `memory_search` still
finds. With nothing saved yet, the notice says so. The digest is told once:
memories saved later in the conversation reach the model through the tool
call that saved them, and the system prompt and the tool list never change
for them, so the prompt cache holds. After a compaction or a fresh context
window has dropped the digest, the next prompt carries a new one.

Subagents can search the memory but not change it: `memory_search` is among
the tools they get, so what one of them learns comes back in its result for
the main agent to judge before anything is saved.

Memories are kept as JSON lines, one memory to a line, under
`oh-my-crab/memory` in the user's data folder, or in the folder
`OMC_MEMORY_DIR` names: a project's in `projects/<name>-<hash>/memories.jsonl`,
beside a `project.json` saying which project it is for, and the user's in
`user/memories.jsonl`. They can be read and edited by hand; a line that is not
a memory is left as it is. Every change holds a lock on `memories.lock` beside
the file, reads it again and replaces it whole through a temporary file, so
conversations writing at once never lose each other's memories and a reader
never sees half a file. On Unix the folders and files are made readable by
the user alone.

The `memory` setting (`on` by default, or `off`) is offered in each
conversation; off, nothing is loaded and the agent has none of the three
tools. Turning it either way changes the tool list from the next turn on.
Set `OMC_MEMORY=0` to turn memory off for every conversation of the process;
the setting is then not offered. `crab memory [folder]` prints the memories a
conversation in the folder, or in the current one, would start with, as JSON:
the project's name and folder, where its memories and the user's are kept,
whether memory is on, and every memory with its scope.

## When things go wrong

A request that fails in a way that passes on its own (a rate limit, an
overloaded or failing server, a dropped connection or a stream that ends before
the reply is complete) is sent again, up to 8 times. The wait doubles each
time from about 2 seconds, with some jitter, up to 60 seconds; when the
provider says how long to wait with `retry-after` or `retry-after-ms`, that wait
is kept to, and a provider that asks for more than 60 seconds, however much
more, is not waited for. Each wait shows as a running `retry` step, under an id
no other wait of the process has, saying which attempt it is and why, and
stopping the turn ends it. Sign-in problems, spent quotas, billing, missing
credits and requests the provider will not take fail the turn at once, and so
does a reply the model declined to give or the provider's content filter
stopped.

A reply that is empty, or that the output limit or a full context window cut
off before any tool call,
is not kept: the model is sent a short notice, which the client never shows as
a turn and the history does not keep, and asked again, up to three times a
turn. A reply cut off part of the way through its text is carried on and kept
as one reply. The tool calls of a reply the output limit cut off are never run:
each fails with the reason, and the model is told to take a smaller step, up
to three times a turn, counted apart from the other recoveries, before the
turn fails. Arguments sent as a JSON string holding an object are read out of
it; a call whose arguments are not a JSON object fails with what is wrong with
them. Arguments are put right against the tool's schema before the call is
shown or approved, so what the reader approves is exactly what runs.

When the model makes the same calls with the same arguments five times in a
row, or makes three edits in a row to one file that leave it as it was, a
notice telling it to change course is added to the results of that step, and
after six such edits it is told to stop and explain to the user.

The todo list the model keeps with `todo` is watched as well. When it still
has items pending or in progress after 12 calls that changed files since it
last changed, a reminder listing them is added to the results of that step, at
most twice before the list changes again. A call counts when the files it
reports changing really changed, or when it is a successful `write`; a shell
command that only looks, such as `ls` or a test run, does not. When a turn
that wrote the list or changed files is about to end with open items, the
model is sent a notice, once a turn, to finish them or update the list; a list
left over from an earlier turn does not hold up a turn that only answered a
question. Neither happens when the model's reply ends by asking the user a
question.

Nothing in the text of a reply is ever run, so a call the model quotes from a
file or from what the user pasted cannot turn into a real one. When a reply
without tool calls holds what looks like a call written out as text, a block
marked as a call, such as `<tool_call>{"name": "read", "arguments":
{...}}</tool_call>`, or a fenced JSON block that is a call of a known tool or
the arguments of an `edit`, the model is sent a notice, once a turn, that
nothing ran and that it should make a real call if it meant to. Any other code
block, JSON included, is left alone.

Both notices come after an answer that is already kept, in the history and in
the saved conversation. The model's reply to a notice is kept as a reply of its
own, and an empty one ends the turn; it is given to the model joined to the
answer before it, since every provider wants a message from the reader between
two replies. When the request that follows a notice fails, the turn ends with
the answer it already had instead of being undone.

## Reading code

`read` of a code file without a line selector returns an outline instead of
the text when the file has at least 200 lines and is written in Rust, Go,
TypeScript, JavaScript, Python, C, C++, Java or Lua. The file is parsed with
the same tree-sitter grammars as `ast_grep`. Imports, signatures, type, trait,
impl and class headers, constants and comments are shown with their real line
numbers, and every function or method body that would hide at least six lines
is folded to one unnumbered line such as `… 30 lines folded (120-149)`. A
Python docstring stays visible, and a callback that only calls other functions
with callbacks, such as a `describe` block, is opened so that each test inside
it is folded on its own. A footer lists the folded ranges as a selector, for
example `read src/lib.rs:120-149,200-260`, and `src/lib.rs:1-` reads the whole
text. A file is read whole as before when it is shorter, does not parse
cleanly, is larger than 4 MiB, or when folding would hide less than a quarter of
its lines. A selector always returns the text. A program that builds the
tools itself can turn outlines off with `Read::with_outline(sandbox, false)`.

An outline carries a tag of its own: the letter `O` and four hex digits, so it
can never be mistaken for the four digits of the text's tag. `edit` accepts
that tag, works out the outline's folds again from the file and refuses any
operation that touches a folded line, naming the range to read first. An edit
made with an outline's tag returns the outline tag of the new contents and
says so, so a chained edit still keeps off the folded lines. Reading a range
returns the plain tag, which allows edits anywhere, as does the tag an edit
made with a plain tag returns. The tag is checked before anything else in the
call, so a stale tag is reported as such. Each kind of read is kept apart when old reads are
left out: an outline is only made out of date by another bare read of the same
path, never by a read of a range, and a read of a range never by an outline.

### Merge conflicts

When the lines `read` returns include a merge conflict marker (`<<<<<<<`,
`|||||||`, `=======` or `>>>>>>>` at the start of a line), a note after them
lists every conflict in the file: its number, counting from 1 in file order,
its lines, and each side with its label and lines, the common ancestor's side
included for the diff3 and zdiff3 styles. A file with conflicts is never read
as an outline. `read src/lib.rs:conflicts` returns only the conflict blocks,
numbered, under the file's tag.

`edit` resolves a conflict by number without the model typing the block out:
`{"op": "resolve", "conflict": 2, "take": "theirs"}` replaces the whole block,
markers included, with one side, `ours`, `theirs`, `base` or `both` (ours then
theirs), or with `text` instead of `take`, where an empty text removes the
block. The number is checked against the tag like any other operation, so a
file changed since the read is refused rather than resolved in the wrong
place.

### Generated files

`edit`, `write` (when it would overwrite a file) and `ast_edit` refuse files
that a tool or package manager writes. A file counts as one when a comment in
its first 30 lines carries a generator's banner, such as `Code generated …
DO NOT EDIT`, any other line saying generated and do not edit or modify,
`@generated`, `This file was automatically generated` or `<auto-generated>`,
or when a comment in its first 10 lines says `generated by` protoc, gRPC, sqlc,
Swagger, OpenAPI Generator, FlatBuffers or Thrift. Comments are recognized in
the syntax of the file's extension: `#` in shell, Python, TOML and YAML, `//`
and `/* */` in C-like languages, only `<!-- -->` in Markdown, HTML and plain
text, so a Markdown heading or bullet never counts, and a passing mention such
as an id "auto-generated by the database" is no banner. A file also counts as
one when its name is a lockfile
(`Cargo.lock`, `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `go.sum` and
others) or one generators use (`*.pb.go`, `*_pb2.py`, `*.g.dart`,
`*.designer.cs` and others). The refusal names the evidence and tells the
model to change the source or the generator instead. When the user explicitly
asks for the file itself to change, the model sets `generated: true`: in the
file's section for `edit`, and as an argument of `write` and `ast_edit`. An
`ast_edit` over a directory leaves generated files unchanged and lists them.

### Mistyped paths

When `read` is given a path that does not exist, it looks for files in the
working directory, honoring `.gitignore`, whose path ends with the one given,
or matches it when case is ignored. If there is exactly one, it is read and the
result begins by saying which file was read instead; if there are several, the
error lists up to five. `edit` never follows such a path: its error names the
candidates. `write` refuses a new file whose directory does not exist when
existing files end with its path, and creates it anyway with `create: true`.

## Reading images, databases, archives and JSON

An image larger than 3.75 MB, or longer than 2000 pixels on a side, is not
refused but shrunk before it reaches the model: it is decoded, turned upright
by its EXIF orientation, scaled so its long side is at most 2000 pixels and
re-encoded, as PNG when it has transparent pixels and that stays small enough,
as JPEG at quality 80 otherwise. Lower JPEG qualities and then smaller sizes
are tried until it fits. The header names the original and shown sizes and
the factor to multiply coordinates by to map them back to the original.
Images that already fit are passed on unchanged; files over 64 MiB are
refused, and so is an image that must shrink but is more than 12000 pixels on
a side or whose pixels would take more than 256 MiB to decode, which is
checked from its header before any pixel is decoded, so a small file cannot
unpack into a huge bitmap. Shrinking scales by averaging blocks of pixels and
runs off the async runtime, for files and fetched URLs alike.

The text after a database, archive or JSON file's name and a `:` names what
to read inside it, and the usual line selectors page the result:

| Path                           | Result                                                    |
| ------------------------------ | --------------------------------------------------------- |
| `app.db`                       | tables and views, with row counts and `CREATE` statements |
| `app.db:users`                 | the schema and the first 50 rows, numbered from 1         |
| `app.db:users:51-100`, `:-20`  | other rows; `offset` and `limit` work too                 |
| `app.db:SELECT ...`            | a read-only query, paged the same way                     |
| `src.zip`                      | members with their sizes                                  |
| `src.zip:lib/a.rs:10-40`       | a member's text, numbered, without a tag                  |
| `src.zip:lib`                  | the members under a directory                             |
| `data.json:.items[0].name`     | the query's values, pretty-printed and numbered           |
| `data.json:.items[]:100-200`   | lines of that output                                      |

SQLite files are found by their header, whatever their name. The database is
opened read-only with `query_only` set, only `SELECT`, `WITH` and
`PRAGMA table_info` statements run, and a statement that would write is
refused. Every statement, the row counts of the table list and of a table
included, is interrupted at the 20-second deadline of the read, so the thread
running it stops too. Row ranges as large as the largest number are clamped,
not overflowed. Zip files are found by their header, tar files by their
header or `.tar` name, and gzipped tar files by a gzip header and a `.tar.gz`
or `.tgz` name. A member is unpacked only up to 32 MiB and a gzip stream only
up to 1 GiB, so a small archive cannot unpack into unbounded memory. The tar
stream is followed block by block as it is read, and a long name of more than
4096 bytes, an extended header of more than 64 KiB or a member path longer
than 4096 bytes is refused before it is loaded into memory. Binary members
are reported with their size; image and PDF members are shown as images and
PDF files are. A JSON file followed by a line selector such as
`data.json:20-40` is read as text, as before.

A JSON query is a small path language of `crab`'s own, not jq: a pipeline of
stages separated by `|`, each applied to every value the stage before it
yields. A stage is a path or a builtin:

- `.` is the value itself; `.name`, `."a key"` and `.["a key"]` take a key of
  an object, `.[2]` and `.[-1]` an element of an array, `.[2:5]` and `.[:-1]`
  part of an array or string, and `.[]` every element of an array or value of
  an object. Steps chain, as in `.items[0].name` or `.items[].tags[]`. A
  missing key or an index past the end gives `null`.
- `keys` (an object's keys, sorted, or an array's indexes), `length`, `type`,
  `first`, `last`, and `select(.path == literal)` or `select(.path != literal)`
  with a string, number, `true`, `false` or `null` literal.

Anything else, such as a function jq has but this language lacks, is refused
with the character it starts at. A query yields at most 100,000 values and
4 MB of output, and evaluation never recurses deeper than the query is long.
The document is parsed with a nesting limit of 128 levels, and a deeper one
is refused with a message saying so. A read inside a database, archive or
JSON file that takes more than 20 seconds is given up on and fails.

## Reading the web and GitHub

`read` of an `http://` or `https://` URL fetches the page and returns it as
numbered lines, like a file. An HTML page becomes Markdown with its links and
images made absolute, so they can be read in turn. Only the page's main
content is kept: its `main` element, or its one `article`, without the
navigation, headers, footers, sidebars, cookie banners and hidden elements
around it; a page without either keeps its body, still without that chrome,
and a page where that would leave next to nothing is kept whole. The padding
that lines up table columns is taken out. `url:full` converts the whole page
and `url:raw` returns the HTML as sent. A PDF, such as an arXiv paper, gives
its text page by page, JSON is indented with its keys in the order sent
(`url:raw` keeps it as sent), and other text is shown as it is. Nothing on the
page runs: scripts are dropped unread. Line selectors work as they do for
files (`url:100-200`). A fetch takes at most 30 seconds and reads at most
10 MiB of the body. A page longer than one read shows is saved whole, in the
same folder as long `bash` output, and the note that says where to go on
points into that file, so the rest is paged from disk rather than fetched
again.

GitHub is read through the same tool, by URL or by a short path, without the
model needing other tools:

| Path                                             | Result                                                 |
| ------------------------------------------------ | ------------------------------------------------------ |
| `pr://123`, `issue://45`                         | in the repository the `origin` remote points at        |
| `pr://owner/repo/123`, `issue://owner/repo/45`   | in the repository named                                |
| `gh://owner/repo/pull/123`                       | state, description, changed files, reviews, comments   |
| `pr://123:diff`, `gh://owner/repo/pull/123:diff` | the unified diff; `:diff:1-400` pages it               |
| `gh://owner/repo/issues/45`                      | the issue and its comments                             |
| `gh://owner/repo/blob/<ref>/<path>`              | a file at a branch, tag or commit, with line selectors |
| `gh://owner/repo/tree/<ref>/<path>`              | a directory at a ref                                   |
| `gh://owner/repo`                                | the repository's facts, top-level files and README     |

`https://github.com/...` URLs of the same things, and `github.com/...` without
the scheme when no such file exists in the working directory, read the same
way; a pull request's `/files` page reads as its diff. A ref is the first
path segment after `blob` or `tree`, so a branch with a `/` in its name is
named by its commit instead. Other github.com pages
are fetched as web pages. The repository of `pr://` and `issue://` comes from
`git remote get-url origin` in SSH, `ssh://`, `git://` or HTTPS form; when
there is no such remote, or it is not on GitHub, the error says to name the
repository. GitHub is asked through its REST API, signed in with the token of
the `gh` command line tool when it is installed and signed in, otherwise with
`GH_TOKEN` or `GITHUB_TOKEN`, otherwise not at all, which reaches public
repositories at GitHub's lower anonymous rate limit. The token is kept in
memory for the rest of the process and forgotten when GitHub refuses it;
nothing is written to disk. Lists of comments, reviews and files show their
first 100 entries. The header of each result says how the request was signed.

Reading the network asks first unless the conversation is in yolo mode, as
`bash` does. The approval is per site: "always allow" for one origin never
covers another, and every GitHub path and github.com URL counts as
`https://github.com`. A redirect to another origin is not followed but
reported, so that reading its target is approved afresh, and a host name that
resolves to a local or private address is refused.

`web_search` sends one query to a search service and returns ranked results,
each with its title, URL, date when known and snippet, after a short written
answer when the service gives one. The services are tried in order, and when
one fails the next is asked and the result notes the failure:

| Service    | Set                                             |
| ---------- | ----------------------------------------------- |
| Brave      | `BRAVE_API_KEY` or `BRAVE_SEARCH_API_KEY`       |
| Tavily     | `TAVILY_API_KEY`; also writes an answer         |
| Exa        | `EXA_API_KEY`                                   |
| Perplexity | `PERPLEXITY_API_KEY` or `PPLX_API_KEY`          |
| Kagi       | `KAGI_API_KEY`                                  |
| SearXNG    | `SEARXNG_URL` or `OMC_SEARXNG_URL`, an instance with JSON output on |
| DuckDuckGo | nothing                                         |

Without `OMC_SEARCH`, every service that is set up is tried in that order,
with DuckDuckGo last. `OMC_SEARCH` names the services to try and their order,
as in `OMC_SEARCH=tavily,searxng`; a service it names that is not set up is
skipped and noted, and leaving `duckduckgo` out of it means no search runs
without a key. When no service can be asked, the error lists the variables to
set. DuckDuckGo has no search API, so its no-script results page is read
instead: it needs no account, but its markup can change and it sometimes
answers automated searches with a challenge page, which counts as a failure.
Set a key for searches that must work. Perplexity is asked through its Search
API, which returns results without an answer. `recency` limits results to the
last day, week, month or year where the service can: SearXNG has no week and
widens it to a month, and Kagi has no year and ignores it. Each service gets
20 seconds. A search sends the query off the machine, so it asks for approval
outside yolo mode, under one scope for all searches.

## Prompt cache

Providers cache the start of a request, tools then system prompt then
messages, only while it stays byte for byte the same. `crab` therefore makes
the system prompt and the tool list once per conversation, when its first
turn starts, and sends them unchanged after that; they are made again only
when the model changes or the checkpoint tools or memory are turned on or
off. What changes later reaches the agent as a short notice at the start of
the next prompt instead:

- today's date and the git branch, on the first prompt, again when either
  changes, and again after a compaction or a fresh context window has dropped
  the earlier notice;
- plan mode turning on, with its rules and the tools it allows, and turning
  off;
- skills, subagent kinds and instruction files that were added, changed or
  removed on disk since the system prompt was written; the notice only points
  at tools the agent is offered;
- the digest of the memories earlier conversations saved, on the first prompt
  and again after a compaction or a fresh context window has dropped it.

The `skill` tool is on the list even when no skill exists yet, so that a skill
added later can be loaded without changing the list; until then a call to it
says there are no skills.

The system prompt carries an outline of the working directory, so the agent
need not list folders before it starts: three levels deep, folders first and
by name, at most 25 entries for the working directory itself and 15 for any
folder below it, with a line counting the rest. Entries that `.gitignore`,
`.ignore` or git's exclude files leave out are not shown, in a repository or
not, nor are hidden entries or the folders of version control, builds and
vendored packages (`.git`, `target`, `build`, `dist`, `node_modules`,
`vendor`, `.venv` and the like). When the outline would pass 120 lines, the
deepest folders are left closed, a level at a time from the top, so the upper
levels are always whole. A walk that finds more than 20,000 entries is tried
again a level shallower, and one that takes longer than 300 milliseconds, on
a network mount say, gives no outline at all; the walk runs on a thread of its
own, so it never holds up the rest of `crab`. The outline is made once, when
the conversation's first turn starts, and kept unchanged for the rest of it,
also when the system prompt is made again for another model; the prompt says
it is a snapshot from the start of the conversation. Subagents get the same
outline in their own system prompts.

Plan mode no longer takes tools off the list. The tools that change things
stay listed, and a call to one while plan mode is on is refused with a
message naming the tools that are allowed; it is never run and never asks for
approval. Outside plan mode `exit_plan_mode` is refused the same way.

Two settings shape the cache on Claude:

- `cache` (`5m` or `1h`) is how long cached context lives after each request.
  An hour costs twice the input price to write instead of a quarter more, and
  pays off when replies come more than five minutes apart.
- `cache_warming` (`on` by default, or `off`). Shortly before cached context
  would expire, at nine tenths of its life, `crab` sends the last request again
  asking for no output, which keeps the cache alive at the price of reading it.
  It does so only when the expected saving, the chance of another request
  before expiry times the extra cost of a miss, less the cost of the refresh,
  is at least $0.02: the chance is taken as certain while a turn runs (a long
  tool call) and as 15% while the conversation waits for the reader. Warming
  stops an hour after the last real request while a turn runs and half an hour
  after it while idle; when the model, the effort, the checkpoint setting or a
  cache setting changes; when the turn is cancelled or fails, before a
  compaction the reader asked for, and when another conversation is opened;
  and for models that think within a fixed token budget, whose requests cannot
  be replayed without output. Nothing is warmed while a request is on its way,
  since that request reads the cache itself. What warming cost is saved with
  the conversation under `cache_warming`.

Prices per model and cache lifetimes are in `crates/omc-providers/src/models.rs`.
The requests that summarize a conversation for a compaction write nothing to
the cache, since they are never sent again.

## Language services

After `edit` or `write` changes a file, `crab` has it formatted as its language
says and adds the errors and warnings found in the files it changed to the
tool's result, at most 20, waiting up to three seconds for them. The tag in the
result is the tag of the formatted file, so the next edit can use it.

The formatting and the diagnostics come from whoever can provide them, and only
one set of language servers runs:

- A client that runs language servers of its own sets `languages` when it opens
  the conversation. `crab` then sends it each request as a `Language` event
  with a ticket, and it answers with a `Language` command under that ticket. A
  request it does not answer in time gets no formatting and no diagnostics.
- Otherwise `crab` starts the usual server of each language itself, when its
  program is installed: `rust-analyzer`, `gopls`, `typescript-language-server`,
  `pyright-langserver` or `pylsp`, `clangd`, `lua-language-server` and `zls`.
  Loading a project can run its build scripts, so a server starts the first
  time a file of its language is written, which the user allowed. In `yolo`
  mode the servers of the languages the project root names (`Cargo.toml`,
  `go.mod`, `package.json` and so on) start when the conversation opens
  instead, so the first write already gets its diagnostics; a server started
  on the first write may still be loading and miss them. A server that stops
  is started again, up to three times. These come from `crates/omc-lsp`, which
  the default `lsp` feature of `omc-session` builds in; a build without it has
  no servers of its own.
- A program that embeds `omc-session` can pass its own
  `omc_tools::LanguageService` in `Options` to `run_with`, and one that builds
  the tools itself can hand `Registry::builtin_in` its own `Languages`.

With none of these, `edit` and `write` behave as they would without language
services.

## Debugging

The `debug` tool drives real debuggers through the Debug Adapter Protocol, so
the agent can find why a program crashes, hangs or computes a wrong value by
stopping it and looking, rather than by adding print statements. One tool takes
an `action`: `launch` a program with its arguments, folder and environment, or
`attach` to a process by id or to an adapter already listening at
`host:port`; set and clear breakpoints at a line, with a condition, a hit count
or a log message instead of a stop, on a function, or on exceptions;
`continue`, `pause` and step over, into or out; list the threads, the stack and
a frame's scopes; read variables, expanded to a few levels with a bounded
number of children each; `evaluate` an expression in a frame; read what the
program printed; and `disconnect`. Every answer is short text rather than the
protocol's JSON: a stop says why and where, shows the source line with the
lines around it, the top frames and the frame's arguments and locals, and what
the program printed since the last call.

The adapter comes from the program: a compiled program is debugged with
`lldb-dap`, `codelldb` or `gdb -i dap` (gdb 14 or newer), whichever is
installed first in that order, a `.py` file with `debugpy` (the project's
`.venv` or `venv` interpreter first, then `python3`, as long as it can import
`debugpy`), and a `.go` file or Go package folder with `dlv dap`. A call can
name the `language` or the `adapter`, and add adapter-specific settings to the
launch request with `config`. When nothing is installed for the language, the
error names what to install. `OMC_DAP_<ID>` replaces an adapter's command, as
in `OMC_DAP_LLDB_DAP=/opt/llvm/bin/lldb-dap` or
`OMC_DAP_DEBUGPY="/srv/venv/bin/python -m debugpy.adapter"`, and
`debuggers.json` in `~/.config/oh-my-crab/`, or the file `OMC_DEBUGGERS_FILE`
names, changes the known adapters and adds others:

```json
{
  "adapters": [
    { "id": "gdb", "command": ["/opt/gdb/bin/gdb", "-i", "dap"] },
    {
      "id": "zig-lldb",
      "command": ["/opt/zig-lldb/bin/lldb-dap"],
      "languages": ["zig"],
      "transport": "stdio",
      "launch": { "initCommands": [] }
    }
  ]
}
```

A new adapter needs `command` and `languages`, and is preferred over the known
ones for its languages. `transport` is `stdio`, or `port` for an adapter that
listens on the port written `{port}` in its command. `launch`, `binary`,
`attach` and `remote` are settings added to every launch, to launches of a
compiled program, to attaching to a process, and to attaching through an
address. Adapters that start child sessions of their own, as the JavaScript
debugger does, are not supported.

A session lasts across tool calls until the agent disconnects it, up to four at
once, named `dbg-1`, `dbg-2` and so on; an action without `session` acts on the
one used last. Running and stepping wait for the next stop at most `timeout`
seconds, 15 by default and 300 at most, and then answer that the program is
still running instead of holding the turn; `wait` waits again and `pause`
interrupts it. A stop or an exit that happens between calls is reported at the
top of the next call on that session, and a `continue` or step then reports it
instead of running on. Every request to the adapter has a time limit too, so an
adapter that stops answering fails the call rather than hanging it. Sessions
belong to the conversation, like background runs: a cancelled turn leaves them
as they are, and when the conversation closes or another is opened each
adapter is told to disconnect, which ends the programs it launched and detaches
from the ones it attached to; whatever is left after a few seconds is killed,
together with the adapter's process group and any program a session launched.

Launching and attaching run code, so outside `yolo` mode they ask for approval
like `bash`, under the scope `debug`; the other actions work on a session the
user already allowed and ask nothing. The program, its folder and the files of
line breakpoints must be inside the working directory, and source lines are
shown only from files inside it. Expressions are evaluated as watch
expressions, never as debugger commands. `debug` changes the program's state,
so it is not offered in plan mode, and subagents do not get it. Its tool events
are of kind `Execute` when they launch, attach, continue or step, and `Other`
otherwise.

## Providers

Signed in with OAuth or a device code: `claude`, `codex` and `copilot`.

Signed in with a Kimi account's one-time code or with an API key: `kimi-code`
(Kimi Code, the Kimi For Coding subscription). The account login presents
itself as Kimi's own command-line client, which Kimi's terms forbid third-party
tools to do; see [docs/providers](docs/providers/supported.md).

With an API key from the environment, or one a client saved: `moonshot`, `zai-coding` (the
Z.ai GLM Coding Plan), `zai`, `minimax`, `deepseek`, `xai`, `groq`,
`cerebras`, `mistral`, `openai`, `gemini`, `together`, `fireworks` and
`openrouter`.

Local servers that need no key: `ollama`, `lmstudio`, `llamacpp` and `vllm`.

See [docs/providers](docs/providers/supported.md) for each one's key variable,
address and default model, and for how to add one.

### Custom endpoints

Any other server that speaks the Chat Completions API, such as a company
gateway or a local server on another port, can be added without a change to
crab. Write it to `providers.json` beside `auth.json`, in
`~/.config/oh-my-crab/`, or in the file `OMC_PROVIDERS_FILE` names:

```json
{
  "providers": [
    {
      "id": "work",
      "label": "Work gateway",
      "base_url": "https://llm.example.com/v1",
      "wire": "chat_completions",
      "api_key_env": "WORK_LLM_KEY",
      "headers": { "X-Team": "infra" },
      "default_model": "qwen-coder",
      "replay_reasoning": false
    }
  ]
}
```

Only `id` and `base_url` are needed. The id is lowercase letters, digits, `-`
and `_`, and cannot be a built-in one. The label is the id when left out.
`wire` takes only `chat_completions`, its default. `api_key_env` is one
variable or a list tried in order; without it no key is sent. `default_model`
is the model a conversation opens with when none is picked; without it crab
takes the first model the server lists. `replay_reasoning` sends the model's
earlier reasoning back as `reasoning_content`, which servers of thinking
models such as Kimi and DeepSeek want around tool calls.

The file is read once, when the providers are first listed. A custom provider
is listed, signed in to and talked to like a built-in one. A file that does not
parse, or that defines a provider wrongly, is ignored as a whole, and crab says
why on standard error.

A client lists them with `crab providers`, or `omc_session::providers()` when it
links the harness, so a provider added to crab shows up in every client without
a change there.

## Development

```sh
make fmt
make lint
cargo test --workspace
```

## License

MIT, see [LICENSE](LICENSE).
