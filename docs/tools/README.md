# Tools

Every tool the agent can have, grouped by what it is for. `crab tools`
prints the built-in ones with the first sentence of each description; the
tools marked *sometimes* below appear only in some conversations and are not
in that list. How calls are checked, approved and cut is in
[how tool calls run](calls.md).

The columns:

- **Asks** says whether a call asks the reader first outside yolo mode, and
  what an "always allow" answer covers. See
  [tool calls and approvals](../concepts.md#tool-calls-and-approvals) and
  [permission rules](../extending/permissions.md), which can also stop or
  allow calls of any tool.
- **Plan** says whether plan mode allows the tool. Plan mode keeps every tool
  on the list but refuses calls to the others without running them. See
  [modes](../concepts.md#modes).

A few rarely used tools are hidden: they are not on the tool list sent with
each request, but `load_tools` names them, each with a one-line summary. See
[hidden tools](#hidden-tools).

## Files

| Tool       | What it does                                                                                     | Asks           | Plan | Detail                    |
| ---------- | ------------------------------------------------------------------------------------------------ | -------------- | ---- | ------------------------- |
| `read`     | Reads a file or folder; outlines of long code files; images, PDFs, SQLite, archives and JSON queries | no, for files  | yes  | [files](files.md)         |
| `edit`     | Changes files by line number against the tag of the last read, and resolves merge conflicts      | yes, per folder | no   | [files](files.md)         |
| `write`    | Creates a file or overwrites it whole                                                            | yes, per folder | no   | [files](files.md)         |
| `ast_edit` | Rewrites code by syntax tree across files                                                        | yes, per folder | no   | [files](files.md#generated-files) |

After `edit` and `write`, the file is formatted and its diagnostics are added
to the result; see [language services](../language-services.md).

## Search

| Tool       | What it does                                                         | Asks | Plan |
| ---------- | -------------------------------------------------------------------- | ---- | ---- |
| `grep`     | Searches file contents with a regular expression                     | no   | yes  |
| `glob`     | Finds files by glob pattern                                          | no   | yes  |
| `ast_grep` | Searches code by syntax tree, ignoring formatting and comments       | no   | yes  |

## Code intelligence

| Tool  | What it does                                                                                                   | Asks                                 | Plan                           | Detail                                                  |
| ----- | -------------------------------------------------------------------------------------------------------------- | ------------------------------------ | ------------------------------ | ------------------------------------------------------- |
| `lsp` | Asks a file's language server for definitions, references, implementations, hovers, symbols, diagnostics and code actions, renames a symbol across files, applies code actions, formats files, moves a file with its imports, and shows and restarts the servers | for changes, per action (`lsp rename`, `lsp code_actions`, `lsp format`, `lsp rename_file`) | yes, but changes only as dry runs | [language services](../language-services.md#the-lsp-tool) |

## Shell

| Tool   | What it does                                                                                       | Asks          | Plan | Detail             |
| ------ | -------------------------------------------------------------------------------------------------- | ------------- | ---- | ------------------ |
| `bash` | Runs a shell command in the working folder; condenses noisy output; `background: true` returns a handle at once | yes, per command | no   | [calls](calls.md)  |
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
| `task`          | Runs up to 32 subagents on self-contained assignments, optionally in worktrees or the background | no   | yes, with read-only subagents | [subagents](subagents.md) |
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
| `exit_plan_mode` | Hands the plan to the reader for approval and ends plan mode when approved, in the same context or a fresh one; saves every plan; refused outside it | no   | only | [plans](../concepts.md#plans) |
| `skill`          | Loads a skill's instructions; listed even when there are no skills                             | no   | yes  | [skills](../extending/skills-and-agents.md#skills) |
| `load_tools`     | *Sometimes.* Hands over the description and parameters of hidden tools so they can be called   | no   | yes  | [hidden tools](#hidden-tools) |
| `context_notes`  | Reads or replaces the conversation's notebook, which is saved with it                          | no   | yes  | [context management](../concepts.md#context-management) |
| `new_context`    | Starts a fresh context window holding only the notebook                                       | no   | yes  | [context management](../concepts.md#context-management) |
| `checkpoint`     | *Sometimes.* Marks a point before a long investigation                                        | no   | yes  | [context management](../concepts.md#context-management) |
| `rewind`         | *Sometimes.* Replaces everything since the checkpoint with a report; files are not restored   | no   | yes  | [context management](../concepts.md#context-management) |
| `recall`         | *Sometimes.* Reads back tool output or messages that pruning or compaction left out           | no   | yes  | [long conversations](../long-conversations.md) |

`checkpoint` and `rewind` exist with the `checkpoints` setting on. `recall`
is on the main agent's list from the first turn, so that the list never
changes, and answers once something has been left out.

## Hidden tools

Some tools are hidden from the tool list to keep it, and the prompt, short.
A hidden tool is still the agent's: `load_tools` names every hidden tool with
the first sentence of its description, and when the model calls it with the
names it wants, it returns their whole descriptions and parameter schemas.
The model then calls them by name like any other tool. Their calls are checked
against the schema, put to the permission rules, asked about, shown to the
hooks and scheduled exactly as calls of listed tools are; a call made without
loading the tool first runs too, as long as its arguments fit.

Hiding never changes the tool list in the middle of a conversation, so it
keeps the [prompt cache](../long-conversations.md#prompt-cache): the hidden
tools, and `load_tools` with its list, are fixed when the tool list is made,
on the first turn, and loading a tool only adds a tool result to the
conversation. `load_tools` exists only when some tool is hidden.

By default these are hidden:

| Tool            | Why                                                        |
| --------------- | ---------------------------------------------------------- |
| `ast_edit`      | Rewrites by syntax tree are rare; `edit` covers most edits |
| `ast_grep`      | Searches by syntax tree are rare; `grep` covers most       |
| `debug`         | Driving a debugger is rare, and its description is long    |
| `mcp_resources` | Tool-server resources are seldom needed                    |
| `memory_edit`   | Correcting a memory is rare                                |

`OMC_DEFERRED_TOOLS` changes the set: a comma-separated list of tool names and
of name prefixes ending in `*`, with `default` standing for the set above.
`OMC_DEFERRED_TOOLS=default,mcp__*` hides every tool-server tool as well,
`OMC_DEFERRED_TOOLS=debug,web_search` hides only those two, and `0` (or an
empty value, `none`, `off`) hides nothing. `load_tools`, `exit_plan_mode` and
`submit_result` are never hidden. See [configuration](../configuration.md).

Subagents hide the same tools among the ones they get, with a `load_tools` of
their own when any is left. A script's description gives the signatures of
listed tools only and names the hidden ones, which a script can call once the
model has loaded them. `crab tools` lists hidden tools with the rest, and
`load_tools` when some are hidden.

## Tool-server tools

Tools of the [tool servers](../extending/mcp.md), the ones the client passes
in `Open` and the ones set up in files, are added beside these as
`mcp__<server>__<tool>`. Every call asks, the scope being that one tool, and
plan mode refuses them.

When a server offers resources, `mcp_resources` comes with them: without a
`uri` it lists the resources and templates of every server, or of `server`;
with one it reads that resource. It only reads, so it never asks and plan
mode allows it.

## What subagents and scripts do not get

A client can deny any tool by name in `Open`; a denied tool is given to no
one, subagents and scripts included.

**Subagents** never get `exit_plan_mode`, `ask`, `todo`, `context_notes`,
`new_context`, `checkpoint`, `rewind`, `wait`, `debug`, `set_title`,
`remember` or `memory_edit`: the tools that talk to the reader, steer the
conversation, change memory, or hold state that belongs to the conversation.
They get a `task` tool of their own, which cannot start background runs, only
above the deepest level of subagents allowed (see
[nesting](subagents.md#nesting)). Their `bash` cannot start background runs. A
kind whose definition lists tools gets only those. In plan mode subagents get
only the tools plan mode allows. Subagents and scripts do get `lsp`; in plan
mode it is the copy that refuses changes other than dry runs.

**Scripts** never get `script` itself, `task`, `ask`, `exit_plan_mode`,
`todo`, `context_notes`, `new_context`, `checkpoint`, `rewind`, `wait`,
`set_title`, `submit_result`, `recall`, `skill`, `remember`,
`memory_edit` or `load_tools`. A subagent's script calls only that subagent's tools. See
[scripts](script.md).
