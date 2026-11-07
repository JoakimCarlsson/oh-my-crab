# Tools

Every tool the agent can have, grouped by what it is for. `crab tools`
prints the built-in ones with the first sentence of each description; the
tools marked *sometimes* below appear only in some conversations and are not
in that list. How calls are checked, approved and cut is in
[how tool calls run](calls.md).

The columns:

- **Asks** says whether a call asks the reader first outside yolo mode, and
  what an "always allow" answer covers. See
  [tool calls and approvals](../concepts.md#tool-calls-and-approvals).
- **Plan** says whether plan mode allows the tool. Plan mode keeps every tool
  on the list but refuses calls to the others without running them. See
  [modes](../concepts.md#modes).

## Files

| Tool       | What it does                                                                                     | Asks           | Plan | Detail                    |
| ---------- | ------------------------------------------------------------------------------------------------ | -------------- | ---- | ------------------------- |
| `read`     | Reads a file or folder; outlines of long code files; images, PDFs, SQLite, archives and JSON queries | no, for files  | yes  | [files](files.md)         |
| `edit`     | Changes files by line number against the tag of the last read, and resolves merge conflicts      | yes, the tool  | no   | [files](files.md)         |
| `write`    | Creates a file or overwrites it whole                                                            | yes, the tool  | no   | [files](files.md)         |
| `ast_edit` | Rewrites code by syntax tree across files                                                        | yes, the tool  | no   | [files](files.md#generated-files) |

After `edit` and `write`, the file is formatted and its diagnostics are added
to the result; see [language services](../language-services.md).

## Search

| Tool       | What it does                                                         | Asks | Plan |
| ---------- | -------------------------------------------------------------------- | ---- | ---- |
| `grep`     | Searches file contents with a regular expression                     | no   | yes  |
| `glob`     | Finds files by glob pattern                                          | no   | yes  |
| `ast_grep` | Searches code by syntax tree, ignoring formatting and comments       | no   | yes  |

## Shell

| Tool   | What it does                                                                                       | Asks          | Plan | Detail             |
| ------ | -------------------------------------------------------------------------------------------------- | ------------- | ---- | ------------------ |
| `bash` | Runs a shell command in the working folder; condenses noisy output; `background: true` returns a handle at once | yes, the tool | no   | [calls](calls.md)  |
| `wait` | Blocks until the next background `bash` command or `task` subagent ends, and returns its result    | no            | yes  | [background runs](../concepts.md#background-runs) |

## Web

| Tool         | What it does                                                                      | Asks              | Plan | Detail                                  |
| ------------ | --------------------------------------------------------------------------------- | ----------------- | ---- | --------------------------------------- |
| `read`       | Given a URL, `pr://`, `issue://` or `gh://` path, reads a web page or GitHub      | yes, per site     | yes  | [web and GitHub](web-and-github.md)     |
| `web_search` | Sends one query to a search service and returns ranked results                    | yes, all searches | yes  | [web and GitHub](web-and-github.md)     |

## Debug

| Tool    | What it does                                                                    | Asks                        | Plan | Detail              |
| ------- | ------------------------------------------------------------------------------- | --------------------------- | ---- | ------------------- |
| `debug` | Drives lldb-dap, gdb, Delve or debugpy over the Debug Adapter Protocol          | when launching or attaching | no   | [debugging](debug.md) |

## Script

| Tool     | What it does                                                                                        | Asks                     | Plan                        | Detail               |
| -------- | --------------------------------------------------------------------------------------------------- | ------------------------ | --------------------------- | -------------------- |
| `script` | Runs a short JavaScript program that calls the other tools; only what it prints and returns comes back | each call, as if direct | yes, calling only what plan mode allows | [scripts](script.md) |

## Subagents

| Tool            | What it does                                                                                 | Asks | Plan                       | Detail                     |
| --------------- | -------------------------------------------------------------------------------------------- | ---- | -------------------------- | -------------------------- |
| `task`          | Runs up to 16 subagents on self-contained assignments, optionally in worktrees or the background | no   | yes, with read-only subagents | [subagents](subagents.md) |
| `submit_result` | *Sometimes.* Hands a subagent's typed result back, checked against the call's `output_schema` | no   | yes                        | [subagents](subagents.md) |

`submit_result` is given only to a subagent asked for a typed result.

## Memory

| Tool            | What it does                                                         | Asks | Plan | Detail             |
| --------------- | -------------------------------------------------------------------- | ---- | ---- | ------------------ |
| `remember`      | Saves one durable fact or lesson for later conversations             | no   | yes  | [memory](../memory.md) |
| `memory_search` | Searches the saved memories of the project and the user              | no   | yes  | [memory](../memory.md) |
| `memory_edit`   | Corrects or forgets a memory by id                                   | no   | yes  | [memory](../memory.md) |

None of the three exists with the `memory` setting off or `OMC_MEMORY=0`.

## Conversation control

| Tool             | What it does                                                                                  | Asks | Plan | Detail |
| ---------------- | --------------------------------------------------------------------------------------------- | ---- | ---- | ------ |
| `ask`            | Asks the reader a form of questions and waits for the answers                                 | no   | yes  | [`Inquire`](../wire-protocol.md#inquire) |
| `todo`           | Keeps the todo list the reader sees, sent to the client as `Plan`                              | no   | yes  | [troubleshooting](../troubleshooting.md) |
| `set_title`      | *Sometimes.* Names the conversation; given only while it has no name                          | no   | yes  | [history](../concepts.md#history-saved-and-reopened-by-id) |
| `exit_plan_mode` | Hands the plan to the reader for approval and ends plan mode when approved; refused outside it | no   | only | [modes](../concepts.md#modes) |
| `skill`          | Loads a skill's instructions; listed even when there are no skills                             | no   | yes  | [skills](../extending/skills-and-agents.md#skills) |
| `context_notes`  | Reads or replaces the conversation's notebook, which is saved with it                          | no   | yes  | [context management](../concepts.md#context-management) |
| `new_context`    | Starts a fresh context window holding only the notebook                                       | no   | yes  | [context management](../concepts.md#context-management) |
| `checkpoint`     | *Sometimes.* Marks a point before a long investigation                                        | no   | yes  | [context management](../concepts.md#context-management) |
| `rewind`         | *Sometimes.* Replaces everything since the checkpoint with a report; files are not restored   | no   | yes  | [context management](../concepts.md#context-management) |
| `recall`         | *Sometimes.* Reads back tool output or messages that pruning or compaction left out           | no   | yes  | [long conversations](../long-conversations.md) |

`checkpoint` and `rewind` exist with the `checkpoints` setting on. `recall`
is on the main agent's list from the first turn, so that the list never
changes, and answers once something has been left out.

## Tool-server tools

Tools of the [tool servers](../extending/mcp.md) the client passes in `Open`
are added beside these. Every call asks, the scope being that one tool, and
plan mode refuses them.

## What subagents and scripts do not get

A client can deny any tool by name in `Open`; a denied tool is given to no
one, subagents and scripts included.

**Subagents** never get `task`, `exit_plan_mode`, `ask`, `todo`,
`context_notes`, `new_context`, `checkpoint`, `rewind`, `wait`, `debug`,
`set_title`, `remember` or `memory_edit`: the tools that start subagents,
talk to the reader, steer the conversation, change memory, or hold state that
belongs to the conversation. Their `bash` cannot start background runs. A
kind whose definition lists tools gets only those. In plan mode subagents get
only the tools plan mode allows.

**Scripts** never get `script` itself, `task`, `ask`, `exit_plan_mode`,
`todo`, `context_notes`, `new_context`, `checkpoint`, `rewind`, `wait`,
`set_title`, `submit_result`, `recall`, `skill`, `remember` or
`memory_edit`. A subagent's script calls only that subagent's tools. See
[scripts](script.md).
