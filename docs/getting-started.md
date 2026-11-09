# Getting started

This page installs `crab`, signs it in to a provider and holds a first
conversation with it by hand, typing JSON lines into `crab serve`. You would
not normally do this: [pandemonium](https://github.com/JoakimCarlsson/pandemonium)
is the usual client, and it starts `crab`, signs in and draws the conversation
for you. Talking to `crab` directly is the quickest way to see what a client
sees, and what every other page of these docs assumes.

The output below is from a real session against the `claude` provider, in a
small git repository holding `README.md`, `greet.py` and `AGENTS.md`. Long
lines are cut with `…`, runs of `Thought` and `Said` chunks are left out, and
the repository's folder is written `/path/to/hello`.

## Install

You need a Rust toolchain. From the root of this repository:

```sh
make install
```

This runs `cargo install` for the `omc` crate and puts the `crab` binary in
`~/.local/bin`. Set `PREFIX` to install somewhere else, as in
`make install PREFIX=/usr/local`. Running `crab` with no subcommand prints the
list of subcommands; [commands](usage.md) describes each.

## Sign in to a provider

`crab providers` lists every provider a conversation can be opened with and
how each one signs in:

```console
$ crab providers
[{"id":"claude","name":"Claude","sign_in":"browser"},{"id":"codex","name":"Codex","sign_in":"browser"},{"id":"ollama","name":"Ollama","sign_in":"none"},{"id":"openrouter","name":"OpenRouter","sign_in":"api_key","key_env":["OPENROUTER_API_KEY"]},{"id":"copilot","name":"GitHub Copilot","sign_in":"device_code"},{"id":"kimi-code","name":"Kimi Code","sign_in":"device_code","key_env":["KIMI_CODE_API_KEY","KIMI_API_KEY"]},…]
```

There are four ways in:

- **`api_key`**: export the variable `key_env` names before starting `crab`,
  for example `OPENROUTER_API_KEY`. Without it, opening a conversation fails:

  ```json
  {"Failed":"no API key found; set OPENAI_API_KEY"}
  ```

  A program that links the harness can also save a key with
  `omc_session::save_key`; a key in the environment wins over a saved one.
- **`browser`** (`claude`, `codex`) and **`device_code`** (`copilot`,
  `kimi-code`): sign in over the wire with the `Login` command, shown below.
  `kimi-code` also takes a key from the variables it lists.
- **`none`**: local servers such as `ollama` need nothing.

The browser and device-code logins are saved in `auth.json` under
`oh-my-crab` in the user's config folder (`~/.config/oh-my-crab/` on Linux),
or in the folder a client passes as `home` when it
opens a conversation, so that it can keep several accounts apart. Tokens are
refreshed by themselves when they expire. `crab account` prints who a provider
is signed in as, or nothing when it is not:

```console
$ crab account claude
you@example.com
```

[Providers](providers/overview.md) lists every provider, its key variable and
its default model.

### Signing in over the wire

Start `crab serve` and open a conversation. When there is no login, `crab`
says so instead of opening it:

```json
> {"Open":{"directory":"/path/to/hello","home":"/path/to/empty-home","provider":"claude","model":null,"yolo":false}}
< {"SignedOut":{"reason":"not logged in","account":null}}
```

Lines starting with `>` are what you type and lines starting with `<` are what
`crab` prints; the markers are not part of the protocol. Send `Login` with the
provider's id, and `crab` answers with the page to open:

```json
> {"Login":{"provider":"claude"}}
< {"LoginUrl":"https://claude.ai/oauth/authorize?response_type=code&client_id=…&redirect_uri=http%3A%2F%2Flocalhost%3A54545%2Fcallback…"}
```

`crab` never opens a browser itself, so open the address yourself. A
device-code provider sends `{"LoginCode":{"url":…,"code":…}}` instead, with a
code to type on that page. When you finish, `crab` prints `"SignedIn"`, or a
`Failed` event with the reason. Then send the same `Open` again.

## Open a conversation

Make a scratch git repository to work in, then start the harness:

```console
$ crab serve
```

`crab` reads one command per line on standard input and writes one event per
line on standard output. Open the conversation:

```json
> {"Open":{"directory":"/path/to/hello","provider":"claude","model":null,"yolo":false,"conversation":"first-chat"}}
< {"Settings":[{"id":"model","category":"model","name":"Model","current":"claude-haiku-5-5","picks":[…]},{"id":"mode","category":"mode","name":"Mode","current":"default","picks":[{"value":"default","name":"Ask",…},{"value":"yolo","name":"Run freely",…},{"value":"plan","name":"Plan",…}]},{"id":"effort",…},{"id":"cache",…},{"id":"cache_warming",…},{"id":"memory",…},{"id":"plan_model",…},{"id":"plan_autosave",…},{"id":"checkpoints",…}]}
< {"Commands":[{"name":"compact","description":"Summarize the older part of the conversation to free up context; …"},…]}
< {"Resumed":{"title":null,"settings":[],"turns":[],"offered":[…],"commands":[…],"window":1000000}}
< {"Trust":{"ticket":1,"root":"/path/to/hello","standing":"Unknown","skipped":[{"kind":"Instruction","name":"/path/to/hello/AGENTS.md","path":"/path/to/hello/AGENTS.md"}]}}
```

What came back:

- `Settings`: everything the client can change, with the value each has now.
  `model: null` took the provider's first model.
- `Commands`: what can follow a `/` in a prompt: `/compact`, and one command
  per skill found on disk.
- `Resumed`: the saved conversation. `conversation` gives it an id, so `crab`
  saves it and can open it again later; this one is new, so it has no turns.
  Leave `conversation` out and nothing is saved.
- `Trust`: the repository has an `AGENTS.md`, which `crab` does not read until
  you trust the project, since a cloned repository could use it to steer the
  agent. See [project trust](extending/project-trust.md).

Trust it for this conversation only:

```json
> {"Trust":{"ticket":1,"verdict":"Allow"}}
```

`Always` would record the decision for good, and `Reject` would record that
the project is not trusted.

## Send a prompt

A prompt starts a turn. `turn` is an id of your choosing that the turn is
saved under; it can be left out.

```json
> {"Prompt":{"text":"In one sentence, what does this project do?","files":[],"turn":"t1"}}
< {"Started":"Thinking"}
< {"Thought":"The question"}
< {"Thought":" it"}
…
< {"Used":{"used":23012,"size":1000000}}
< {"Tool":{"id":"toolu_01J33J6NoCqxrtykWj4hWQFh","title":"read","kind":"Read","state":"Running","target":"README.md","diff":null,"output":null,"parent":null,"agent":null,"worktree":null}}
< {"Tool":{"id":"toolu_013UzKYYVQq8qhrAuYQ1r9UQ","title":"set_title","kind":"Other","state":"Running","target":"Summarize project purpose in one sentence",…}}
< {"Titled":"Summarize project purpose in one sentence"}
< {"Tool":{"id":"toolu_013UzKYYVQq8qhrAuYQ1r9UQ","title":null,"kind":null,"state":"Done","target":null,"diff":null,"output":"Named the conversation: Summarize project purpose in one sentence",…}}
< {"Tool":{"id":"toolu_01J33J6NoCqxrtykWj4hWQFh","title":null,"kind":null,"state":"Done","target":null,"diff":null,"output":"[tag 589D]\n     1\t# hello\n     2\t\n     3\tA tiny greeter.\n",…}}
…
< {"Started":"Text"}
< {"Said":"This project is a tiny"}
< {"Said":" Python greeter:"}
…
< {"Used":{"used":23416,"size":1000000}}
< {"Finished":null}
```

How to read it:

- `Started` opens a block of the reply, `Thinking` or `Text`; the `Thought`
  or `Said` chunks after it belong to that block. Join them to get the text.
- `Tool` reports a tool call. The first event for an `id` says what the call
  is; later events for the same `id` update it, here from `Running` to `Done`
  with the output. Fields that do not change are `null`.
- `Used` is how full the context window is, in tokens.
- `Titled` is the agent naming the conversation with its `set_title` tool.
- `Finished` ends the turn: `null` when it went well, or the reason it failed.

Read-only tools such as `read` ran without asking. See
[how tool calls run](tools/calls.md) and the [tool index](tools/README.md).

## Approve a tool call

Tools that change files or run commands ask first. Ask for a change:

```json
> {"Prompt":{"text":"Add test_greet.py with one plain assert for greet.","files":[],"turn":"t2"}}
< {"Used":{"used":23486,"size":1000000}}
< {"Tool":{"id":"toolu_01DaXrXmX4fnwJ27Zr2ZsYt8","title":"write","kind":"Edit","state":"Running","target":"test_greet.py (5 lines, 89 bytes)\n+ from greet import greet\n+ \n+ \n+ def test_greet():\n+     assert greet(\"World\") == \"Hello, World!\"",…}}
< {"Ask":{"ticket":1,"title":"write: test_greet.py (5 lines, 89 bytes)","command":"test_greet.py (5 lines, 89 bytes)\n+ from greet import greet\n…","plan":null,"choices":[{"id":"allow","name":"Allow once","verdict":"Allow"},{"id":"always","name":"Always allow Edit(/**)","verdict":"Always"},{"id":"always-project","name":"Always allow Edit(/**) in this project","verdict":"Always"},{"id":"always-user","name":"Always allow Edit(//path/to/hello/**) in every project","verdict":"Always"},{"id":"reject","name":"Reject","verdict":"Reject"}]}}
```

The turn now waits. Answer with `Choose`, naming the ticket and one of the
choices' ids:

```json
> {"Choose":{"ticket":1,"option":"allow"}}
< {"Tool":{"id":"toolu_01DaXrXmX4fnwJ27Zr2ZsYt8","title":null,"kind":null,"state":"Done","target":null,"diff":null,"output":"Created test_greet.py (89 bytes)",…}}
…
< {"Tool":{"id":"toolu_01HTYDkC8yGY6ggkUGBxiuak","title":"bash","kind":"Execute","state":"Running","target":"python3 -m pytest -q",…}}
< {"Ask":{"ticket":2,"title":"bash: python3 -m pytest -q","command":"python3 -m pytest -q","plan":null,"choices":[…]}}
> {"Choose":{"ticket":2,"option":"allow"}}
…
< {"Finished":null}
```

`allow` lets this one call run. `always` adds the permission rules its name
shows for the rest of the conversation: for `edit` and `write` the folder,
such as `Edit(/**)` here, for `bash` a rule per command, such as
`Bash(python -m pytest)` or `Bash(git status:*)`, and for reading a web page
the host. `always-project` and `always-user`, when offered, also keep the
rules in crab's permissions file for later conversations; see
[permission rules](extending/permissions.md). `reject`, or `"option":null`, refuses the call, and the model is told it
was refused. See [concepts](concepts.md#tool-calls-and-approvals).

## Plan mode

The `mode` setting has three values: `default` asks before changing anything,
`plan` only looks around and then asks you to approve a plan, and `yolo` runs
everything without asking. Change it with `Set`; `crab` answers with the
settings as they now stand.

This session was closed and opened again with the same `Open` line. The
`Resumed` event then carried the two earlier turns, and the agent went on from
them:

```json
> {"Set":{"setting":"mode","value":"plan"}}
> {"Prompt":{"text":"Plan adding a --name command-line flag to greet.py, in at most three steps.","files":[],"turn":"t3"}}
< {"Settings":[…,{"id":"mode","category":"mode","name":"Mode","current":"plan",…},…]}
< {"Started":"Thinking"}
…
< {"Tool":{"id":"toolu_01XsCGhgB92oGsMdZuHkfF8f","title":"exit_plan_mode","kind":"Other","state":"Running","target":"Context",…}}
< {"Ask":{"ticket":1,"title":"Plan ready for review","command":null,"plan":"## Context\n\n`greet.py` currently only defines `greet(name)`, …","choices":[{"id":"allow","name":"Approve and execute","verdict":"Allow"},{"id":"fresh","name":"Approve and execute in a fresh context","verdict":"Allow"},{"id":"reject","name":"Keep planning","verdict":"Reject"},{"id":"discard","name":"Reject the plan","verdict":"Reject"}]}}
```

In plan mode the agent can read, search and ask, but a call to a tool that
changes things is refused without running. It finishes by handing its plan to
`exit_plan_mode`, which you see as an `Ask` with a `plan`. `reject` ("Keep
planning") sends it back to be revised, and a `note` sent with the choice
says what should change:

```json
> {"Choose":{"ticket":1,"option":"reject","note":"Keep the default greeting when --name is missing."}}
```

`fresh` approves it to be carried out in a fresh context window that starts
from the plan alone, and `discard` rejects it outright. Approving it with
`allow` ends plan mode and returns to the mode before it, here `default`, and
the agent starts on the plan in the same turn:

```json
> {"Choose":{"ticket":1,"option":"allow"}}
< {"Settings":[…,{"id":"mode",…,"current":"default",…},…]}
< {"Tool":{"id":"toolu_01XsCGhgB92oGsMdZuHkfF8f",…,"state":"Done",…,"output":"Plan approved. Plan mode is over and full tool access is restored.\n…"}}
< {"Tool":{"id":"toolu_014Q4zmxVjqUj4Rjb6bDeJCG","title":"todo","kind":"Other","state":"Running","target":"3 tasks",…}}
< {"Tool":{"id":"toolu_01UwNjXhpABJ5bmkc2hgrzZW","title":"edit","kind":"Edit","state":"Running","target":"greet.py\nreplace lines 1-2\n- def greet(name):\n-     return f\"Hello, {name}!\"\n+ import argparse\n…",…}}
< {"Ask":{"ticket":2,"title":"edit: greet.py",…}}
> {"Choose":{"ticket":2,"option":"always"}}
< {"Plan":[{"text":"Add --name flag to greet.py","progress":"Doing"},{"text":"Add CLI tests to test_greet.py","progress":"Pending"},{"text":"Verify with pytest and smoke runs","progress":"Pending"}]}
< {"Tool":{"id":"toolu_01UwNjXhpABJ5bmkc2hgrzZW",…,"state":"Done",…,"diff":{"path":"greet.py","old":"def greet(name):\n    return f\"Hello, {name}!\"\n","new":"import argparse\n…"},…}}
…
< {"Finished":null}
```

`Plan` is the agent's todo list, sent whole each time it changes. An `edit`
event carries the file's `diff` once it is done. After `always` for `edit`,
later edits in the project folder in this conversation did not ask again.
`bash` asked once more, was answered `always` too, and later runs of that
command ran without asking, while other commands still asked.

## Yolo mode

In `yolo` nothing asks:

```json
> {"Set":{"setting":"mode","value":"yolo"}}
> {"Prompt":{"text":"Run greet.py with --name Crab.","files":[],"turn":"t4"}}
< {"Settings":[…,{"id":"mode",…,"current":"yolo",…},…]}
< {"Used":{"used":34602,"size":1000000}}
< {"Tool":{"id":"toolu_01QsWMtAE9SRrFy4V4SsLAue","title":"bash","kind":"Execute","state":"Running","target":"python3 greet.py --name Crab; rm -rf __pycache__",…}}
< {"Tool":{"id":"toolu_01QsWMtAE9SRrFy4V4SsLAue",…,"state":"Done",…,"output":"exit code: 0\n--- stdout ---\nHello, Crab!",…}}
< {"Started":"Text"}
< {"Said":"Running `python"}
< {"Said":"3 greet.py --name"}
< {"Said":" Crab` printed `"}
< {"Said":"Hello, Crab!`"}
< {"Said":" and exited with status 0."}
< {"Used":{"used":34699,"size":1000000}}
< {"Finished":null}
```

`"yolo":true` in `Open` starts a conversation in this mode. A setting chosen
with `Set` is saved with the conversation and wins when it is opened again, so
this conversation will reopen in `yolo` whatever `Open` says.

## Stop a turn

A prompt sent while a turn runs is queued rather than dropped, and `Cancel`
stops the turn. Here a second prompt was sent while the first was still
answering:

```json
> {"Prompt":{"text":"Count from 1 to 40 in words, one per line.","files":[]}}
< {"Started":"Text"}
> {"Prompt":{"text":"Then say done.","files":[],"turn":"t6"}}
< {"Queued":{"ticket":1,"turn":"t6","text":"Then say done.","files":[],"state":"Waiting"}}
> "Cancel"
< "Halted"
< {"Finished":null}
< {"Queued":{"ticket":1,"turn":"t6","text":"Then say done.","files":[],"state":"Dropped"}}
```

Commands and events without fields, such as `Cancel`, `Close`, `Halted` and
`SignedIn`, are bare JSON strings. Had the turn gone on, the waiting prompt
would have been put into it after its current tool calls; see
[queued prompts and steering](concepts.md#queued-prompts-and-steering).

## Close, and open again

```json
> "Close"
```

`Close`, or the end of standard input, ends the conversation and the process.
Commands left running in the background are stopped.

Because the conversation was opened with an id, it was saved after every
step. `crab history` prints it without opening it:

```console
$ crab history first-chat
{"title":"Summarize project purpose in one sentence","settings":[["mode","yolo"]],"turns":[{"id":"t1","prompt":"In one sentence, what does this project do?","files":[],"events":[{"Started":"Thinking"},{"Thought":"The question itself is simple, …"},…,{"Started":"Text"},{"Said":"This project is a tiny Python greeter: `greet.py` defines a `greet(name)` function that returns `\"Hello, {name}!\"`."},{"Used":{"used":23416,"size":1000000}}]},{"id":"t2",…},{"id":"t3",…},{"id":"t4",…}],"offered":[…],"commands":[…],"window":1000000}
```

Sending the same `Open` again brings it all back in a `Resumed` event, and the
model sees the whole conversation. Chunks of a reply are joined into one
`Said` or `Thought` in the saved turns.

## Next

- [Concepts](concepts.md): the ideas behind what you just saw.
- [Wire protocol](wire-protocol.md): every command, event and field.
- [Configuration](configuration.md): files, environment variables and
  settings.
- [Tool index](tools/README.md): what the agent can do.
