# Extensions

An extension is a program crab starts when a conversation opens and keeps
running until it ends. Crab speaks to it in JSON lines on its standard input
and output, so it can be written in any language. Where a
[hook](hooks.md) is a command run once for each event, an extension stays up,
keeps its own state between events, and can do more:

- watch the conversation: sessions, turns, the model's replies, tool calls,
  compactions and subagents;
- steer it where that makes sense: refuse or rewrite a tool call, decide in
  the reader's place whether a call may run, replace a tool's result, add
  context to or rewrite a prompt, keep a turn going, keep a compaction from
  happening, or stop the turn;
- give the model tools of its own.

An extension that is slow, crashes or writes nonsense never takes the
conversation down: crab waits for it only as long as its timeout, starts it
again when it stops, and turns it off for the conversation when it keeps
stopping.

`examples/extensions/guard.py` in crab's repository is a small extension in
Python with nothing but the standard library. It refuses `bash` calls that
run `rm`, rewrites `bash` calls that run `date` to run with `TZ=UTC`, tells
the model before each prompt that it is on, and gives the model a
`count_words` tool.

## Declaring extensions

Extensions are declared in JSON files of this shape:

```json
{
  "extensions": {
    "guard": {
      "command": "python3",
      "args": ["./guard.py"],
      "env": { "GUARD_LOG": "${HOME}/guard.log" },
      "timeout": 10,
      "tool_timeout": 600
    }
  }
}
```

| Field          | What it is                                                                                         | Default                    |
| -------------- | -------------------------------------------------------------------------------------------------- | -------------------------- |
| `command`      | The program. A relative path with a folder in it, such as `./guard.py`, is taken from the folder named below; a bare name is looked up on the search path. | required |
| `args`         | Its arguments.                                                                                     | none                       |
| `env`          | Variables it is started with, over crab's own environment.                                         | none                       |
| `cwd`          | The folder it is started in, taken from the folder named below.                                    | see below                  |
| `timeout`      | Seconds crab waits for its answer to `initialize` and to each event it asks about.                 | 10                         |
| `tool_timeout` | Seconds crab waits for a call of one of its tools.                                                 | 600                        |
| `disabled`     | `true` leaves it out.                                                                              | `false`                    |

Text in `command`, `args`, `env` and `cwd` may name environment variables as
`${NAME}`, or `${NAME:-fallback}` for a fallback when the variable is unset
or empty, as [tool-server files](mcp.md) do. An extension's name may hold
letters, digits, `-` and `_`.

They are read from two places:

1. Crab's own `<config>/extensions.json` (`~/.config/oh-my-crab/` on Linux;
   see [folders](../configuration.md#folders)), or the file
   `OMC_EXTENSIONS_FILE` names. These are the user's own and always start.
   Relative paths are taken from the file's folder, and an extension without
   `cwd` starts in the conversation's folder.
2. `.oh-my-crab/extensions.json` in the folders from the working folder up to
   the repository root, the nearest declaration of a name winning. These are
   the project's and start only once the project is trusted, as its hooks and
   tool servers do (see [project trust](project-trust.md)): while it is not,
   each is listed in the `Trust` event as a hook named
   `extension <name>: <command line>`, and trusting the project records it
   with the hooks, so a changed command line waits for trust again. Relative
   paths are taken from the project folder that holds `.oh-my-crab`, which is
   also where an extension without `cwd` starts.

The user's extensions run first, then the project's; a project extension
named like one of the user's is left out. Set `OMC_EXTENSIONS=0` to start no
extension at all. Extensions start when the conversation opens, before its
first turn, so their tools are part of the tool list from the start; those of
a project trusted later start before the next turn. When the conversation is
closed, or another one is opened in its place, each is told `session_end`,
sent `shutdown`, its input is closed, and it is killed when it has not ended
within a second.

## The protocol

Every message is one JSON-RPC 2.0 object on one line, in UTF-8. Crab writes
requests and notifications to the extension's standard input and reads its
answers from its standard output. What the extension writes to standard
error is passed on to crab's standard error, one line at a time, under its
name. A line on standard output that is not JSON is reported and skipped.

A request carries an `id` and must be answered with the same `id` and a
`result`, or an `error` object with a `message`, which counts as no answer.
A notification carries no `id` and gets no answer. Answers may come in any
order.

### `initialize`

The first request, sent once the program started and again each time it is
started after it stopped:

```json
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocol":1,"crab_version":"0.1.0","name":"guard","session_id":"c-1","cwd":"/work/app"}}
```

The answer says which events the extension wants and which tools it gives:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "events":["tool_call","turn_start"],
  "tools":[{"name":"count_words","description":"Counts the words in a text.",
            "input_schema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"]},
            "read_only":true}]}}
```

An extension that does not answer within its `timeout`, or answers with
something that is not an object, failed to start; it is reported and left out
for the conversation. The events are read again on each start; the tools only
the first time, since the model's tool list is fixed for the conversation.

A tool has a `name` of at most 64 letters, digits, `_` and `-`, a
`description` for the model, and an `input_schema`, a JSON schema of an
object (one that takes any object by default). `read_only: true` says it
only looks, so plan mode and read-only subagents keep it; `approval: true`
has the reader asked before each call, as for tools that change things. A
tool named like one crab already has is left out.

### `event`

Crab tells the extension of each event it wants with an `event` message. The
`event` field names it, and every event carries `session_id`, `cwd` and
`permission_mode` (`default`, `plan` or `bypassPermissions` in yolo mode),
and `agent_id` and `agent_type` when a subagent is the one acting.

Events that can steer something are requests: crab waits for the answer, at
most the extension's `timeout`. `null`, an empty object, an error and no
answer all mean no opinion, except for `tool_call`, where no answer in time,
an error or a crash refuses the call, since the check never happened.
Events that only tell are notifications.

```json
{"jsonrpc":"2.0","id":7,"method":"event","params":{"event":"tool_call","session_id":"c-1","cwd":"/work/app","permission_mode":"default","tool_name":"bash","tool_input":{"command":"rm -rf build"},"tool_use_id":"toolu_01"}}
{"jsonrpc":"2.0","id":7,"result":{"block":true,"reason":"the guard extension does not allow rm"}}
```

Several extensions that want the same event are asked one after another, in
the order they were declared, each seeing what the ones before it changed: a
rewritten tool call, prompt or result. The first one that blocks ends the
round. Extensions run after the [hooks](hooks.md) of the same point, and see
what the hooks left.

| Event | When | Fields besides the common ones | Kind | What the answer may hold |
| ----- | ---- | ------------------------------ | ---- | ------------------------ |
| `session_start` | The conversation opened (`startup`), opened in place of another (`clear`), a saved one opened again (`resume`), or it was just compacted (`compact`) | `source`, `model` | request | `context`: told to the model at the start of the next prompt, in a `<session-start-extension>` notice |
| `session_end` | The conversation is closed or replaced | `reason`: `prompt_input_exit`, `other`, `clear` or `resume` | notification | |
| `turn_start` | A prompt is about to start a turn | `prompt` | request | `block: true` with `reason`: the turn fails with the reason and the model is not asked; `prompt`: sent in place of the reader's words; `context`: told to the model before the prompt, in an `<extension-context>` block |
| `turn_end` | A turn ended | `error`: why it failed, or `null` | notification | |
| `message` | The model replied, before its calls run | `role` (`assistant`), `text`, `thinking`, `tool_calls` (each `id`, `name`, `arguments`) | notification | |
| `tool_call` | Before a tool call is asked about or run | `tool_name`, `tool_input` (the arguments, put right against the tool's schema), `tool_use_id` | request | `block: true` with `reason`: the call is refused and the model reads the reason; `input`: arguments in place of the model's, put right against the schema again; `decision`: `allow` runs it without asking the reader, `ask` has the reader asked, `deny` refuses it like `block`; `context`: a note added to the result; `stop`: the turn stops once the batch of calls ends, with this reason |
| `permission_request` | The reader is about to be asked whether a call may run | `tool_name`, `tool_input` | request | `decision`: `allow` runs it, with `input` as its arguments when given and `rules` (permission rules in Claude Code's syntax, such as `Bash(npm test)`) allowed for the rest of the conversation; `deny` refuses it with `reason`, and `stop: true` also stops the turn |
| `tool_result` | A tool call ended | the fields of `tool_call`, and `output`, `is_error`, and `duration_ms` for a failure | request | `output`: what the model reads in place of the result; `context`: a note added to the result; `stop` |
| `tool_batch` | Every call of one reply has its result | `calls`: each with the fields of `tool_call`, `output` and `is_error` | request | `context`: a note after the results; `stop`: the turn ends instead of sending the results to the model |
| `stop` | The main agent is about to end its turn | `answer`, `again` (whether it already went on because of such a check since it last called a tool) | request | `continue`: the turn goes on, the model told this reason; bounded as for `Stop` hooks |
| `subagent_start` | A subagent is about to start | `agent_id`, `agent_type` | request | `context`: put before its assignment |
| `subagent_stop` | A subagent is about to end its work | `answer`, `again` | request | `continue`, as for `stop` |
| `compact_start` | Before the conversation is summarized | `trigger` (`manual` or `auto`), `focus` (what followed `/compact`, or `null`) | request | `block: true` with `reason`: the compaction does not happen, as when a `PreCompact` hook blocks it; `instructions`: added to what the summary should give attention to |
| `compact_end` | The conversation was summarized | `trigger`, `summary` | notification | |
| `notification` | Crab waits on the reader | `kind` (`permission_prompt`, `elicitation_dialog` or `idle_prompt`), `title`, `message` | notification | |
| `model_change` | The `model` setting changed | `from`, `to` | notification | |

A turn that stops because of `stop` keeps the results of its last batch, with
a note giving the reason that the model reads with the next prompt, and ends
as one that finished; the reason is also written to crab's standard error.

### `tool`

When the model calls one of the extension's tools, crab asks:

```json
{"jsonrpc":"2.0","id":9,"method":"tool","params":{"name":"count_words","call_id":"toolu_02","arguments":{"text":"one two three"}}}
{"jsonrpc":"2.0","id":9,"result":{"content":"3 words"}}
```

`content` is the text the model reads, or a list of `{"type":"text","text":…}`
blocks, which are joined. `is_error: true` makes the call a failure. The
call goes through everything a call of crab's own tools does first: hooks,
the `tool_call` event of every extension, permission rules and, with
`approval`, the reader. A call that gets no answer within `tool_timeout`, or
whose extension stops, fails with the reason; an extension that is not
running is started first.

### What an extension may send

| Message | Kind | What crab does |
| ------- | ---- | -------------- |
| `log` with `message` and an optional `level` | notification | Writes it to crab's standard error under the extension's name. |

Crab answers any request from an extension with an error, since it offers
none yet.

### `shutdown`

A notification sent when the conversation ends, after `session_end`. The
extension should exit; crab also closes its standard input, and kills it a
second later.

## When things go wrong

- An extension that does not answer an event in time is taken to have no
  opinion, except that a `tool_call` it does not answer is refused, with the
  reason the model reads. A turn never waits on an extension longer than its
  `timeout`.
- An extension that exits, or whose output ends, is started again, with
  `initialize`, the next time crab needs it. Requests it had not answered get
  no answer. When it stops more than three times within five minutes it stays
  off for the rest of the conversation: it is told nothing, and its tools fail
  saying so. Each stop and restart is reported on crab's standard error.
- An extension that cannot be started, or does not answer `initialize`, is
  reported and left out for the conversation.
- Answers to requests that already timed out, and answers with an `id` crab
  does not know, are ignored.
