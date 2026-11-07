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
  `LanguageAsk` (`format`, `diagnostics`), `LanguageAnswer` (`formatted`,
  `diagnostics`), `Severity` (`error`, `warning`, `information`, `hint`),
  `SignIn` (`browser`, `device_code`, `api_key`, `none`) and `KeySource`,
  which is internally tagged by a `from` field.
- **Field names** are snake_case as written in Rust.
- **Optional fields** (marked "optional" in the tables below) are written as
  `null` when empty; crab always writes them. A message sent to crab may
  leave them out, which is the same as `null`.
- **Defaulted fields** (marked "default") may be left out when sent to crab:
  a list defaults to `[]`, a string to `""`, a boolean to `false`. Crab still
  writes them in what it sends, except `key_env` in `ProviderInfo`, which is
  left out when empty.
- **Paths** are JSON strings.
- **Pairs** (`(String, String)`) are two-element arrays: `["effort","high"]`.
- **Integers** are unsigned: tickets and token counts are `u64`.

Fields not marked optional or default are required; a command that leaves one
out is refused as a bad command.

## Lifecycle

1. The client starts `crab serve`.
2. The client sends `Open`. Crab answers with `Settings` and `Commands`, then,
   for a conversation opened with an id, `Titled` when it has a name and
   `Resumed` with its saved turns, then `Trust` when the project brings
   something that is left out until it is trusted. When opening fails, crab
   sends `Failed` or `SignedOut` instead, and the client can send `Open`
   again.
3. The client sends `Prompt`. Crab streams the turn as events: `Started`,
   `Said`, `Thought`, `Tool`, `Used`, `Plan`, `Background`, and requests that
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
{"Ask":{"ticket":1,"title":"edit: calc.py","command":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
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

The shortest valid `Open`:

```json
{"Open":{"directory":"/path/to/project","provider":"claude","yolo":false}}
```

With every field:

```json
{"Open":{"directory":"/home/me/project","home":null,"provider":"claude","model":"claude-sonnet-4-5","yolo":false,"tools":[{"name":"files","reach":{"Command":{"program":"mcp-files","args":["--root","."],"env":[["LOG","warn"]]}}},{"name":"issues","reach":{"Http":{"url":"https://example.com/mcp","headers":[["Authorization","Bearer x"]]}}}],"denied":["web_search"],"disabled":["AGENTS.md"],"conversation":"c-42","history":null,"keep":50,"defaults":[["effort","high"]],"before":null,"languages":true}}
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
| `text`    | string         | required | What the reader wrote. `/compact` and `/<skill>` are understood here.                                       |
| `files`   | list of path   | required | Files attached, by path.                                                                                    |
| `turn`    | string         | optional | The id the client knows the turn by; saved with the turn so the client can go back to before it.            |
| `context` | string         | optional | Words the model is told before the reader's, which are not saved as what the reader wrote.                  |

```json
{"Prompt":{"text":"Fix the failing test","files":["/home/me/project/log.txt"],"turn":"t-7","context":null}}
```

`/compact`, optionally followed by what the summary should keep, compacts the
conversation instead of starting a normal turn; it takes no files and is
refused while a turn runs.

### `Choose`

Answers an `Ask` under `ticket`.

| Field    | Type    | Presence | Meaning                                                    |
| -------- | ------- | -------- | ---------------------------------------------------------- |
| `ticket` | integer | required | The `Ask` being answered.                                  |
| `option` | string  | optional | The `id` of the `Choice` picked; `null` declines.          |

Any `option` other than `allow` or `always` declines, as `null` does.

```json
{"Choose":{"ticket":1,"option":"allow"}}
{"Choose":{"ticket":1,"option":null}}
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

```json
{"Language":{"ticket":4,"answer":{"formatted":{"text":"fn main() {}\n"}}}}
{"Language":{"ticket":5,"answer":{"diagnostics":{"diagnostics":[{"path":"/home/me/project/src/lib.rs","line":3,"column":9,"severity":"error","message":"mismatched types","source":"rustc","code":"E0308"}]}}}}
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

### `Close`

Ends the conversation, and the `crab` process exits. See
[Lifecycle](#lifecycle).

```json
"Close"
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

| Field  | Type    | Meaning                                     |
| ------ | ------- | ------------------------------------------- |
| `used` | integer | Tokens the conversation holds.              |
| `size` | integer | Tokens the model's context window takes.    |

```json
{"Used":{"used":22927,"size":1000000}}
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
| `worktree` | `Worktree`  | optional | The git worktree an isolated subagent works in, and what became of its changes.         |

`Kind` is one of `Read`, `Edit`, `Search`, `Execute`, `Fetch` and `Other`.
`read` of a web address is `Fetch`; `debug` is `Execute` when it launches,
attaches, continues or steps the program, and `Other` otherwise; `lsp` is
`Edit` when it renames (not as a dry run), and `Search` otherwise.

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

A tool call offers `allow` ("Allow once"), `always` ("Always allow", or
"Always allow <scope>" when the approval covers less than the whole tool) and
`reject`. A plan offers `allow` ("Approve and start") and `reject` ("Keep
planning").

From a real run:

```json
{"Ask":{"ticket":1,"title":"edit: calc.py","command":"calc.py\nreplace lines 2-2\n-     return a - b\n+     return a + b","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
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

Only these two are asked. The `lsp` tool's `diagnostics` of a file go to the
client as a `diagnostics` request; its other actions (definitions,
references, hovers, symbols, renames and the diagnostics of the whole project)
are not sent to the client, and the tool tells the model they cannot be
answered in this conversation.

```json
{"Language":{"ticket":4,"ask":{"format":{"path":"/home/me/project/src/main.rs"}}}}
{"Language":{"ticket":5,"ask":{"diagnostics":{"paths":["/home/me/project/src/lib.rs"],"wait_ms":3000}}}}
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

```json
[{"id":"claude","name":"Claude","sign_in":"browser"},{"id":"ollama","name":"Ollama","sign_in":"none"},{"id":"openrouter","name":"OpenRouter","sign_in":"api_key","key_env":["OPENROUTER_API_KEY"]},{"id":"copilot","name":"GitHub Copilot","sign_in":"device_code"},...]
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
