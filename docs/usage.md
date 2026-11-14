# Commands

```text
crab serve                      hold a conversation over standard input and output
crab exec [options] [prompt]    run one prompt with nobody asked and print the answer
crab tools                      print the tools the agent can have, as a JSON list
crab providers                  print the providers, as a JSON list of id, name and sign-in
crab account <p> [folder]       print who provider <p> is signed in as, or nothing
crab limits [p] [--json]        print how much of its subscription allowance each signed-in provider has used
crab history <id> [folder]      print a saved conversation: title, settings, turns and branches
crab rename <id> <folder> <t>   name a saved conversation nobody has open
crab import <id> <folder>       save a conversation a client kept itself, read as JSON
crab fork <id> <copy> <folder> [turn]  copy a conversation's branch, up to before a turn, to a new one
crab memory [folder]            print the memories a conversation in the folder starts with, as JSON
crab hooks [folder]             print the hooks a conversation in the folder would find and its project's trust, as JSON
crab trust <folder> [on|off]    print whether the folder's project is trusted, or record that it is or is not
crab permissions [folder]       print the permission rules a conversation in the folder would use, as JSON
crab permissions add <allow|ask|deny> <user|project> <rule> [folder]
                                add a rule to crab's permissions file
crab permissions remove <rule> [folder]
                                take a rule out of crab's permissions file
```

`crab --help` prints the same list; `crab` with no subcommand or an unknown
one prints it as an error.

- **`serve`** reads [commands](wire-protocol.md) from standard input and
  writes events to standard output, one JSON message per line, until `Close`
  or the end of standard input. It holds one conversation at a time; an `Open`
  for another one replaces it. [Getting started](getting-started.md) walks
  through a session.
- **`exec`** runs one prompt, given as words or on standard input, in the
  current folder or `--folder`, with every tool allowed and nobody asked:
  approvals and questions are turned down, the project's hooks, instruction
  files, skills and agents are used only with `--trust`, and the turn is
  stopped after `--timeout` seconds. `--provider` and `--model` pick the
  model, signed in as for `serve`, such as `OPENROUTER_API_KEY` for
  `openrouter`. It prints what the agent said after its last tool call, or
  with `--json` one object: `result`, `is_error`, `error`, `requests`,
  `input_tokens` (every token sent, summed over the requests), `output_tokens`,
  `tool_calls` and `cost_usd`, what the run cost in US dollars at the models'
  list prices with subagents included, left out when no model's prices are
  known. It exits 1 when the turn fails. For scripts, CI and
  sandboxes: `crab exec --provider openrouter --model anthropic/claude-sonnet-4.5 --json "fix the failing test"`.
- **`limits [provider] [--json] [--folder <folder>]`** asks each signed-in
  provider that meters a subscription allowance how much of it the account has
  used, or only `provider`, and prints a bar, a share and the time to the reset
  for each window. `--json` prints a list of `Limits` instead, see the
  [wire protocol](wire-protocol.md#limits-limit-and-limitunit). `--folder` reads
  the logins kept in another folder. A provider that cannot be asked, or whose
  account has no plan, is reported on standard error and left out.
- **`tools`** lists the built-in tools by name with the first sentence of
  each description, in name order. Tool-server tools are not listed, since
  the client configures them, and neither are the tools that only appear in
  some conversations: `recall`, `checkpoint`, `rewind` (with the
  `checkpoints` setting on) and `submit_result` (for subagents asked for a
  typed result). The memory tools are left out when `OMC_MEMORY=0`. See the
  [tool index](tools/README.md).
- **`providers`** lists every provider, built-in and from `providers.json`,
  with how it signs in and the variables its key is read from. See
  [providers](providers/overview.md).
- **`account`** prints the account a browser or device-code login reported,
  from `auth.json` in `folder` or in crab's config folder
(`~/.config/oh-my-crab/` on Linux), and prints
  nothing when there is none.
- **`history`** prints the conversation saved under `id`, in the history
  `folder` or the default one, as the events a live conversation would have
  sent, with the branch those turns are of and every branch of the
  conversation (see [branches](wire-protocol.md#branches)). It works while the
  conversation is open elsewhere.
- **`rename`** names a saved conversation, and fails while another process
  has it open. A client with the conversation open sends `Rename` instead.
- **`import`** reads a conversation a client kept itself as JSON on standard
  input and saves it under `id`, so that `crab` can open it from then on. It
  refuses a conversation that already has a history.
- **`fork`** copies the branch the conversation `id` in `folder` is on, up to
  before the turn `turn` or whole, to the new conversation `copy` in the same
  folder: its turns, name, settings, notebook and what was saved with it. The
  copy has one branch, and a client opens it with `Open` like any other. It
  works while `id` is open elsewhere, and refuses a `copy` that already has a
  history.
- **`memory`** prints what a conversation in the folder would start with. See
  [memory](memory.md).
- **`hooks`** prints every hook found, whether it runs, and what an
  untrusted project leaves out. See [hooks](extending/hooks.md).
- **`trust`** prints or records whether a project is trusted, for every
  worktree of its repository. See [project trust](extending/project-trust.md).
- **`permissions`** prints every permission rule found, where it came from
  and whether it is used, or adds a rule to crab's permissions file for the
  user or the folder's project, or takes one out. See
  [permission rules](extending/permissions.md).

## Session commands

A conversation held by `serve` understands these commands, sent as a prompt
starting with `/` or as a wire command of the same name. Each takes no files
and, but for `/btw`, is refused while a turn runs.

| Prompt            | Wire command                    | What it does                                                                                                     |
| ----------------- | ------------------------------- | ---------------------------------------------------------------------------------------------------------------- |
| `/retry`          | `"Retry"`                       | Takes the last reply back out and answers the last prompt again, under the turn id it was saved with.            |
| `/fresh`          | `"Fresh"`                       | Empties the conversation's context; the model, settings, name, goal and notebook stay.                          |
| `/btw <question>` | `{"Btw":{"question":"..."}}`    | Asks the model, the `fast` slot's when it is set, a question about the conversation on the side, without tools; the answer comes as an `Aside` event and is not added to the conversation. Allowed while a turn runs. |
| `/handoff [goal]` | `{"Handoff":{"goal":"..."}}`    | Has the model, the `fast` slot's when it is set, write a hand-off prompt, toward `goal` when one is given, then empties the conversation and starts it again with that prompt, telling the client with `HandedOff`. |

A saved conversation keeps the turns `/fresh` and `/handoff` took out,
recorded as lost, so a replayed history still shows them. The
[wire protocol](wire-protocol.md#retry) gives the events each sends.

How saved conversations, queued prompts and the rest fit together is in
[concepts](concepts.md). The commands and events are defined in
`crates/omc-wire`. `crates/omc-client` is a small blocking client for programs
that start `crab` and talk to it.
