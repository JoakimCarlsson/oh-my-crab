# Concepts

The ideas the rest of these docs build on, a short section each, with a link
to where the details are. [Getting started](getting-started.md) shows most of
them in a live session.

## Conversation

A conversation is one agent working in one folder with one provider. A client
starts `crab serve`, sends `Open` with the folder, the provider and optionally
a model, and from then on the two talk about that conversation only. One
`crab serve` process holds one conversation at a time: an `Open` for another
replaces it, and `Close` ends the process. pandemonium starts one process per
agent session.

What a conversation can use is set when it opens: the built-in tools, the
tools of the tool servers the client passes and of those set up in files, less
the tools the client denies, and the skills, agent definitions and instruction
files found on disk. A project's tool servers join once it is trusted, and a
server that wanted a sign-in once the reader signed in.
See [`Open`](wire-protocol.md#open).

## Turn

A turn is one prompt and everything the agent does for it. The agent asks the
model, runs the tool calls in the reply, sends the results back, and repeats
until the model answers without calling a tool. The turn then ends with
`Finished`: `null` when it went well, or the reason it failed. `Cancel` stops
a turn, subagents included; the client then sees `"Halted"` and `Finished`.

## History: saved and reopened by id

A conversation opened with an id (`conversation` in `Open`) is saved by
`crab`, so a client does not need to keep a copy. Saved with it are:

- its messages and turns, with the id the client gave each turn;
- the turns that were stopped or failed, which the model never sees but a
  client still shows;
- its title, the settings chosen for it and the list of what can be set;
- the commands it offers and the size of the context window;
- the agent's notebook, and the files each tool call changed.

Opening the same id again brings it back as it was, in a `Resumed` event, and
the model carries on from the whole conversation. The history is one JSON
lines file, `<id>.jsonl`, under `oh-my-crab/conversations` in the user's data
folder, or in the folder `history` names. `keep` opens only the latest turns,
and `before` goes back to before a turn, for a client that lets the reader go
back and try again. Without an id nothing is saved.

## Branches

Going back does not throw the later turns away. A saved conversation is a
tree: going back to before a turn starts a new branch from that point, and
the turns left behind stay saved on the branch they were on. `Resumed` lists
every branch with the turns it holds and where it went back from, and
`Switch` moves the conversation to another branch at its tip, so the reader
can return to an earlier line of work and carry on from it. `crab fork`
copies a branch, up to a turn, into a conversation of its own. See
[branches](wire-protocol.md#branches).

The agent names an unnamed conversation itself with `set_title`, and the
client hears it as `Titled`. A name the client sends with `Rename` always
wins. `crab history`, `crab rename` and `crab import` work on saved
conversations from a shell; see [commands](usage.md) and
[history replay](wire-protocol.md#history-replay).

## Commands and events

The client sends commands and `crab` sends events, one JSON message per line.
Commands are few: `Open`, `Prompt`, `Cancel`, `Set`, `Rename`, `Login`,
`Switch`, `Close`, and the answers `Choose`, `Answer`, `Trust` and
`Language`. Events report what happens as it happens: the reply streamed in
`Said` and `Thought` chunks, tool calls as `Tool` events that are updated as
they run, questions that wait for an answer, and the end of each turn.

A question carries a ticket, and its answer names that ticket. Approvals are
`Ask` events answered with `Choose`, the `ask` tool's forms are `Inquire`
answered with `Answer`, and project trust is `Trust` answered with `Trust`.
See the [wire protocol](wire-protocol.md).

## Tool calls and approvals

Tools that only look, such as `read`, `grep` and `glob`, run without asking.
Tools that change things or reach out ask the reader first: `bash`, `edit`,
`write`, `ast_edit`, `debug` when it launches or attaches, `web_search`,
reading a web page, and tool-server tools. The question is an `Ask` event
offering "Allow once", "Always allow" and "Reject". A rejected call fails and
the model reads that the user denied it.

"Always allow" adds permission rules in Claude Code's syntax, never a whole
tool that reaches further than the call: for `bash` a rule per command, such
as `Bash(cargo test:*)` for `cargo test -p foo`, which a later `cargo test;
rm -rf ~` does not ride on; for `edit`, `write` and `ast_edit` the folder,
such as `Edit(/src/**)`; for reading the web the host, such as
`WebFetch(domain:docs.rs)`, with every GitHub path counted as `github.com`;
for other tools the tool. The rules hold for the rest of the conversation, or,
with the "in this project" and "in every project" choices, are kept in crab's
permissions file for later conversations.

Permission rules can also be written by hand, in crab's file or in Claude
Code's settings: allow rules that skip the question, ask rules that always
ask, and deny rules that stop a call even in yolo mode. A project's own
settings bring allow rules only once it is trusted. See
[permission rules](extending/permissions.md).

When one reply asks for several calls, every question is asked before any
call runs. Then calls that only look, side by side, run at once, while calls
that change things run alone, in the model's order; see
[which calls run at once](tools/calls.md#which-calls-run-at-once).

Hooks can allow or refuse a call before the reader is asked, and the client
can deny a tool outright when it opens the conversation. See
[how tool calls run](tools/calls.md) and [hooks](extending/hooks.md).

## Modes

The `mode` setting says how much the agent asks:

- **`default`** ("Ask") asks before the tools above run.
- **`plan`** only looks. A call to a tool that changes things is refused
  without running and without asking, with a message naming the tools that
  are allowed. The agent ends by handing its plan to `exit_plan_mode`, which
  the reader sees as an `Ask` with a `plan` (see [plans](#plans)).
  "Approve and execute" ends plan mode, returning to the mode it began in,
  and the agent carries on in the same turn; "Approve and execute in a fresh
  context" does the same in a fresh context window that holds only the plan;
  "Keep planning" keeps it in plan mode, passing on what the reader wrote;
  "Reject the plan" keeps it in plan mode and has the agent stop.
- **`yolo`** ("Run freely") runs everything without asking, except what a
  deny rule stops.

`"yolo": true` in `Open` starts in yolo mode. The [tool index](tools/README.md)
marks which tools plan mode allows.

## Plans

The agent submits its plan whole, as markdown, each time it calls
`exit_plan_mode`; a revision is a new submission, never a diff. The reader
answers the `Ask` with one of four choices, and may send a `note` with any of
them, which the agent is told:

| Choice    | Verdict  | What happens                                                                                                     |
| --------- | -------- | ---------------------------------------------------------------------------------------------------------------- |
| `allow`   | `Allow`  | Plan mode ends and the agent carries the plan out in the same turn, in the context it planned in.               |
| `fresh`   | `Allow`  | Plan mode ends, and once the batch of calls the approval is in ends, a fresh context window opens that starts with the plan, the note and the notebook, as `new_context` does; the planning conversation stays in the history but the model no longer sees it. |
| `reject`  | `Reject` | Plan mode goes on; the agent revises the plan, minding the note, and submits it again.                           |
| `discard` | `Reject` | Plan mode goes on; the agent is told the plan is rejected and ends its turn to wait for the reader.            |

A client that only knows `allow` and `reject` keeps working: they mean what
they always meant, and any other allowing choice approves the plan as `allow`
does.

Every submitted plan is saved as markdown beside the conversation's history,
in `<id>.plans/plan-<n>.md`, numbered in the order they were submitted, when
the conversation is saved. With the `plan_autosave` setting on, it is also
saved in the project, as `.oh-my-crab/plans/<title>.md` under the working
folder, a plan of the same title replacing the earlier one; `OMC_PLANS_DIR`
names another folder.

The plan lives in the history itself, as the `exit_plan_mode` call that
submitted it or the first message of the fresh window opened for it, so a
reopened conversation and every [branch](#branches) keep the plan they hold.
That plan is never left out of the context to make room: the output of an
`exit_plan_mode` call is never pruned or shaken out, nor is the first message
of a fresh window opened for a plan, and when a compaction summarizes the part
that holds the latest plan, the plan is restated whole after the summary,
marked approved or not.

While an approved plan is being carried out, every subagent `task` starts is
told of it in its system prompt, so it does its part as the plan describes it.
Subagents started in plan mode, while a new plan is made, are not.

The `plan_model` setting picks a model to plan with: while plan mode is on,
every request, and every subagent's, goes to that model, and once the plan is
approved, even in the middle of a turn, the conversation's `model` carries it
out. It takes a model of the conversation's provider, or of another signed-in
provider as `provider/model`; `same`, the default, plans with `model`. When
the plan model cannot be used, the client is told with a `Failed` event and
`model` plans.

## Settings

A `Settings` event lists what the client can change, with the value each has
now: `model`, `mode`, `effort` (when the model takes one), `cache` and
`cache_warming` (when the provider offers them), `memory`, `plan_model`,
`plan_autosave` and `checkpoints`.
The client changes one with `Set`, and `crab` answers with a new `Settings`
event. A new value applies from the next request.

A value chosen with `Set` is saved with the conversation and applied again
when it is reopened, over what `Open` asks for: a conversation last set to
`yolo` reopens in `yolo` even when `Open` says `"yolo": false`. A model named
in `Open` still wins over the saved one, and `defaults` in `Open` fill in
settings that were never chosen. See
[settings round-trips](wire-protocol.md#settings-round-trips) and
[conversation settings](configuration.md#conversation-settings).

## Queued prompts and steering

A prompt sent while a turn is running is not dropped. It is queued, and the
client gets a `Queued` event in the `Waiting` state with a ticket that every
later event about that prompt repeats.

At the next safe point of the running turn the waiting prompts are put into
the conversation, in the order they came, so the model reads them mid-turn.
A safe point is after the current batch of tool calls and any compaction, and
before the next model request. Each prompt put in this way is reported as
`Steered` and saved as a turn of its own, under the id the client gave it.

When the turn ends before a prompt could be put in, the prompt that waited
longest starts the next turn by itself and is reported as `Started`; any
others wait for that turn. If that turn cannot start, the prompt is reported
as `Dropped` after its `Started`.

When the turn is cancelled, fails or needs signing in again, the prompts
still waiting are never sent. Each is reported as `Dropped`, so the client can
offer it to the reader again. Prompts already put into a cancelled or failed
turn are saved with that turn, as stopped or failed.

A few commands behave differently while a turn runs:

- A `/compact` is refused with a `Failed` event.
- An `Open` cancels the turn as `Cancel` does, and drops the prompts waiting
  for it, before the other conversation is opened.
- While a hook runs on submitted prompts, a prompt sent during a turn waits
  for that turn to end and starts a turn of its own, so that the hook checks
  it too.

Every other command works as it does between turns. Prompts with files are
handled like any other prompt. See
[queued prompts](wire-protocol.md#queued-prompts).

## Background runs

`bash` with `background: true` starts a long command, such as a dev server or
a long build, and returns a handle at once; `task` with `background: true`
does the same for subagents. `wait` blocks until the next one ends and returns
its result. The client sees a `Background` event when each starts and ends.

Background runs belong to the conversation, not to the turn: cancelling a
turn leaves them running, and they are stopped when another conversation is
opened or the process ends. Debugging sessions are kept the same way.
Subagents cannot start background runs. See
[`Background`](wire-protocol.md#background).

## Subagents

The `task` tool hands self-contained assignments to subagents, each running
a loop of its own and handing back its final message, or JSON checked
against a schema. The kinds are `task`, `scout` and `reviewer`, plus the
agent definitions found on disk. One call runs up to 32, and at most 32 run at once
across the conversation, and each can work in a copy of its own of the working
tree, a copy-on-write clone or a git worktree, whose changes are applied back
as a patch when it ends. A subagent asks the main
agent's model unless its definition or the call names another, such as
`haiku`. Their tool calls reach the client as `Tool` events whose `parent` is
the `task` call.

Subagents get the main agent's tools less the ones that talk to the reader or
steer the conversation. A subagent may start subagents of its own, two levels
below the main agent by default. See
[subagents](tools/subagents.md) and
[agent definitions](extending/skills-and-agents.md#agent-definitions).

## The system prompt and notices

Providers cache the start of a request (the tools, then the system prompt,
then the messages) only while it stays byte for byte the same. So `crab`
builds the system prompt and the tool list once per conversation, when its
first turn starts, and sends them unchanged after that. They are rebuilt only
when the model changes or the `checkpoints` or `memory` setting is turned on
or off. The system prompt holds crab's instructions (or the user's or the
project's `SYSTEM.md` in their place, and any `APPEND_SYSTEM.md` after them;
see [the system prompt](extending/skills-and-agents.md#the-system-prompt)),
the instruction files that always apply (`AGENTS.md`, `CLAUDE.md` and the
like; those for some files only arrive with
[the first matching file](extending/skills-and-agents.md#rules-for-some-files-only)), the skills and
subagent kinds, the working folder, the operating system and the model, and an
outline of the working directory.
A few rarely used tools are left off the tool list, named by `load_tools`
instead, which hands their schemas over when asked; see
[hidden tools](tools/README.md#hidden-tools).

Whatever changes later reaches the model as a short notice at the start of the
next prompt instead:

- today's date and the git branch;
- plan mode turning on or off;
- skills, agent kinds and instruction files added, changed or removed on disk;
- the digest of saved memories.

For the same reason, tools are never taken off the list mid-conversation:
plan mode refuses calls instead, and `skill` and `recall` are listed from the
first turn even before they have anything to do. See
[prompt cache](long-conversations.md#prompt-cache).

## Goals

A reader who sends `/goal <objective>` sets a goal: crab starts each next turn
itself, opening every turn with a reminder of the objective, until the agent
ends it with the `goal` tool, two turns in a row make no progress, its token
budget runs out, or the reader pauses it or stops a turn. See
[goals](goals.md).

## Context management

When a conversation nears the model's context window, `crab` first leaves old
tool outputs out of what it sends, then replaces large old outputs and code
blocks with short notes, and only then compacts: it asks the model for a
summary of the older messages and keeps the recent ones as they are. Nothing
is deleted. The whole conversation stays saved, and the model can read any of
it again with `recall`. A reader can compact on demand by sending `/compact`.

The agent manages its own context too. `context_notes` keeps a notebook that
is saved with the conversation, and `new_context` starts a fresh context
window holding only that notebook. With the `checkpoints` setting on,
`checkpoint` and `rewind` replace a long investigation with a short report.
See [long conversations](long-conversations.md). [Memory](memory.md) carries
facts across conversations.

## Project trust

A freshly cloned repository could steer the agent, so what a project folder
brings is not used until the reader trusts the project: its hooks, its
instruction files, its skills and agent definitions, and the Claude Code
plugins its settings turn on. When something is left out, the client gets a
`Trust` event, and answers it with `Always` (trust for good), `Allow` (for
this conversation only) or `Reject`. Trust is recorded per repository, shared
by all its worktrees. The user's own folders are always used. See
[project trust](extending/project-trust.md).
