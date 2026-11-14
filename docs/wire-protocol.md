# Wire protocol

`crab serve` holds one conversation with the program that started it. The
client writes commands to crab's standard input and reads events from its
standard output. This page is the contract between the two. The types are
defined in `crates/omc-wire`; a Rust client can depend on that crate, and
`crates/omc-client` is a small blocking client that starts `crab` and talks to
it.

The example lines on this page were written by crab itself: some are taken
from a real `crab serve` session, the others were produced by
`omc_wire::encode` from values of the real types. Long lines are shortened
only where marked with `...`.

## Framing

- Every message is one JSON object, or one JSON string, on a line of its own,
  ended by `\n`. A message never spans lines: newlines inside strings are
  escaped as `\n`. Text is UTF-8.
- Crab flushes standard output after every event.
- Crab trims trailing whitespace from each line before reading it. A line that
  is not a valid command is answered with a `Failed` event whose text starts
  with `bad command:`, and the conversation goes on.
- Crab writes diagnostics meant for a person (a tool server it cannot reach,
  a history file it cannot save) to standard error, never to standard output.
  A client should drain standard error so crab never blocks on it.

### How the types are encoded

The types are encoded with serde's defaults, with a few exceptions noted
below.

- **Enums** are externally tagged. A variant with no data is a bare JSON
  string of its name: `"Cancel"`, `"Halted"`. A variant with one value is an
  object with the variant name as its only key: `{"Said":"hi"}`. A variant
  with named fields is an object whose only key is the variant name and whose
  value holds the fields: `{"Rename":{"title":"x"}}`.
- **Variant names** are PascalCase as written in Rust (`Open`, `Running`,
  `Always`), except in these types, whose names are snake_case or lowercase:
  `LanguageAsk` (`format`, `diagnostics`, `definition`, `type_definition`,
  `implementation`, `references`, `hover`, `document_symbols`,
  `workspace_symbols`, `workspace_diagnostics`, `rename`, `format_range`,
  `code_actions`, `apply_code_action`, `will_rename_file`, `file_renamed`,
  `status`, `reload`), `LanguageAnswer` (`formatted`, `diagnostics`,
  `locations`, `hover`, `symbols`, `edits`, `unsupported`, `no_server`,
  `failed`, `actions`, `applied`, `servers`, `done`), `Severity` (`error`, `warning`, `information`, `hint`),
  `SignIn` (`browser`, `device_code`, `api_key`, `none`), `ServerWire`
  (`chat_completions`, `responses`) and `KeySource`,
  which is internally tagged by a `from` field.
- **Field names** are snake_case as written in Rust.
- **Optional fields** (marked "optional" in the tables below) are written as
  `null` when empty; crab always writes them. A message sent to crab may
  leave them out, which is the same as `null`.
- **Defaulted fields** (marked "default") may be left out when sent to crab:
  a list defaults to `[]`, a string to `""`, a boolean to `false`. Crab still
  writes them in what it sends, except `key_env` and `credentials` in
  `ProviderInfo`, which are left out when empty, and `server` and `models`
  in `ProviderInfo`, left out when false or empty.
- **Unknown fields** in a message sent to crab are ignored, so a client may
  set a field a newer crab reads, such as `navigation` in `Open`, and an
  older crab goes on as if it were not there.
- **Paths** are JSON strings.
- **Pairs** (`(String, String)`) are two-element arrays: `["effort","high"]`.
- **Integers** are unsigned: tickets and token counts are `u64`.

Fields not marked optional or default are required; a command that leaves one
out is refused as a bad command.

## Lifecycle

1. The client starts `crab serve`.
2. The client sends `Open`. Crab answers with `Settings` and `Commands`, then,
   for a conversation opened with an id, `Titled` when it has a name,
   `Resumed` with its saved turns and `Spent` when it has cost something, then
   `Trust` when the project brings something that is left out until it is
   trusted. When opening fails, crab sends `Failed` or `SignedOut` instead, and
   the client can send `Open` again.
3. The client sends `Prompt`. Crab streams the turn as events: `Started`,
   `Said`, `Thought`, `Tool`, `Used`, `Spent`, `Plan`, `Background`, and requests that
   wait on the client (`Ask`, `Inquire`, `Language`). The turn ends with
   exactly one `Finished`, or with `SignedOut` when the account needs signing
   in again.
4. Between turns, and during them, the client may send `Set`, `Rename`,
   `Login`, `Switch` and more prompts. A prompt sent during a turn is queued (see
   [Queued prompts](#queued-prompts)).
5. `Cancel` stops the running turn: crab sends `Halted` and then
   `Finished` with `null`.
6. `Close` ends the conversation, and the `crab` process exits with status 0
   as soon as the conversation's `SessionEnd` [hooks](extending/hooks.md), if
   any, have ended. Closing standard input does the same. A turn still running is
   stopped without a `Halted` or `Finished`; background commands and debugging
   sessions are stopped too. Events already queued for output may still be
   written before the process exits.

A complete session, from a real run (the `Settings`, `Commands` and `Resumed`
lines are shortened):

```json
{"Open":{"directory":"/home/me/project","provider":"claude","model":null,"yolo":false,"conversation":"docs-1","history":"/home/me/hist"}}
{"Settings":[{"id":"model","category":"model","name":"Model","current":"claude-haiku-5-5","picks":[...]},...]}
{"Commands":[{"name":"compact","description":"Summarize the older part of the conversation to free up context; text after the command says what the summary should keep in particular"},...]}
{"Resumed":{"title":null,"settings":[],"turns":[],"offered":[...],"commands":[...],"window":1000000}}
{"Prompt":{"text":"Read calc.py and fix the bug in add. Be brief.","files":[],"turn":"t-1"}}
{"Used":{"used":22927,"size":1000000}}
{"Tool":{"id":"toolu_01BJncUS5JG4uPGyhLqWQqzL","title":"read","kind":"Read","state":"Running","target":"calc.py","diff":null,"output":null,"parent":null,"agent":null,"worktree":null}}
{"Tool":{"id":"toolu_01BJncUS5JG4uPGyhLqWQqzL","title":null,"kind":null,"state":"Done","target":null,"diff":null,"output":"[tag 5906]\n     1\tdef add(a, b):\n     2\t    return a - b\n","parent":null,"agent":null,"worktree":null}}
{"Used":{"used":23009,"size":1000000}}
{"Tool":{"id":"toolu_01TuYTkx2rQq1MVbexcwgAye","title":"edit","kind":"Edit","state":"Running","target":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","diff":null,"output":null,"parent":null,"agent":null,"worktree":null}}
{"Ask":{"ticket":1,"title":"edit: calc.py","command":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow Edit(/**)","verdict":"Always"},{"id":"always-project","name":"Always allow Edit(/**) in this project","verdict":"Always"},{"id":"always-user","name":"Always allow Edit(//home/me/calc/**) in every project","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
{"Choose":{"ticket":1,"option":"allow"}}
{"Tool":{"id":"toolu_01TuYTkx2rQq1MVbexcwgAye","title":null,"kind":null,"state":"Done","target":null,"diff":{"path":"calc.py","old":"def add(a, b):\n    return a - b\n","new":"def add(a, b):\n    return a + b\n"},"output":"calc.py: tag D960\n     1\tdef add(a, b):\n     2\t    return a + b","parent":null,"agent":null,"worktree":null}}
{"Started":"Text"}
{"Said":"Fixed `"}
{"Said":"add"}
{"Said":"` in `calc.py`: it returned `a - b`, and now returns `a + b`."}
{"Used":{"used":23141,"size":1000000}}
{"Finished":null}
"Close"
```

The commands sent to crab and the events it sends back are interleaved here in
the order they happened.

## Behaviours

### Tickets

Every request that waits on the client carries a `ticket`, and the client
answers under the same ticket:

| Request event | Answer command | What a missing answer does                     |
| ------------- | -------------- | ---------------------------------------------- |
| `Ask`         | `Choose`       | The tool call waits until the turn is stopped. |
| `Inquire`     | `Answer`       | The `ask` tool waits until the turn is stopped.|
| `Language`    | `Language`     | Crab gives up after the wait it asked for.     |
| `Trust`       | `Trust`        | The project stays untrusted.                   |

Tickets are numbered from 1 and count up separately for each family: `Ask` and
`Inquire` share one counter, `Language`, `Trust` and `Queued` each have their
own. A ticket is only meaningful together with the command that answers it.
An answer under a ticket crab no longer waits on is ignored without an event.

`Cancel`, and an `Open` that cancels a running turn, decline every `Ask` and
abandon every `Inquire` still waiting.

### Queued prompts

A prompt sent while a turn is running is not dropped. It is queued, and crab
reports it with a `Queued` event in the `Waiting` state, under a ticket that
every later event about it repeats. Queued prompts are numbered from 1 for the
life of the process.

- At the next safe point of the running turn, after its current batch of tool
  calls and any compaction, and before the next model request, crab puts the
  waiting prompts into the conversation in the order they came, so the model
  reads them mid-turn. Each is reported as `Steered` and saved as a turn of its
  own under the `turn` id it was sent with.
- When the turn ends with the model's final answer before a waiting prompt
  could be put in, the oldest waiting prompt starts the next turn by itself and
  is reported as `Started`; the others wait for that turn. When that turn
  cannot start, the prompt is reported as `Dropped` after its `Started`, and so
  is every other one still waiting.
- When the turn is cancelled, fails or needs signing in again, the prompts
  still waiting are never sent to the model: each is reported as `Dropped` so
  the client can offer it again. Prompts already steered into a cancelled or
  failed turn are saved with that turn as stopped or failed.
- When the project has a `UserPromptSubmit` hook, prompts are not steered into
  a running turn, so that the hook sees each one; they wait and start turns of
  their own.
- A `/compact` sent while a turn is running is refused with a `Failed` event
  and is not queued.

From a real run, a prompt sent while a `bash` call was running, then `Cancel`:

```json
{"Prompt":{"text":"Also add a docstring.","files":[],"turn":"t-3"}}
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Waiting"}}
"Cancel"
"Halted"
{"Finished":null}
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Dropped"}}
```

### Settings round-trips

After `Open`, crab sends the whole list of settings as `Settings`. The client
changes one with `Set`, naming the setting's `id` and one of its picks'
`value`. When the setting takes the value, crab saves the choice with the
conversation and sends the whole `Settings` list again with the new
`current`; the new value applies from the next model request. A `Set` for an
unknown setting or a value the setting does not take is ignored, and no event
is sent. Crab also sends `Settings` again when a setting changes on its own,
for example when the mode leaves plan mode after a plan is approved, or when
the model changes and so do the effort levels it offers.

The settings crab offers today:

| `id`            | `category`      | Values                                    | Offered when                               |
| --------------- | --------------- | ----------------------------------------- | ------------------------------------------ |
| `model`         | `model`         | the provider's models                     | always                                     |
| `mode`          | `mode`          | `default`, `yolo`, `plan`                 | always                                     |
| `effort`        | `thought_level` | the model's levels, such as `low`...`max` | the model takes an effort                  |
| `cache`         | `cache`         | `5m`, `1h`                                | the provider offers a cache lifetime       |
| `cache_warming` | `cache_warming` | `on`, `off`                               | the provider's cache can be kept warm      |
| `memory`        | `memory`        | `on`, `off`                               | memory is not turned off for the process   |
| `plan_model`    | `plan_model`    | `same`, the provider's models, or any model as `model` or `provider/model` | always            |
| `plan_autosave` | `plan_autosave` | `on`, `off`                               | always                                     |
| `snapshots`     | `snapshots`     | `on`, `off`                               | always                                     |
| `checkpoints`   | `checkpoints`   | `on`, `off`                               | always                                     |

A client should not hard-code this list: show what `Settings` offers. `yolo`
in `Open` picks the starting mode (`yolo` or `default`). Saved choices are
applied again when a conversation with an id is opened; `defaults` in `Open`
gives values for the settings the saved conversation has no choice for, and a
`model` given in `Open` wins over a saved model.

### History replay

When `Open` carries a `conversation` id, crab keeps the conversation's
history in a file named after the id (in `history`, or under `oh-my-crab/conversations`
in the user's data folder) and loads it again the next time the same id is
opened. An id may hold only ASCII letters, digits, `-`, `_` and `.`, must not
start with `.`, and must not be a Windows device name; any other id fails the
`Open` with `Failed`.

On opening such a conversation crab sends `Resumed`, even when nothing was
saved yet, in which case its `turns` is empty. Each saved turn comes back as a
`PastTurn`: the prompt as the client sent it, the `turn` id, the attached file
names, and the events a live conversation would have sent for it. A replayed
turn differs from the live one in a few ways:

- `Said` and `Thought` come back as one chunk per reply, not as the chunks
  that were streamed.
- A turn that answered has no `Finished` event; a turn that was stopped ends
  with `Halted` and `Finished` with `null`, and one that failed ends with
  `Finished` carrying the reason.
- `Used` follows the tool calls of the reply it measured, instead of coming
  before them.
- A compaction shows as a finished `compact` call whose output is its summary.

`keep` limits how many turns are loaded. `before` goes back to before the
turn with that id: the conversation goes on from there on a new branch, and
`Resumed` carries the turns before it. A `before` id that names no turn of the
branch fails the `Open`.

### Branches

A saved conversation is a tree of branches. Going back with `before` does not
delete anything: the turn named and the turns after it stay in the file, on
the branch they were on, and the conversation goes on on a new branch that
holds the turns before that point. Its first prompt starts the new line.

- Crab names branches itself: `1` is the branch a conversation starts on, and
  each new branch takes the next number. A history saved before crab
  recorded branches loads as branch `1`.
- `Resumed` says which branch its `turns` are of in `branch`, and lists every
  branch in `branches`, each with the branch it went back from (`parent`), the
  turn of that branch it went back to before (`before`) and all its turns,
  the ones it shares with its parent included. That is enough to draw the
  tree: two branches share their turns up to `before`.
- `Switch` moves the conversation to another branch, at its tip, and crab
  answers with `Resumed` for that branch. The model carries on from that
  branch's turns. The branch a conversation is on is saved, so opening it
  again later comes back on the same branch.
- Going back again before any prompt was sent on a branch made by going back
  moves that branch's starting point instead of making another branch, so
  that stepping back several turns one at a time leaves one branch.
- `crab history` prints the branches too, and `crab fork` copies the branch a
  conversation is on, up to a turn, into a new conversation of its own; see
  [commands](usage.md).

From a real run: three turns (`t-1` to `t-3`) were saved, then the
conversation was opened again with `"before":"t-2"`, prompted on the new
branch, opened again, and switched back to branch `1` (lines shortened):

```json
{"Open":{"directory":"/home/me/project","provider":"claude","model":"claude-haiku-5-5","yolo":true,"conversation":"br-1","before":"t-2"}}
{"Resumed":{"title":null,...,"turns":[{"id":"t-1","prompt":"Remember the colour red. Reply with only: ok",...}],...,"branch":"2","branches":[{"id":"1","parent":null,"before":null,"turns":[{"id":"t-1",...},{"id":"t-2",...},{"id":"t-3",...}]},{"id":"2","parent":"1","before":"t-2","turns":[{"id":"t-1",...}]}]}}
{"Prompt":{"text":"Now also remember blue. Which colours did I ask you to remember? One line.","files":[],"turn":"t-4"}}
{"Said":"Red and blue."}
{"Finished":null}
"Close"
{"Open":{"directory":"/home/me/project","provider":"claude","model":"claude-haiku-5-5","yolo":true,"conversation":"br-1"}}
{"Resumed":{...,"branch":"2",...}}
{"Switch":{"branch":"1"}}
{"Resumed":{...,"turns":[{"id":"t-1",...},{"id":"t-2",...},{"id":"t-3",...}],...,"branch":"1",...}}
{"Prompt":{"text":"Which colours did I ask you to remember? One line.","files":[],"turn":"t-5"}}
{"Said":"Red and green."}
{"Finished":null}
{"Switch":{"branch":"7"}}
{"Failed":"no branch has the id 7"}
```

In the file, going back is a `branch` record followed by the cut-back, and
switching is a `switch` record; the turns of every branch stay where they
were written.

### Files of a turn

In a git repository, with the `snapshots` setting on, crab keeps the files of
the folder it works in before every turn that has an id and again when the
turn ends, however it ends. Every file the repository does not ignore is kept,
tracked or not, so what a shell command, a formatter or a generator wrote is
kept along with the agent's own edits, byte for byte: the reader's
`core.autocrlf` is not applied, so putting files back never changes their line
endings. Neither the branch, the head nor the
index changes: the files are written as commits no branch holds, under
`refs/worktree/oh-my-crab/<conversation>/before/<turn>` and
`.../after/<turn>`, with every byte of an id that is not a letter, a digit or
`-` written as `_` and two hexadecimal digits. `<conversation>` is the
conversation's id followed by `-` and a hash of the path of its history file,
so conversations saved under one id in different folders stay apart. A
conversation without an id keeps them under `unsaved-<process id>`.

- `Changes` asks what a turn changed, and crab answers with a `Changes` event
  holding each file and its diff. A turn that ended is told from the files
  kept before and after it, so nothing done later is in it; the running turn
  is told against the files as they are now.
- `Undo` takes what a turn changed back out of some files or all of them,
  leaving what was done to them since. It changes nothing when lines the turn
  changed were changed again.
- `Restore` puts every file back as it was before a turn, deleting files made
  since.
- `Undo` and `Restore` keep the files as they were first, under
  `.../<conversation>/backup`, are refused while a turn runs, and leave the
  conversation as it is. The model is told what happened with the next
  prompt.
- A turn crab starts itself toward a goal is kept under the id its
  `Continued` event carries.
- A turn without an id, a `/compact`, and a folder that is in no git
  repository keep nothing, and the three commands then answer `Failed`.
- Every `Failed` these three commands answer with starts with `files:`, so a
  client can tell it from a failure of the conversation, which goes on.

```json
{"Prompt":{"text":"Rename add to sum.","files":[],"turn":"t-4"}}
{"Finished":null}
{"Changes":{"turn":"t-4"}}
{"Changes":{"turn":"t-4","open":false,"files":[{"path":"calc.py","from":null,"status":"M","diff":"@@ -1 +1 @@\n-def add(a, b):\n+def sum(a, b):","added":1,"removed":1,"truncated":false}]}}
{"Undo":{"turn":"t-4","paths":["calc.py"]}}
{"Undone":{"turn":"t-4","files":["calc.py"]}}
```

### Tool events are updates

A tool call is reported by several `Tool` events with the same `id`. The first
says what started (`title`, `kind`, `state` `Running`, `target`); later ones
carry only what changed (`state`, `output`, `diff`, `agent`, `worktree`) and
`null` for the rest. A client merges each event into the call with that id,
keeping the fields that are `null` in the update.

- When a finished call changed more than one file, the first change is the
  call's `diff`, and each other file gets an extra `Tool` event with the id
  `<id>:<n>` (`n` counting from 1), `title` `edit`, kind `Edit`, state `Done`
  and its own `diff`.
- The work of a subagent is reported as `Tool` events whose `id` is the
  subagent call's id, a `:` and the inner call's id, with `parent` set to the
  subagent call's id and `agent` to the subagent's kind. Nested subagents
  chain the ids: `a:b:c` has the parent `a:b`. A subagent's own `task` call
  is reported the same way, its `agent` saying what kind it starts.

### Other rules

- `Prompt` before a successful `Open` fails with `Failed` (`the conversation
  is not open`).
- `Open` sent while a conversation is open replaces it. A running turn is
  cancelled first, as `Cancel` does, with `Halted`, `Finished` and a `Dropped`
  event for every waiting prompt; background commands are stopped and
  debugging sessions disconnected. An `Open` naming an unknown provider fails
  before any of that, and the open conversation goes on.
- `Login` runs in the background; other commands work meanwhile.
- A `Rename` whose title is only whitespace is ignored; otherwise runs of
  whitespace are folded to one space. A name the client gives always wins over
  one the agent picks.

## Commands

Commands are what the client sends. `Command` has these variants.

### `Open`

Opens the conversation; it must come before anything else. Carries an
`Opening`:

| Field          | Type                       | Presence | Meaning                                                                                                                      |
| -------------- | -------------------------- | -------- | ---------------------------------------------------------------------------------------------------------------------------- |
| `directory`    | path                       | required | The folder the agent works in.                                                                                               |
| `home`         | path                       | optional | The folder the logins are kept in; crab's own when `null`.                                                                   |
| `provider`     | string                     | required | The provider's id, such as `claude`; see [supported providers](providers/supported.md) and `crab providers`.                 |
| `model`        | string                     | optional | The model to start with; the provider's first listed or default model when `null`.                                           |
| `yolo`         | bool                       | required | Whether tools that change things run without asking: mode `yolo` instead of `default`.                                       |
| `tools`        | list of `ToolServer`       | default  | Tool servers (MCP) whose tools the agent is given besides its own; they win over servers of the same name set up in files.   |
| `denied`       | list of string             | default  | Names of tools the agent is not given, whoever offers them.                                                                  |
| `disabled`     | list of string             | default  | Instruction files (by path), skills and agent definitions (by name) the reader turned off.                                   |
| `conversation` | string                     | optional | The id the history is saved under and loaded from; nothing is saved when `null`.                                             |
| `history`      | path                       | optional | The folder the history files are kept in.                                                                                    |
| `keep`         | integer                    | optional | How many of the saved turns to load; all when `null`.                                                                        |
| `defaults`     | list of pairs              | default  | `[setting id, value]` pairs for settings the saved conversation has no choice for.                                           |
| `before`       | string                     | optional | A turn id: the conversation goes back to before that turn, on a new branch; the turns left behind stay saved. See [Branches](#branches). |
| `languages`    | bool                       | default  | Whether the client answers `Language` requests; crab then starts no language servers of its own.                             |
| `navigation`   | bool                       | default  | Whether the client, which set `languages`, also answers the questions about code of the `lsp` tool; see [`Language`](#language-1). Means nothing without `languages`. |

The shortest valid `Open`:

```json
{"Open":{"directory":"/path/to/project","provider":"claude","yolo":false}}
```

With every field:

```json
{"Open":{"directory":"/home/me/project","home":null,"provider":"claude","model":"claude-sonnet-4-5","yolo":false,"tools":[{"name":"files","reach":{"Command":{"program":"mcp-files","args":["--root","."],"env":[["LOG","warn"]]}}},{"name":"issues","reach":{"Http":{"url":"https://example.com/mcp","headers":[["Authorization","Bearer x"]]}}}],"denied":["web_search"],"disabled":["AGENTS.md"],"conversation":"c-42","history":null,"keep":50,"defaults":[["effort","high"]],"before":null,"languages":true,"navigation":true}}
```

#### `ToolServer` and `Reach`

A `ToolServer` is one tool server, by the name its tools are filed under:

| Field   | Type    | Meaning                 |
| ------- | ------- | ----------------------- |
| `name`  | string  | What the server is called. |
| `reach` | `Reach` | How it is reached.      |

`Reach` is one of:

- `Command`: a program crab starts and speaks to on its standard streams.
  Fields `program` (string), `args` (list of string), `env` (list of pairs,
  added over crab's own environment). All three are required.
- `Http`: an HTTP address. Fields `url` (string) and `headers` (list of pairs
  sent with every request). Both are required.

A server crab cannot reach is reported on standard error and left out; the
conversation opens without it. Crab also reads tool servers from files, the
project's once it is trusted; a server passed here wins over one of the same
name there. See [tool servers](extending/mcp.md).

### `Prompt`

Starts a turn with these words and files, or queues them when a turn is
running.

| Field     | Type           | Presence | Meaning                                                                                                     |
| --------- | -------------- | -------- | ----------------------------------------------------------------------------------------------------------- |
| `text`    | string         | required | What the reader wrote. `/compact`, `/goal`, `/guided-goal`, `/retry`, `/fresh`, `/btw`, `/handoff` and `/<skill>` are understood here. |
| `files`   | list of path   | required | Files attached, by path.                                                                                    |
| `turn`    | string         | optional | The id the client knows the turn by; saved with the turn so the client can go back to before it.            |
| `context` | string         | optional | Words the model is told before the reader's, which are not saved as what the reader wrote.                  |

```json
{"Prompt":{"text":"Fix the failing test","files":["/home/me/project/log.txt"],"turn":"t-7","context":null}}
```

`/compact`, optionally followed by what the summary should keep, compacts the
conversation instead of starting a normal turn; it takes no files and is
refused while a turn runs.

`/goal` sets, shows and changes the conversation's goal; see
[goals](goals.md). `/goal <objective>` sets one, with an optional budget
first (`/goal 500k <objective>`); `/goal` alone shows it; `/goal pause`,
`/goal resume` and `/goal clear` change how it stands; `/goal budget <N|off>`
changes its budget. Each sends a `Goal` event when the goal changed or is
shown. Setting and resuming start a turn toward the goal, under the `turn` id
sent, which ends with `Finished` as any turn does. The others start no turn:
sent between turns, they are answered as a short turn of their own, a `Said`
with a few words on the goal, or on why the command cannot be done, then
`Finished` (`null`, or the reason when it cannot be done), so that every
prompt sent between turns ends with `Finished`; sent while a turn runs, they
are carried out at once, with no `Finished`, and one that cannot be done is
answered with `Failed`. Setting and resuming are refused while a turn runs.

`/guided-goal`, optionally followed by an idea, starts a turn under the `turn`
id sent in which the agent asks the reader what the goal should be and then
sets it, which sends a `Goal` event; it is refused while a turn runs.

`/retry`, `/fresh`, `/btw <question>` and `/handoff [goal]` do what the
commands `Retry`, `Fresh`, `Btw` and `Handoff` do; each takes no files. Sent
as a prompt, `/fresh` ends with `Finished`, `/retry` and `/handoff` end with
the `Finished` of the turn they start, and `/btw` is answered with `Aside`
alone.

### `Choose`

Answers an `Ask` under `ticket`.

| Field    | Type    | Presence | Meaning                                                    |
| -------- | ------- | -------- | ---------------------------------------------------------- |
| `ticket` | integer | required | The `Ask` being answered.                                  |
| `option` | string  | optional | The `id` of the `Choice` picked; `null` declines.          |
| `note`   | string  | optional | What the reader wrote along with the choice. Only a plan's choices pass it on to the agent; it is ignored for a tool call. |

`allow` lets the call run once. `always` lets it run and adds the permission
rules the `Ask` named for the rest of the conversation; `always-project` and
`always-user` do that and keep the rules in crab's permissions file, for the
project or for every project, and are accepted only when the `Ask` offered
them. Any other `option` declines, as `null` does. A client that only knows
`allow`, `always` and `reject` keeps working as before; the new choices are
just more entries in `choices`. See
[permission rules](extending/permissions.md).

```json
{"Choose":{"ticket":1,"option":"allow"}}
{"Choose":{"ticket":1,"option":null}}
{"Choose":{"ticket":3,"option":"reject","note":"Split the second step in two."}}
```

### `Answer`

Answers an `Inquire` form under `ticket`.

| Field     | Type            | Presence | Meaning                                                         |
| --------- | --------------- | -------- | --------------------------------------------------------------- |
| `ticket`  | integer         | required | The form being answered.                                        |
| `replies` | list of `Reply` | optional | One `Reply` per question, in order; `null` abandons the form.   |

A `Reply` is what the reader answered to one question:

| Field   | Type           | Meaning                                                   |
| ------- | -------------- | --------------------------------------------------------- |
| `picks` | list of string | The `label`s of the alternatives picked.                  |
| `other` | string         | What the reader typed in place of, or beside, them.       |

```json
{"Answer":{"ticket":2,"replies":[{"picks":["Postgres"],"other":""}]}}
{"Answer":{"ticket":2,"replies":null}}
```

### `Rename`

Names the conversation, replacing any name it has. Crab answers with
`Titled`.

| Field   | Type   | Presence | Meaning       |
| ------- | ------ | -------- | ------------- |
| `title` | string | required | The new name. |

```json
{"Rename":{"title":"Flaky test"}}
```

### `Cancel`

Stops the running turn, subagents included. Crab sends `Halted`, then
`Finished` with `null`, then a `Queued` event in the `Dropped` state for each
waiting prompt. With no turn running, it still drops waiting prompts and
declines open questions, and sends nothing else.

```json
"Cancel"
```

### `StopSubagent`

Stops one subagent, named by the `id` a `Subagent` event gave it, and every
subagent it started. Its siblings and the turn carry on: the `task` call (or
`wait`, for one in the background) returns the others' results as usual, and
the stopped one's part says the reader stopped it. Crab sends a `Subagent`
event in the `Stopped` state for it and for each subagent it started. A
stopped subagent working in a worktree of its own has its changes held, not
applied. An id that names no running subagent is answered with `Failed` and
leaves the turn as it is.

| Field | Type   | Presence | Meaning                 |
| ----- | ------ | -------- | ----------------------- |
| `id`  | string | required | The id of the subagent. |

```json
{"StopSubagent":{"id":"toolu_07/2"}}
```

### `Set`

Gives a setting a value. See [Settings round-trips](#settings-round-trips).

| Field     | Type   | Presence | Meaning                            |
| --------- | ------ | -------- | ---------------------------------- |
| `setting` | string | required | The setting's `id`.                |
| `value`   | string | required | The `value` of one of its `Pick`s. |

```json
{"Set":{"setting":"mode","value":"plan"}}
```

### `Login`

Signs in to a provider in the background. Crab sends `LoginUrl` (an address
the client opens in a browser) or `LoginCode` (a page and a code), then
`SignedIn` or `Failed`. An unknown provider fails at once.

| Field      | Type   | Presence | Meaning              |
| ---------- | ------ | -------- | -------------------- |
| `provider` | string | required | The provider's id.   |

```json
{"Login":{"provider":"claude"}}
```

### `Language`

Answers a `Language` request under `ticket`.

| Field    | Type             | Presence | Meaning                    |
| -------- | ---------------- | -------- | -------------------------- |
| `ticket` | integer          | required | The request being answered.|
| `answer` | `LanguageAnswer` | required | The answer.                |

`LanguageAnswer` is one of (note the snake_case tags):

- `formatted`: field `text` (optional string), the whole formatted text of the
  file, or `null` when it cannot be formatted or is formatted already.
- `diagnostics`: field `diagnostics` (list of `Diagnostic`), what the language
  servers said, in any order.
- `locations`: field `locations` (list of `Stretch`), the places a
  `definition`, `type_definition`, `implementation` or `references` request
  found, in any order; empty when there are none.
- `hover`: field `text` (optional string), what the symbol is, as plain text
  or Markdown, or `null` when the server has nothing to say about it.
- `symbols`: field `symbols` (list of `SymbolInfo`), the symbols of a file,
  the outermost first and each followed by those inside it, or of the
  project.
- `edits`: field `files` (list of `FileEdits`), the edits a `rename` makes.
  The client writes none of them; crab makes them to the files on disk.
- `unsupported`: field `reason` (string). The client's language services, or
  the server of the file, do not answer this kind of question.
- `no_server`: field `reason` (string). No language server reads files like
  the one asked about, or the one that would is not installed or could not
  be started.
- `failed`: field `reason` (string). The server was asked and failed, or
  refused, as a rename that would create, move or delete files must be.
- `actions`: field `actions` (list of `ActionInfo`), the code actions a
  `code_actions` request found, in the order the server gave them, which
  `apply_code_action` counts by.
- `applied`: fields `title` (string, the action applied) and `files` (list of
  `FileEdits`), the edits applying a code action makes, those the server
  asked for while carrying out its command among them. The client writes
  none of them.
- `servers`: field `servers` (list of `ServerInfo`), the client's language
  servers, answering `status` and `reload`.
- `done`: no fields (`{"done":{}}`). What was asked, such as telling the
  servers a file moved, has been done.

The `reason` of the last three is shown to the model as it is. An answer of
the wrong kind counts as a failure.

A `Diagnostic`:

| Field      | Type       | Presence | Meaning                                              |
| ---------- | ---------- | -------- | ---------------------------------------------------- |
| `path`     | path       | required | The file, absolute.                                  |
| `line`     | integer    | required | The line it starts on, from 1.                       |
| `column`   | integer    | required | The character of the line it starts at, from 1.      |
| `severity` | `Severity` | required | `error`, `warning`, `information` or `hint`.         |
| `message`  | string     | required | What it says.                                        |
| `source`   | string     | optional | What said it, such as `rustc`.                       |
| `code`     | string     | optional | The diagnostic's code, such as `E0308`.              |

A `Stretch`, a stretch of a file, has `path` (path, absolute), `line` and
`column` (integers, where it starts) and `end_line` and `end_column`
(integers, where it ends, before that character), all counted from one.

A `SymbolInfo`:

| Field       | Type      | Presence | Meaning                                                                                  |
| ----------- | --------- | -------- | ---------------------------------------------------------------------------------------- |
| `name`      | string    | required | Its name.                                                                                |
| `kind`      | string    | required | What kind of symbol it is, in lower case, such as `function` or `struct`.                |
| `detail`    | string    | optional | More about it, such as a function's signature.                                           |
| `container` | string    | optional | The symbol it is inside of, such as the type of a method.                                |
| `depth`     | integer   | default  | How deeply it is nested in its file's symbols, from 0; 0 for the symbols of a project.   |
| `location`  | `Stretch` | required | Where its name is.                                                                       |

A `FileEdits` has `path` (path, absolute) and `edits` (list of `TextEdit`),
the edits to that file in any order, none overlapping. A `TextEdit` has
`line`, `column`, `end_line` and `end_column` (integers, counted from one, the
stretch replaced in the file as it is on disk before any of the edits) and
`text` (string, what replaces it).

An `ActionInfo` has `title` (string, what the server calls it), `kind`
(optional string, such as `quickfix` or `refactor.extract`), `preferred`
(bool, default, whether the server prefers it) and `disabled` (optional
string, why it cannot be applied now).

A `ServerInfo` has `name` (string, such as `rust-analyzer`), `languages`
(list of strings, default, the languages it reads), `state` (string, how it
stands in a word or two: `running`, `loading`, `starting`, `stopped`,
`failed` and so on) and `detail` (optional string, more about it, such as why
it failed).

Lines and columns are counted from one, and a column counts characters
(Unicode scalar values), not UTF-16 code units or bytes; a client whose
language servers count UTF-16 code units converts.

```json
{"Language":{"ticket":4,"answer":{"formatted":{"text":"fn main() {}\n"}}}}
{"Language":{"ticket":5,"answer":{"diagnostics":{"diagnostics":[{"path":"/home/me/project/src/lib.rs","line":3,"column":9,"severity":"error","message":"mismatched types","source":"rustc","code":"E0308"}]}}}}
{"Language":{"ticket":6,"answer":{"locations":{"locations":[{"path":"/home/me/project/src/lib.rs","line":3,"column":8,"end_line":3,"end_column":14},{"path":"/home/me/project/src/main.rs","line":5,"column":5,"end_line":5,"end_column":11}]}}}}
{"Language":{"ticket":7,"answer":{"hover":{"text":"```rust\npub fn helper() -> u32\n```"}}}}
{"Language":{"ticket":8,"answer":{"symbols":{"symbols":[{"name":"Config","kind":"struct","detail":null,"container":null,"depth":0,"location":{"path":"/home/me/project/src/lib.rs","line":1,"column":12,"end_line":1,"end_column":18}}]}}}}
{"Language":{"ticket":9,"answer":{"edits":{"files":[{"path":"/home/me/project/src/lib.rs","edits":[{"line":3,"column":8,"end_line":3,"end_column":14,"text":"assist"}]}]}}}}
{"Language":{"ticket":10,"answer":{"unsupported":{"reason":"the rust-analyzer language server does not find implementations"}}}}
{"Language":{"ticket":11,"answer":{"no_server":{"reason":"no language server reads .toml files"}}}}
{"Language":{"ticket":12,"answer":{"failed":{"reason":"the rename would also create a file"}}}}
{"Language":{"ticket":13,"answer":{"actions":{"actions":[{"title":"Import `std::collections::HashMap`","kind":"quickfix","preferred":true,"disabled":null}]}}}}
{"Language":{"ticket":14,"answer":{"applied":{"title":"Import `std::collections::HashMap`","files":[{"path":"/home/me/project/src/lib.rs","edits":[{"line":1,"column":1,"end_line":1,"end_column":1,"text":"use std::collections::HashMap;\n\n"}]}]}}}}
{"Language":{"ticket":15,"answer":{"servers":{"servers":[{"name":"rust-analyzer","languages":["rust"],"state":"running","detail":null}]}}}}
{"Language":{"ticket":16,"answer":{"done":{}}}}
```

### `Trust`

Answers a `Trust` request under `ticket`.

| Field     | Type      | Presence | Meaning                 |
| --------- | --------- | -------- | ----------------------- |
| `ticket`  | integer   | required | The request answered.   |
| `verdict` | `Verdict` | required | The reader's decision.  |

`Always` trusts the project for good, recorded for every worktree of the
repository; `Allow` trusts it for this conversation only; `Reject` records
that it is not trusted, and crab does not ask again in this conversation. What
a trusted project brings is used from the next turn on. See
[project trust](extending/project-trust.md).

```json
{"Trust":{"ticket":1,"verdict":"Always"}}
```

### `Switch`

Moves a saved conversation to another branch, at its tip. A running turn is
cancelled first, as `Cancel` does, and the prompts waiting for it are dropped;
background runs and debugging sessions go on. Crab answers with `Resumed` for
the branch. A conversation opened without an id, or a branch id the
conversation does not have, gets a `Failed` event, and the conversation stays
where it was. See [Branches](#branches).

| Field    | Type   | Presence | Meaning                                     |
| -------- | ------ | -------- | ------------------------------------------- |
| `branch` | string | required | The branch's `id`, as `Resumed` lists it.   |

```json
{"Switch":{"branch":"1"}}
```

### `Changes`

Asks what a turn changed in the files. Crab answers with a `Changes` event,
or with `Failed` when no files were kept for the turn. See
[Files of a turn](#files-of-a-turn).

| Field  | Type   | Presence | Meaning                                      |
| ------ | ------ | -------- | -------------------------------------------- |
| `turn` | string | required | The id the client gave the turn in `Prompt`. |

```json
{"Changes":{"turn":"t-4"}}
```

### `Restore`

Puts the files back as they were before a turn, and crab answers with a
`Restored` event. Fails with `Failed` while a turn is running or when no files
were kept before the turn. See [Files of a turn](#files-of-a-turn).

| Field  | Type   | Presence | Meaning                                      |
| ------ | ------ | -------- | -------------------------------------------- |
| `turn` | string | required | The id the client gave the turn in `Prompt`. |

```json
{"Restore":{"turn":"t-4"}}
```

### `Undo`

Takes what a turn changed back out of the files, and crab answers with an
`Undone` event. Fails with `Failed`, changing nothing, while a turn is
running, when no files were kept before and after the turn, or when lines the
turn changed were changed again since. See
[Files of a turn](#files-of-a-turn).

| Field   | Type           | Presence | Meaning                                                                                                   |
| ------- | -------------- | -------- | --------------------------------------------------------------------------------------------------------- |
| `turn`  | string         | required | The id the client gave the turn in `Prompt`.                                                              |
| `paths` | list of string | default  | The files, as `Changes` gave their `path`, or every file the turn changed when empty. A renamed file is put back only when both its names are listed. |

```json
{"Undo":{"turn":"t-4","paths":["calc.py"]}}
```

### `Close`

Ends the conversation, and the `crab` process exits. See
[Lifecycle](#lifecycle).

```json
"Close"
```

### `Retry`

Takes the last reply back out of the conversation and answers the reader's
last prompt again. The conversation is cut back to before that prompt, crab
sends `Resumed` with the turns left, and a turn starts with the prompt as the
reader wrote it, under the `turn` id it was saved with (none for a
conversation that is not saved), and ends with `Finished`. Images attached to
the prompt are sent again; files that are not images are not read again.
Fails with `Failed` while a turn is running or when the conversation has no
prompt. The same as the prompt `/retry`.

```json
"Retry"
```

### `Fresh`

Starts the conversation afresh: every message is taken out of what the model
is given, and crab sends `Resumed` with the turns left. The provider, model,
settings, name, goal and notebook stay. A saved conversation keeps its turns,
recorded as lost, so a replayed history still shows them. The `SessionStart`
hooks run again, with `clear`. Fails with `Failed` while a turn is running.
The same as the prompt `/fresh`.

```json
"Fresh"
```

### `Btw`

Asks the model a question on the side, and crab answers with `Aside`. The
model is shown a record of the conversation as it stood when the last turn
ended, and is offered no tools; neither the question nor the answer is added
to the conversation, and no `Finished` follows. Allowed while a turn is
running, which goes on undisturbed. A question that cannot be answered is
told with `Failed`. The same as the prompt `/btw <question>`.

| Field      | Type   | Presence | Meaning           |
| ---------- | ------ | -------- | ----------------- |
| `question` | string | required | What the reader asks. |

```json
{"Btw":{"question":"Which file holds the parser?"}}
```

### `Handoff`

Hands the work over to a new conversation. A turn of its own has the model
write a prompt that hands the conversation over, toward `goal` when one is
given. The conversation then starts afresh as `Fresh` does, crab sends
`HandedOff` with the prompt, and a turn starts with it as the first turn of
the new conversation, ending with `Finished`; there is no `Finished` between
the two. When the model cannot write the prompt the turn ends with
`Finished` and the reason, and the conversation stays as it was. Fails with
`Failed` while a turn is running. The same as the prompt `/handoff [goal]`.

| Field  | Type   | Presence | Meaning                                                                  |
| ------ | ------ | -------- | ------------------------------------------------------------------------ |
| `goal` | string | optional | What the new conversation should work toward; carry on the same work when left out. |

```json
{"Handoff":{"goal":"Write the migration guide"}}
```

### `Servers`

Asks for the OpenAI-compatible servers declared in crab's configuration (see
[custom endpoints](providers/custom-endpoints.md)), answered with `Servers`.
Like `SaveServer`, `RemoveServer` and `FindModels`, it may be sent before
`Open` and while a turn is running, so a client can run a settings page on a
`crab serve` that holds no conversation.

```json
"Servers"
```

### `SaveServer`

Adds a `Server`, or puts it in place of the one with the same `id`, in crab's
configuration, and answers with `Servers` as they now stand. The server is a
provider from then on: `crab providers` lists it, and `Open` takes its id.
Fails with `Failed`, changing nothing, when the server is not valid: an id
that is not lowercase letters, digits, `-` and `_` or is a built-in
provider's, an address that is not `http://` or `https://`, both `api_key`
and `api_key_env`, or a blank model. `api_key` left out or `null` keeps the
key saved for the id, an empty string removes it, and any other value
replaces it; `has_api_key` is ignored.

```json
{"SaveServer":{"id":"litellm","label":"LiteLLM","base_url":"http://localhost:4000/v1","wire":"responses","api_key_env":[],"api_key":"sk-local","has_api_key":false,"headers":[],"default_model":null,"models":["gpt-5"],"replay_reasoning":false}}
```

#### `Server` and `ServerWire`

| Field              | Type             | Presence | Meaning                                                                                                    |
| ------------------ | ---------------- | -------- | ---------------------------------------------------------------------------------------------------------- |
| `id`               | string           | required | The provider id it is chosen by.                                                                           |
| `label`            | string           | optional | The name a client shows; the id when `null`.                                                               |
| `base_url`         | string           | required | The address `/chat/completions`, `/responses` and `/models` are appended to.                               |
| `wire`             | `ServerWire`     | default  | `chat_completions` (the default) or `responses`.                                                           |
| `api_key_env`      | list of string   | default  | The environment variables the key is read from, first first.                                              |
| `api_key`          | string           | optional | The key, kept in the configuration. Crab always sends `null`; see `SaveServer` for what sending it does.  |
| `has_api_key`      | bool             | default  | Whether a key is kept for the server; set by crab.                                                         |
| `headers`          | list of pairs    | default  | Headers sent on every request, as `[name, value]`.                                                         |
| `default_model`    | string           | optional | The model a conversation opens with when none is picked.                                                   |
| `models`           | list of string   | default  | The models offered, in order; empty to ask the server at `GET {base_url}/models`.                          |
| `replay_reasoning` | bool             | default  | Whether earlier reasoning goes back as `reasoning_content` on Chat Completions.                            |

### `RemoveServer`

Takes the server `id` out of crab's configuration and answers with the
`Servers` left. Fails with `Failed` when no server has that id.

| Field | Type   | Presence | Meaning                |
| ----- | ------ | -------- | ---------------------- |
| `id`  | string | required | The id of the server. |

```json
{"RemoveServer":{"id":"litellm"}}
```

### `FindModels`

Asks which models a `Server`, saved or not, offers, answered with
`FoundModels` under `ticket`: its `models` when it has some, else the chat
models it lists at `GET {base_url}/models`, without embedding, speech and
image models. A server sent without `api_key` is asked with the key saved for
its id. Runs beside whatever else crab does.

| Field    | Type     | Presence | Meaning                                                     |
| -------- | -------- | -------- | ----------------------------------------------------------- |
| `ticket` | integer  | required | A number the client picks, which the answer carries back.   |
| `server` | `Server` | required | The server to ask.                                          |

```json
{"FindModels":{"ticket":3,"server":{"id":"box","label":null,"base_url":"http://localhost:8000/v1","wire":"chat_completions","api_key_env":[],"api_key":null,"has_api_key":false,"headers":[],"default_model":null,"models":[],"replay_reasoning":false}}}
```

## Events

Events are what crab sends. `Event` has these variants.

### `Settings`

What can be changed about the conversation and how it stands now: a list of
`Setting`. Sent after `Open`, after a `Set` that took, and whenever a setting
changes on its own.

A `Setting`:

| Field      | Type         | Meaning                                                              |
| ---------- | ------------ | -------------------------------------------------------------------- |
| `id`       | string       | What `Set` names the setting by.                                     |
| `category` | string       | What it is for: `model`, `mode`, `thought_level`, or something else. |
| `name`     | string       | What the reader is shown.                                            |
| `current`  | string       | The value it has now.                                                |
| `picks`    | list of `Pick` | The values it can take.                                            |

A `Pick`:

| Field         | Type   | Meaning                                         |
| ------------- | ------ | ----------------------------------------------- |
| `value`       | string | What `Set` sends.                               |
| `name`        | string | What the reader is shown.                       |
| `description` | string | What choosing it means; may be empty.           |

From a real run (one setting shown of seven):

```json
{"Settings":[{"id":"mode","category":"mode","name":"Mode","current":"default","picks":[{"value":"default","name":"Ask","description":"Ask before editing files or running commands"},{"value":"yolo","name":"Run freely","description":"Run everything without asking"},{"value":"plan","name":"Plan","description":"Only look around, then write a plan for approval before changing anything"}]},...]}
```

### `Commands`

The commands offered behind a slash: a list of `Slash`, `compact` first, then
one per skill. Sent after `Open`, and again before a prompt when the skills on
disk changed.

A `Slash`:

| Field         | Type   | Meaning                         |
| ------------- | ------ | ------------------------------- |
| `name`        | string | What is typed after the slash.  |
| `description` | string | What it does.                   |

```json
{"Commands":[{"name":"compact","description":"Summarize the older part of the conversation to free up context; text after the command says what the summary should keep in particular"},{"name":"review","description":"Review the current diff"}]}
```

### `Used`

How full the conversation's context is, as a `Usage`. Sent after each model
reply and after compaction or pruning.

| Field     | Type    | Meaning                                                                                                   |
| --------- | ------- | --------------------------------------------------------------------------------------------------------- |
| `used`    | integer | Tokens the conversation holds: what the request sent, cached or not.                                      |
| `size`    | integer | Tokens the model's context window takes.                                                                  |
| `written` | integer | Tokens the model wrote in the reply this was measured on, reasoning included; 0 after compaction or pruning. A client written before the field may ignore it, and a `Usage` saved without it reads as 0. |

```json
{"Used":{"used":22927,"size":1000000,"written":184}}
```

### `Started`

A new block of the agent's reply starts, of the `Part` given: `Thinking` or
`Text`. The `Thought` or `Said` chunks that follow belong to it until the next
`Started`.

```json
{"Started":"Text"}
```

### `Said`

The next chunk of the agent's reply text, to append to the current `Text`
block.

```json
{"Said":"` in `calc.py`"}
```

### `Thought`

The next chunk of the agent's thinking, to append to the current `Thinking`
block.

```json
{"Thought":"Look at the fixture first."}
```

### `Tool`

The agent started using a tool, or said more about one it is using. Carries a
`Tool`; see [Tool events are updates](#tool-events-are-updates).

| Field      | Type        | Presence | Meaning                                                                                 |
| ---------- | ----------- | -------- | --------------------------------------------------------------------------------------- |
| `id`       | string      | required | The call's id; every event about the call repeats it.                                   |
| `title`    | string      | optional | The tool's name, such as `bash` or `edit`.                                              |
| `kind`     | `Kind`      | optional | What the tool does.                                                                     |
| `state`    | `ToolState` | optional | `Running`, `Done` or `Failed`.                                                          |
| `target`   | string      | optional | The file, command or query it works on, as the tool describes the call; may be several lines. |
| `diff`     | `Diff`      | optional | What it changed in a file.                                                              |
| `output`   | string      | optional | What it returned to the model.                                                          |
| `parent`   | string      | optional | The subagent call whose work this is.                                                   |
| `agent`    | string      | optional | The kind of subagent this call starts, or that made a nested call.                      |
| `worktree` | `Worktree`  | optional | The copy an isolated subagent works in, a git worktree or a clone, and what became of its changes. |

`Kind` is one of `Read`, `Edit`, `Search`, `Execute`, `Fetch` and `Other`.
`read` of a web address is `Fetch`; `debug` is `Execute` when it launches,
attaches, continues or steps the program, and `Other` otherwise; `lsp` is
`Edit` when it renames, formats, moves a file or applies a code action (not
as a dry run), and `Search` otherwise.

`ToolState` is one of `Running`, `Done` and `Failed`.

A `Diff`:

| Field  | Type   | Presence | Meaning                                  |
| ------ | ------ | -------- | ---------------------------------------- |
| `path` | string | required | The file.                                |
| `old`  | string | optional | What it held before, when it existed.    |
| `new`  | string | required | What it holds after.                     |

A `Worktree`:

| Field   | Type    | Presence | Meaning                                                |
| ------- | ------- | -------- | ------------------------------------------------------ |
| `path`  | string  | required | The folder the subagent works in.                      |
| `merge` | `Merge` | optional | What became of its changes; `null` while it works.     |

`Merge` is one of:

- `Applied`: field `files` (list of string, from the repository root). The
  changes were applied to the main working tree, uncommitted.
- `Unchanged`: the subagent changed nothing.
- `Conflicted`: fields `patch` (string, the file the whole patch was saved
  in) and `files` (list of string, the files whose part does not apply). None
  of the changes were applied.
- `Held`: fields `reason` (string) and `patch` (optional string). The changes
  were not applied for a reason other than a conflict, such as the subagent
  failing.

From a real run, a call starting and finishing:

```json
{"Tool":{"id":"toolu_01TuYTkx2rQq1MVbexcwgAye","title":"edit","kind":"Edit","state":"Running","target":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","diff":null,"output":null,"parent":null,"agent":null,"worktree":null}}
{"Tool":{"id":"toolu_01TuYTkx2rQq1MVbexcwgAye","title":null,"kind":null,"state":"Done","target":null,"diff":{"path":"calc.py","old":"def add(a, b):\n    return a - b\n","new":"def add(a, b):\n    return a + b\n"},"output":"calc.py: tag D960\n     1\tdef add(a, b):\n     2\t    return a + b","parent":null,"agent":null,"worktree":null}}
```

A subagent call, a call it makes, and its worktree's fate:

```json
{"Tool":{"id":"toolu_03","title":null,"kind":null,"state":null,"target":null,"diff":null,"output":null,"parent":null,"agent":"explore","worktree":null}}
{"Tool":{"id":"toolu_03:toolu_09","title":"grep","kind":"Search","state":"Running","target":"fn parse","diff":null,"output":null,"parent":"toolu_03","agent":"explore","worktree":null}}
{"Tool":{"id":"toolu_04","title":null,"kind":null,"state":null,"target":null,"diff":null,"output":null,"parent":null,"agent":null,"worktree":{"path":"/home/me/.cache/oh-my-crab/worktrees/c-42/toolu_04","merge":null}}}
{"Tool":{"id":"toolu_04","title":null,"kind":null,"state":null,"target":null,"diff":null,"output":null,"parent":null,"agent":null,"worktree":{"path":"/home/me/.cache/oh-my-crab/worktrees/c-42/toolu_04","merge":{"Applied":{"files":["src/lib.rs"]}}}}}
```

The other `Merge` values encode as `"merge":"Unchanged"`,
`"merge":{"Conflicted":{"patch":"/home/me/.cache/oh-my-crab/output/c-42/worktree-patch-1.log","files":["src/lib.rs"]}}`
and `"merge":{"Held":{"reason":"the subagent failed","patch":null}}`.

### `Background`

A command or task left running beyond the call that started it began, or
ended. Carries a `Background`:

| Field     | Type   | Presence | Meaning                                           |
| --------- | ------ | -------- | ------------------------------------------------- |
| `id`      | string | required | The handle the agent knows it by.                 |
| `label`   | string | required | The command or task that was started.             |
| `running` | bool   | required | Whether it still runs.                            |
| `call`    | string | optional | The id of the tool call that started it.          |

Background runs belong to the conversation, not to a turn: they can report
between turns, and they are stopped by `Open` and `Close`.

```json
{"Background":{"id":"job-1","label":"npm run dev","running":true,"call":"toolu_05"}}
```

### `Subagent`

A subagent started, or ended. Sent once in the `Running` state when it starts,
for subagents in the foreground, in the background and started by other
subagents, and once more in another state when it ends. Carries a `Subagent`:

| Field        | Type   | Presence | Meaning                                                                   |
| ------------ | ------ | -------- | ------------------------------------------------------------------------- |
| `id`         | string | required | Its id for the conversation, which `StopSubagent` names.                  |
| `call`       | string | required | The id of the `task` call that started it, as its `Tool` events carry it. |
| `agent`      | string | required | The kind of subagent.                                                     |
| `label`      | string | required | What it is doing, in a few words.                                         |
| `model`      | string | optional | The model it asks, when that is known.                                    |
| `background` | bool   | required | Whether it runs in the background.                                        |
| `state`      | string | required | How it stands, a `SubagentState`.                                         |

The id is the `task` call id, followed by `/` and the place in the list,
counted from one, when the call started several. The id of a subagent started
by another subagent is that one's id, a `:`, and its own: `toolu_07/2:toolu_11`.
`SubagentState` is one of `Running`, `Finished` (it ended with a result),
`Failed` (it ended with an error) and `Stopped` (`StopSubagent` stopped it or
a subagent above it, or the turn or background run it belonged to was
stopped).

```json
{"Subagent":{"id":"toolu_07/2","call":"toolu_07","agent":"scout","label":"Find the parser","model":"claude-sonnet-4-5","background":false,"state":"Running"}}
{"Subagent":{"id":"toolu_07/2","call":"toolu_07","agent":"scout","label":"Find the parser","model":"claude-sonnet-4-5","background":false,"state":"Stopped"}}
```

### `Halted`

The running turn was stopped, so that nothing it had running goes on. Always
followed by `Finished` with `null`.

```json
"Halted"
```

### `Plan`

The agent's plan, whole, as it stands now: a list of `PlanStep`, each with
`text` (string) and `progress`, a `Progress`: `Pending`, `Doing` or `Done`. Each `Plan`
replaces the one before.

```json
{"Plan":[{"text":"Reproduce the failure","progress":"Done"},{"text":"Fix the fixture","progress":"Doing"},{"text":"Run the suite","progress":"Pending"}]}
```

### `Ask`

The agent waits for permission. Carries a `Permission`; answer with `Choose`.
Sent for a tool call that needs approval in `default` mode, and for a plan
ready for review in `plan` mode.

| Field     | Type           | Presence | Meaning                                                                |
| --------- | -------------- | -------- | ---------------------------------------------------------------------- |
| `ticket`  | integer        | required | Names the question in the answer.                                      |
| `title`   | string         | required | What the agent wants to do: the tool and the first line of the call.   |
| `command` | string         | optional | The whole call, for `bash` or a call described in several lines.       |
| `plan`    | string         | optional | The plan to approve, in plan mode.                                     |
| `choices` | list of `Choice` | required | The answers on offer.                                                |

A `Choice`:

| Field     | Type      | Meaning                                       |
| --------- | --------- | --------------------------------------------- |
| `id`      | string    | What `Choose` sends as `option`.              |
| `name`    | string    | What the reader is shown.                     |
| `verdict` | `Verdict` | `Allow`, `Always` or `Reject`.                |

A tool call offers, in this order:

- `allow` ("Allow once");
- `always`, verdict `Always`, named after the permission rules it adds for the
  rest of the conversation, such as "Always allow Bash(cargo test:*)" or
  "Always allow Edit(/src/**)", or just "Always allow" when the rule is the
  tool's own name;
- `always-project`, verdict `Always`, "… in this project": the same, and the
  rules are kept in crab's permissions file for the project;
- `always-user`, verdict `Always`, "… in every project": the same, kept for
  every project, with paths written from the root of the file system;
- `reject`.

The first `Always` choice is always `always`. `always-project` and
`always-user` are offered only when crab has a permissions file to keep rules
in. No `Always` choice is offered when no rule can cover the call: a command
line crab will not take apart, one that writes a file, or a call an ask rule
matched. A plan offers, in this order:

- `allow`, verdict `Allow`, "Approve and execute": plan mode ends and the
  agent carries the plan out in the same turn;
- `fresh`, verdict `Allow`, "Approve and execute in a fresh context": the
  same, in a fresh context window that starts with the plan and the note;
- `reject`, verdict `Reject`, "Keep planning": the agent revises the plan,
  told the `note` of the `Choose`;
- `discard`, verdict `Reject`, "Reject the plan": the agent is told the plan
  is rejected, with the note, stays in plan mode and ends its turn.

`null`, and any choice besides these and the tool call's allowing ones, keeps
the agent planning as `reject` does; `always`, `always-project` and
`always-user` approve a plan as `allow` does. A client that only knows
`allow` and `reject` keeps working as before. See [plans](concepts.md#plans).

From real runs, an edit and a command:

```json
{"Ask":{"ticket":1,"title":"edit: calc.py","command":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow Edit(/**)","verdict":"Always"},{"id":"always-project","name":"Always allow Edit(/**) in this project","verdict":"Always"},{"id":"always-user","name":"Always allow Edit(//home/me/calc/**) in every project","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
{"Ask":{"ticket":1,"title":"bash: git status","command":"git status","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow Bash(git status:*)","verdict":"Always"},{"id":"always-project","name":"Always allow Bash(git status:*) in this project","verdict":"Always"},{"id":"always-user","name":"Always allow Bash(git status:*) in every project","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
```

### `Inquire`

The agent's `ask` tool waits for the answers to a form. Carries an `Inquiry`
with `ticket` (integer) and `questions` (list of `Question`); answer with
`Answer`.

A `Question`:

| Field          | Type                | Meaning                                        |
| -------------- | ------------------- | ---------------------------------------------- |
| `header`       | string              | A word or two naming what is asked; may be empty. |
| `text`         | string              | The question.                                  |
| `multiple`     | bool                | Whether several alternatives may be picked.    |
| `alternatives` | list of `Alternative` | The answers on offer.                        |

An `Alternative` has `label` (string, what `Reply.picks` sends) and
`description` (string, may be empty).

```json
{"Inquire":{"ticket":2,"questions":[{"header":"Database","text":"Which database should the service use?","multiple":false,"alternatives":[{"label":"Postgres","description":"The one production runs"},{"label":"SQLite","description":""}]}]}}
```

### `Language`

Crab asks the client's language services something, and waits for a
`Language` command under the ticket. Sent only to a client that set
`languages` in `Open`. Carries a `LanguageRequest` with `ticket` (integer) and
`ask` (`LanguageAsk`). See [language services](language-services.md).

`LanguageAsk` is one of (snake_case tags):

- `format`: field `path` (path). Format the file as it is on disk now; answer
  with `formatted`. Crab waits three seconds and a little more for the answer.
- `diagnostics`: fields `paths` (list of path) and `wait_ms` (integer). Say
  what is wrong with these files, which were just written; the client may wait
  up to `wait_ms` milliseconds for its servers to report on the new text, then
  answers `diagnostics` with what it has. Crab waits `wait_ms` and a little
  more.

A request not answered in time gets no formatting or no diagnostics, and the
tool goes on.

A client that also set `navigation` in `Open` is asked the `lsp` tool's
questions about code too. Each names a place as a `Place`: `path` (path,
absolute), `line` and `column` (integers, counted from one, a column counting
characters). Crab waits a minute and a little more for each answer, enough
for a server to start and load the project; one not answered in time fails
the tool call. Any of them may be answered `unsupported`, `no_server` or
`failed`.

- `definition`, `type_definition`, `implementation`, `references`: field
  `at` (`Place`). Where the symbol there is defined, where its type is
  defined, where it is implemented, or every use of it with its declaration;
  answer with `locations`.
- `hover`: field `at` (`Place`). What the symbol there is; answer with
  `hover`.
- `document_symbols`: field `path` (path). The symbols of the file; answer
  with `symbols`.
- `workspace_symbols`: fields `query` (string) and `near` (optional path). The
  project's symbols whose names match `query`, from the server of the file
  `near` when it is given, otherwise from every server of the project; answer
  with `symbols`.
- `workspace_diagnostics`: field `wait_ms` (integer). Everything the servers of
  the project have said is wrong with any of its files, waiting up to
  `wait_ms` milliseconds for servers still loading or checking it; answer with
  `diagnostics`. Crab waits `wait_ms` and a little more.
- `rename`: fields `at` (`Place`) and `new_name` (string). The edits that
  rename the symbol there in every file that uses it, made to the files as
  they are on disk now; answer with `edits`. The client must not write them:
  crab checks that they stay inside the working directory, asks the reader
  first outside yolo mode, writes every file or none, then asks the client to
  `format` them and for their `diagnostics` as after an `edit`, and gives the
  model each file's new snapshot tag. A rename the server would make by
  creating, moving or deleting files is answered `failed`.
- `format_range`: field `range` (`Stretch`). The whole text of the file the
  stretch names with only that stretch formatted; answer with `formatted`.
  Formatting a whole file for the `lsp` tool is the `format` request above,
  which a client without `navigation` is sent too.
- `code_actions`: fields `range` (`Stretch`) and `only` (list of strings,
  default). The code actions the server of the file offers there, only those
  whose kinds start with one of `only` when it names any; answer with
  `actions`.
- `apply_code_action`: fields `range` (`Stretch`), `only` (list of strings,
  default), `index` (integer, counted from one) and `title` (optional
  string). Ask for the actions as for `code_actions`, take the one titled
  `title` when there is one and otherwise the `index`-th, resolve it when the
  server needs that, and when it has a command have the server carry it out,
  keeping the edits the server asks for while it does (`workspace/applyEdit`)
  instead of making them; answer with `applied`. The client must not write
  them: crab writes them as it does a rename's, asking the reader first
  outside yolo mode. An action that would create, move or delete files is
  answered `failed`, and one that only runs a command the server does not
  carry out itself `unsupported`.
- `will_rename_file`: fields `from` and `to` (paths, absolute). The edits the
  servers of the project make before the file is moved, such as to the
  imports and module declarations that name it, made to the files as they
  are on disk now; answer with `edits`, which may name the moved file at its
  old path. Nothing is moved or written by the client: crab moves the file,
  writes the edits, has the files formatted and checked, then sends
  `file_renamed`.
- `file_renamed`: fields `from` and `to` (paths, absolute). The file has been
  moved; tell the servers, and answer `done`.
- `status`: no fields (`{"status":{}}`). The client's language servers for
  the project and how each stands; answer with `servers`.
- `reload`: fields `server` (optional string, a name as `servers` gives it)
  and `near` (optional path). Stop and start again that server, or the one
  that reads the file `near`, or every server running for the project when
  neither is given; answer with `servers`, listing those restarted.

A client that did not set `navigation` is never sent these, so a client
written before they existed keeps working with a newer crab. With it, the
`lsp` tool's `diagnostics` of a file still go to the client as a
`diagnostics` request and its `format` of a whole file as a `format` request,
and its other actions are not sent: the tool tells the model they cannot be
answered in this conversation. A client that set `navigation` before
`format_range`, `code_actions`, `apply_code_action`, `will_rename_file`,
`file_renamed`, `status` and `reload` existed fails to read them; it should
answer a request it cannot read with `unsupported` if it can, and otherwise
crab gives up on it after the wait, failing that tool call alone.

```json
{"Language":{"ticket":4,"ask":{"format":{"path":"/home/me/project/src/main.rs"}}}}
{"Language":{"ticket":5,"ask":{"diagnostics":{"paths":["/home/me/project/src/lib.rs"],"wait_ms":3000}}}}
{"Language":{"ticket":6,"ask":{"references":{"at":{"path":"/home/me/project/src/lib.rs","line":3,"column":8}}}}}
{"Language":{"ticket":7,"ask":{"definition":{"at":{"path":"/home/me/project/src/main.rs","line":5,"column":5}}}}}
{"Language":{"ticket":8,"ask":{"type_definition":{"at":{"path":"/home/me/project/src/main.rs","line":4,"column":9}}}}}
{"Language":{"ticket":9,"ask":{"implementation":{"at":{"path":"/home/me/project/src/lib.rs","line":10,"column":11}}}}}
{"Language":{"ticket":10,"ask":{"hover":{"at":{"path":"/home/me/project/src/lib.rs","line":3,"column":8}}}}}
{"Language":{"ticket":11,"ask":{"document_symbols":{"path":"/home/me/project/src/lib.rs"}}}}
{"Language":{"ticket":12,"ask":{"workspace_symbols":{"query":"Config","near":null}}}}
{"Language":{"ticket":13,"ask":{"workspace_diagnostics":{"wait_ms":3000}}}}
{"Language":{"ticket":14,"ask":{"rename":{"at":{"path":"/home/me/project/src/lib.rs","line":3,"column":8},"new_name":"assist"}}}}
{"Language":{"ticket":15,"ask":{"format_range":{"range":{"path":"/home/me/project/src/lib.rs","line":3,"column":1,"end_line":9,"end_column":2}}}}}
{"Language":{"ticket":16,"ask":{"code_actions":{"range":{"path":"/home/me/project/src/lib.rs","line":3,"column":5,"end_line":3,"end_column":12},"only":["quickfix"]}}}}
{"Language":{"ticket":17,"ask":{"apply_code_action":{"range":{"path":"/home/me/project/src/lib.rs","line":3,"column":5,"end_line":3,"end_column":12},"only":["quickfix"],"index":1,"title":"Import `std::collections::HashMap`"}}}}
{"Language":{"ticket":18,"ask":{"will_rename_file":{"from":"/home/me/project/src/util.rs","to":"/home/me/project/src/helpers.rs"}}}}
{"Language":{"ticket":19,"ask":{"file_renamed":{"from":"/home/me/project/src/util.rs","to":"/home/me/project/src/helpers.rs"}}}}
{"Language":{"ticket":20,"ask":{"status":{}}}}
{"Language":{"ticket":21,"ask":{"reload":{"server":"rust-analyzer","near":null}}}}
```

### `Finished`

The turn ended: `null` when it ended normally or was stopped, or the reason
it failed. Every turn ends with exactly one `Finished`, except a turn that ends
with `SignedOut` or is cut short by `Close`.

```json
{"Finished":null}
{"Finished":"provider error: overloaded"}
```

### `SignedOut`

The account needs signing in: sent instead of `Finished` when a turn fails
authentication, and instead of the opening events when `Open` finds no login.

| Field     | Type   | Presence | Meaning                                |
| --------- | ------ | -------- | -------------------------------------- |
| `reason`  | string | required | Why authentication failed.             |
| `account` | string | optional | The account's name, when it has one.   |

```json
{"SignedOut":{"reason":"not logged in","account":null}}
```

### `SignedIn`

A `Login` succeeded.

```json
"SignedIn"
```

### `LoginUrl`

During `Login`, the address the reader opens in a browser to sign in. Crab
does not open it; the client does. Also sent, outside any `Login`, when a tool
server wants the reader to sign in to it with OAuth, usually while the
conversation opens; the server's tools arrive once the browser comes back
(see [Signing in](extending/mcp.md#signing-in)).

```json
{"LoginUrl":"https://claude.ai/oauth/authorize?code=true"}
```

### `LoginCode`

During `Login`, the page the reader opens and the code to enter there.

| Field  | Type   | Meaning              |
| ------ | ------ | -------------------- |
| `url`  | string | The page to open.    |
| `code` | string | The code to enter.   |

```json
{"LoginCode":{"url":"https://github.com/login/device","code":"ABCD-1234"}}
```

### `Failed`

Crab could not do what was asked, for the reason given. Sent for a bad
command line, an `Open` that fails, a `Prompt` before `Open`, a refused
`/compact`, a failed `Login`, and history or trust that cannot be saved. It
does not end a turn; `Finished` does.

From a real run:

```json
{"Failed":"the conversation is not open"}
{"Failed":"bad command: unknown variant `Nope`, expected one of `Open`, `Prompt`, `Choose`, `Answer`, `Rename`, `Cancel`, `Set`, `Login`, `Language`, `Trust`, `Switch`, `Close` at line 1 column 7"}
```

### `Resumed`

The conversation as it was saved, sent when a conversation with an id is
opened, when `before` goes back to before a turn, and after `Switch`. See
[History replay](#history-replay). Carries a `Conversation`:

| Field      | Type               | Presence | Meaning                                                      |
| ---------- | ------------------ | -------- | ------------------------------------------------------------ |
| `title`    | string             | optional | The conversation's name.                                     |
| `settings` | list of pairs      | default  | The latest value chosen for each setting, by id.             |
| `turns`    | list of `PastTurn` | required | The turns, oldest first.                                     |
| `offered`  | list of `Setting`  | default  | What could be set when it was last offered.                  |
| `commands` | list of `Slash`    | default  | The slash commands last offered.                             |
| `window`   | integer            | optional | The context window of the model last used, in tokens.        |
| `branch`   | string             | optional | The id of the branch `turns` are of; see [Branches](#branches). |
| `branches` | list of `Branch`   | default  | Every branch, in the order they were made, the first first.  |
| `spent`    | `Spent`            | optional | What the conversation has cost so far, when it has cost something; see [`Spent`](#spent). |

A `PastTurn`:

| Field    | Type            | Presence | Meaning                                      |
| -------- | --------------- | -------- | -------------------------------------------- |
| `id`     | string          | optional | The `turn` id the client gave the prompt.    |
| `prompt` | string          | required | What the reader prompted with.               |
| `files`  | list of string  | default  | The names of the files attached.             |
| `events` | list of `Event` | required | What the agent did, as events, in order.     |

From a real run (lines shortened):

```json
{"Resumed":{"title":null,"settings":[],"turns":[{"id":"t-1","prompt":"Read calc.py and fix the bug in add. Be brief.","files":[],"events":[{"Tool":{"id":"toolu_01BJncUS5JG4uPGyhLqWQqzL","title":"read","kind":"Read","state":"Running","target":"calc.py","diff":null,"output":null,"parent":null,"agent":null,"worktree":null}},{"Used":{"used":22927,"size":1000000}},...,{"Started":"Text"},{"Said":"Fixed `add` in `calc.py`: it returned `a - b`, and now returns `a + b`."},{"Used":{"used":23141,"size":1000000}}]}],"offered":[...],"commands":[...],"window":1000000}}
```

A `Branch`:

| Field    | Type                 | Presence | Meaning                                                                 |
| -------- | -------------------- | -------- | ----------------------------------------------------------------------- |
| `id`     | string               | required | What `Switch` names it by: `1` for the first, counting up.              |
| `parent` | string               | optional | The branch it went back from; `null` for the first.                     |
| `before` | string               | optional | The turn of `parent` it went back to before; `null` for the first.      |
| `turns`  | list of `BranchTurn` | default  | Its turns that have an id, oldest first, shared ones included.          |

A `BranchTurn`:

| Field    | Type   | Presence | Meaning                                                                     |
| -------- | ------ | -------- | --------------------------------------------------------------------------- |
| `id`     | string | required | The `turn` id the client gave the prompt.                                   |
| `prompt` | string | required | What the reader prompted with.                                              |
| `ended`  | string | optional | How a turn without an answer ended: `stopped`, `interrupted`, or the failure; `null` for one that answered. |

```json
{"Resumed":{"title":null,"settings":[],"turns":[],"offered":[],"commands":[],"window":200000,"branch":"2","branches":[{"id":"1","parent":null,"before":null,"turns":[{"id":"t-1","prompt":"Say only the word: red","ended":null},{"id":"t-2","prompt":"Say only the word: green","ended":"stopped"}]},{"id":"2","parent":"1","before":"t-1","turns":[]}]}}
```

`crab history <id> [folder]` prints the same `Conversation` without opening
the conversation, with `window` taken from what was saved.

### `Titled`

The conversation was named, by the agent or by `Rename`, or a saved
conversation with a name was opened.

```json
{"Titled":"Calculator fix"}
```

### `Queued`

A prompt sent while a turn was running waits, was put into that turn, started
a turn of its own, or was dropped. See [Queued prompts](#queued-prompts).
Carries a `Queued`:

| Field    | Type         | Presence | Meaning                                                   |
| -------- | ------------ | -------- | --------------------------------------------------------- |
| `ticket` | integer      | required | The prompt's number, the same in every event about it.    |
| `turn`   | string       | optional | The `turn` id the client sent with it.                    |
| `text`   | string       | required | What the reader wrote.                                    |
| `files`  | list of string | default | The names of the files attached.                         |
| `state`  | `QueueState` | required | `Waiting`, `Steered`, `Started` or `Dropped`.             |

A steered or started prompt is the reader's from then on, as one sent between
turns is.

```json
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Waiting"}}
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Steered"}}
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Started"}}
{"Queued":{"ticket":1,"turn":"t-3","text":"Also add a docstring.","files":[],"state":"Dropped"}}
```

### `Trust`

Crab left out what the project folder brings, or some of it, until the reader
trusts it, and waits for a `Trust` command under the ticket. The conversation
goes on without those inputs meanwhile. Sent after `Open`, and again, under a
new ticket, when what is left out changes; the files are read again before
every prompt. Carries a `TrustRequest`:

| Field      | Type            | Meaning                                                                                       |
| ---------- | --------------- | --------------------------------------------------------------------------------------------- |
| `ticket`   | integer         | Names the request in the answer.                                                              |
| `root`     | path            | The folder trust is recorded for: the repository's main checkout, or the folder outside one.  |
| `standing` | `Standing`      | `Unknown` (nobody decided), `Changed` (trusted, but a hook or tool server was added or changed since) or `Declined`. |
| `skipped`  | list of `Skipped` | What was left out.                                                                          |

A `Skipped`:

| Field  | Type    | Meaning                                                                                       |
| ------ | ------- | --------------------------------------------------------------------------------------------- |
| `kind` | `Input` | `Hook`, `Instruction`, `Skill` or `Agent`.                                                    |
| `name` | string  | A skill or agent's name, an instruction file's path, a hook's event, matcher and command, or, for a project's tool server, which is a `Hook` too, `tool server <name>: <command line or address>`. |
| `path` | path    | The file it is defined in.                                                                    |

From a real run:

```json
{"Trust":{"ticket":1,"root":"/home/me/project","standing":"Unknown","skipped":[{"kind":"Hook","name":"PreToolUse bash: ./guard.sh","path":"/home/me/project/.claude/settings.json"},{"kind":"Instruction","name":"/home/me/project/AGENTS.md","path":"/home/me/project/AGENTS.md"}]}}
```

### `Goal`

The conversation's goal was set, or how it stands changed. Sent when it is
set, paused, resumed, cleared, given a new budget, completed or runs out of
budget, and when a saved conversation that has one opens. See
[goals](goals.md). Carries a `Goal`:

| Field       | Type        | Presence | Meaning                                                                                       |
| ----------- | ----------- | -------- | --------------------------------------------------------------------------------------------- |
| `objective` | string      | required | What the agent works toward.                                                                  |
| `state`     | `GoalState` | required | `Active`, `Paused`, `BudgetLimited`, `Complete` or `Cleared`.                                 |
| `budget`    | integer     | optional | The most tokens the goal may use.                                                             |
| `used`      | integer     | default  | Tokens used toward it: input, output and cache writes of every request while it was active, subagents included. |
| `note`      | string      | optional | Why it stands as it does: why it paused, or the agent's summary when it completed.             |

```json
{"Goal":{"objective":"Make every test in the workspace pass","state":"Active","budget":2000000,"used":0,"note":null}}
{"Goal":{"objective":"Make every test in the workspace pass","state":"Paused","budget":2000000,"used":412000,"note":"two turns in a row made the same tool calls with the same results"}}
```

### `Continued`

Crab started a turn of its own toward the active goal, since the last turn
ended with the goal still open. The turn's events follow as for a prompt the
reader sent, and it ends with `Finished`. The prompt is saved as the turn's
prompt, so a replayed history shows it as a turn the reader sent.

| Field    | Type   | Presence | Meaning                                                                                                     |
| -------- | ------ | -------- | ----------------------------------------------------------------------------------------------------------- |
| `prompt` | string | required | The prompt the turn started with.                                                                           |
| `turn`   | string | required | The id crab saved the turn under: `goal-` and a number. Use it as the id of a turn the client prompted: with `before` in `Open`, and with `Changes`, `Restore` and `Undo`. |

```json
{"Continued":{"prompt":"Keep working toward the goal.","turn":"goal-1794563890123456789"}}
```

### `Changes`

What a turn changed in the files, in answer to the `Changes` command.

| Field   | Type                 | Presence | Meaning                                                                                  |
| ------- | -------------------- | -------- | ---------------------------------------------------------------------------------------- |
| `turn`  | string               | required | The turn's id.                                                                           |
| `open`  | boolean              | required | Whether the turn is still running, so the files are set against what they hold now.      |
| `files` | list of `TurnChange` | required | The files the turn changed.                                                              |

A `TurnChange`:

| Field       | Type    | Presence | Meaning                                                                                              |
| ----------- | ------- | -------- | ---------------------------------------------------------------------------------------------------- |
| `path`      | string  | required | Where the file is, from the root of the repository.                                                  |
| `from`      | string  | optional | Where it was before the turn renamed or copied it.                                                   |
| `status`    | string  | required | The letter git writes the change with: `A` added, `M` modified, `D` deleted, `R` renamed, `C` copied, `T` changed in kind. |
| `diff`      | string  | required | The lines of its diff from the first hunk on, each starting with `@`, `+`, `-` or a space; empty for a file that is not text. |
| `added`     | integer | required | How many lines the turn put in.                                                                      |
| `removed`   | integer | required | How many lines the turn took out.                                                                    |
| `truncated` | boolean | default  | Whether `diff` was cut at 2000 lines; `added` and `removed` then count only the lines sent.          |

```json
{"Changes":{"turn":"t-4","open":true,"files":[{"path":"notes.txt","from":null,"status":"A","diff":"@@ -0,0 +1 @@\n+hello","added":1,"removed":0,"truncated":false}]}}
```

### `Restored`

The files were put back as they were before a turn, in answer to `Restore`.
`files` are the paths that changed, from the root of the repository.

```json
{"Restored":{"turn":"t-4","files":["calc.py","notes.txt"]}}
```

### `Undone`

What a turn changed was taken back out of the files, in answer to `Undo`.
`files` are the paths that changed, from the root of the repository; the list
is empty when the turn changed none of the files asked for.

```json
{"Undone":{"turn":"t-4","files":["calc.py"]}}
```

### `Spent`

What the conversation has cost so far, whole: a client replaces what it showed
before. Sent after every model request the provider reported the tokens of,
those of subagents, summaries and cache warming included, so it also comes
between turns; and when a saved conversation that has cost something is
opened. See [what a conversation costs](long-conversations.md#what-a-conversation-costs).

| Field    | Type                | Meaning                                                  |
| -------- | ------------------- | -------------------------------------------------------- |
| `models` | list of `ModelSpend` | What each model cost, in the order they were first asked. |

`ModelSpend`:

| Field         | Type              | Meaning                                                                                                                              |
| ------------- | ----------------- | ------------------------------------------------------------------------------------------------------------------------------------ |
| `model`       | string            | The model's id; one served by another provider than the conversation's is `provider/id`.                                             |
| `requests`    | integer           | Requests it answered.                                                                                                                |
| `input`       | integer           | Input tokens neither read from nor written to the prompt cache.                                                                      |
| `output`      | integer           | Tokens it wrote, reasoning included.                                                                                                 |
| `cache_read`  | integer           | Input tokens read from the prompt cache.                                                                                             |
| `cache_write` | integer           | Input tokens written to the prompt cache.                                                                                            |
| `cost`        | optional integer  | What the requests cost at list prices, in millionths of a US dollar; `null` when the model's prices are not known.                   |
| `missed`      | integer           | How much of `cost` was paid for cache misses, in millionths of a US dollar.                                                          |

```json
{"Spent":{"models":[{"model":"claude-haiku-5-5","requests":3,"input":412,"output":611,"cache_read":45936,"cache_write":23141,"cost":3698,"missed":0}]}}
```

### `Aside`

The answer to a question asked with `Btw`. It is not part of the
conversation, so a replayed history does not show it.

| Field      | Type   | Presence | Meaning                |
| ---------- | ------ | -------- | ---------------------- |
| `question` | string | required | What the reader asked. |
| `answer`   | string | required | What the model answered. |

```json
{"Aside":{"question":"Which file holds the parser?","answer":"src/parse.rs."}}
```

### `HandedOff`

The conversation was handed over to a new one, in answer to `Handoff`: it
started afresh, and a turn starts with `prompt`, whose events follow as for a
prompt the reader sent. Sent after the `Resumed` that shows the conversation
emptied.

| Field    | Type   | Presence | Meaning                                                                                                     |
| -------- | ------ | -------- | ----------------------------------------------------------------------------------------------------------- |
| `prompt` | string | required | The prompt the model wrote, which the new conversation starts with.                                         |
| `turn`   | string | required | The id crab saved the new conversation's first turn under: `handoff-` and a number. Use it as the id of a turn the client prompted. |

```json
{"HandedOff":{"prompt":"You are taking over the parser rewrite...","turn":"handoff-1794563890123456789"}}
```

### `Servers`

The OpenAI-compatible servers declared in crab's configuration, as a list of
`Server` in the order they are offered, in answer to `Servers`, `SaveServer`
and `RemoveServer`. No key is sent: `api_key` is always `null`, and
`has_api_key` says whether one is kept.

```json
{"Servers":[{"id":"litellm","label":"LiteLLM","base_url":"http://localhost:4000/v1","wire":"responses","api_key_env":[],"api_key":null,"has_api_key":true,"headers":[],"default_model":null,"models":["gpt-5"],"replay_reasoning":false}]}
```

### `FoundModels`

The models a server offers, in answer to the `FindModels` under `ticket`.

| Field    | Type           | Presence | Meaning                                                              |
| -------- | -------------- | -------- | -------------------------------------------------------------------- |
| `ticket` | integer        | required | The ticket of the `FindModels` answered.                             |
| `models` | list of string | required | The model ids, in the server's order; empty when it was not reached. |
| `error`  | string         | optional | Why the server could not be asked; `null` when it was.               |

```json
{"FoundModels":{"ticket":3,"models":["qwen3-coder-30b","llama-3.3-70b"],"error":null}}
```


## Types the subcommands print and read

`omc-wire` also holds the shapes of what some `crab` subcommands print or
read. They do not travel over `crab serve`, but a client usually reads them
too.

### `ProviderInfo`, `SignIn` and `KeySource`

`crab providers` prints a list of `ProviderInfo`:

| Field     | Type           | Presence | Meaning                                                                                      |
| --------- | -------------- | -------- | -------------------------------------------------------------------------------------------- |
| `id`      | string         | required | What `Open` and `Login` name the provider by.                                                |
| `name`    | string         | required | What a client shows.                                                                         |
| `sign_in` | `SignIn`       | required | `browser` (answered by `LoginUrl`), `device_code` (by `LoginCode`), `api_key` or `none`.     |
| `key_env` | list of string | default  | The environment variables an API key is read from; left out of the output when empty.        |
| `credentials` | string     | optional | The cloud credentials the provider is reached with and where they come from, as a client shows them; left out of the output when there are none. A provider with `sign_in` `none` that names them is not a local server: it reads the credentials the cloud's own tools keep. |
| `server`  | bool           | default  | Whether it is a server the user declared, which `SaveServer` and `RemoveServer` edit; left out of the output when false. |
| `models`  | list of string | default  | The models a declared server was given to offer; left out of the output when empty, as for every built-in provider. |

```json
[{"id":"claude","name":"Claude","sign_in":"browser"},{"id":"ollama","name":"Ollama","sign_in":"none"},{"id":"openrouter","name":"OpenRouter","sign_in":"api_key","key_env":["OPENROUTER_API_KEY"]},{"id":"copilot","name":"GitHub Copilot","sign_in":"device_code"},...]
```

A declared server comes after the built-in providers:

```json
[{"id":"litellm","name":"LiteLLM","sign_in":"none","server":true,"models":["gpt-5"]}]
```

The cloud providers name their credentials:

```json
[{"id":"bedrock","name":"Amazon Bedrock","sign_in":"api_key","key_env":["AWS_BEARER_TOKEN_BEDROCK"],"credentials":"AWS credentials (the AWS CLI's profiles, SSO or environment)"},{"id":"vertex","name":"Google Vertex AI","sign_in":"none","credentials":"Google Cloud credentials (gcloud or Application Default Credentials)"}]
```

`KeySource` says where a provider's API key comes from, for a program
embedding `omc-session`. It is tagged by a `from` field, `environment` (with
the variable's `name`) or `saved`:
`{"from":"environment","name":"OPENROUTER_API_KEY"}` or `{"from":"saved"}`.

### `ToolInfo`

`crab tools` prints a list of `ToolInfo`, each with `name` and `description`
(both strings):

```json
[{"name":"ask","description":"Asks the user questions during execution and waits for the answers."},...]
```

### `Limits`, `Limit` and `LimitUnit`

`crab limits --json` prints a list of `Limits`, one for each provider asked:
Claude (the five-hour and weekly windows, a weekly cap for a model that has its
own, and extra usage), Codex (the plan's windows, those of extra metered
features and a credit cap), GitHub Copilot (the month's premium requests, chat
and completions), Kimi Code, Z.ai and MiniMax. The providers publish no
documentation for these endpoints, so a field the provider leaves out is left
out here too.

| Field           | Type            | Presence | Meaning                                              |
| --------------- | --------------- | -------- | ---------------------------------------------------- |
| `provider`      | string          | required | The provider's id, such as `claude`.                 |
| `plan`          | string          | optional | The plan the account is on, when the provider says.  |
| `fetched_at_ms` | integer         | required | When the provider was asked, as Unix milliseconds.   |
| `limits`        | list of `Limit` | required | The windows and quotas, in the order to show them.   |

| `Limit` field   | Type      | Presence | Meaning                                                                 |
| --------------- | --------- | -------- | ----------------------------------------------------------------------- |
| `label`         | string    | required | The name to show, such as `5 hour`.                                     |
| `window`        | string    | optional | A stable name for the window's length: `5h`, `7d`, `monthly`.           |
| `unit`          | `LimitUnit` | default | `percent`, `requests`, `tokens`, `credits`, `usd` or `unknown`.        |
| `used_fraction` | number    | optional | How much is used, from 0 to 1.                                          |
| `used`          | number    | optional | How much is used, in `unit`.                                            |
| `limit`         | number    | optional | The cap, in `unit`; absent when there is none or it is not reported.    |
| `resets_at_ms`  | integer   | optional | When the window resets, as Unix milliseconds.                           |
| `unlimited`     | bool      | default  | Whether there is no cap.                                                |

```json
[{"provider":"claude","fetched_at_ms":1794308000000,"limits":[{"label":"5 hour","window":"5h","unit":"percent","used_fraction":0.37,"used":37.0,"limit":100.0,"resets_at_ms":1794312000000}]}]
```

From Rust, `omc_session::limits(id, home)` asks one provider and
`omc_session::limits_providers()` lists those that can be asked.

### `MemoryListing` and `MemoryEntry`

`crab memory [folder]` prints a `MemoryListing`:

| Field            | Type                 | Presence | Meaning                                                              |
| ---------------- | -------------------- | -------- | -------------------------------------------------------------------- |
| `enabled`        | bool                 | required | Whether memory is on for conversations started like this one.        |
| `name`           | string               | required | The project's short name.                                            |
| `project`        | string               | required | The folder the project is shown as.                                  |
| `project_folder` | string               | default  | Where the project's memories are kept; empty when nowhere.           |
| `user_folder`    | string               | default  | Where the user's memories are kept; empty when nowhere.              |
| `memories`       | list of `MemoryEntry`| default  | The project's memories first, each scope oldest first.               |

A `MemoryEntry` has `id`, `scope` (`project` or `user`), `text`, `tags`
(default empty), `created` (`YYYY-MM-DDTHH:MM:SSZ`), and the optional
`conversation`, `updated` and `updated_in`. See [memory](memory.md).

```json
{"enabled":true,"name":"project","project":"/home/me/project","project_folder":"/home/me/.local/share/oh-my-crab/memory/projects/project-9e371259b705fcaa","user_folder":"/home/me/.local/share/oh-my-crab/memory/user","memories":[]}
```

### `Imported` and `ImportedTurn`

`crab import <id> <folder>` reads an `Imported` from standard input: a
conversation a client kept itself, which crab then saves as its own. It fails
when the conversation already has a history.

| Field      | Type                   | Presence | Meaning                                 |
| ---------- | ---------------------- | -------- | --------------------------------------- |
| `title`    | string                 | optional | The conversation's name.                |
| `settings` | list of pairs          | default  | The latest value chosen for each setting. |
| `offered`  | list of `Setting`      | default  | What could be set, as last offered.     |
| `commands` | list of `Slash`        | default  | The slash commands last offered.        |
| `window`   | integer                | optional | The context window last used.           |
| `turns`    | list of `ImportedTurn` | required | The turns, oldest first.                |

An `ImportedTurn`:

| Field    | Type           | Presence | Meaning                                                                          |
| -------- | -------------- | -------- | -------------------------------------------------------------------------------- |
| `id`     | string         | required | The turn's id.                                                                   |
| `prompt` | string         | required | What the reader prompted with.                                                   |
| `files`  | list of string | default  | The names of the files attached.                                                 |
| `said`   | string         | default  | The agent's reply, in one piece.                                                 |
| `end`    | string         | optional | How a turn without an answer ended: `stopped`, `interrupted`, or the failure.    |

```json
{"title":"Flaky test","turns":[{"id":"t-1","prompt":"Why does the test fail?","said":"The fixture is stale."}]}
```
