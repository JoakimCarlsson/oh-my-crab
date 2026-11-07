# Commands

```text
crab serve                      hold a conversation over standard input and output
crab tools                      print the tools the agent can have, as a JSON list
crab providers                  print the providers, as a JSON list of id, name and sign-in
crab account <p> [folder]       print who provider <p> is signed in as, or nothing
crab history <id> [folder]      print a saved conversation: title, settings and turns
crab rename <id> <folder> <t>   name a saved conversation nobody has open
crab import <id> <folder>       save a conversation a client kept itself, read as JSON
crab memory [folder]            print the memories a conversation in the folder starts with, as JSON
crab hooks [folder]             print the hooks a conversation in the folder would find and its project's trust, as JSON
crab trust <folder> [on|off]    print whether the folder's project is trusted, or record that it is or is not
```

`crab --help` prints the same list; `crab` with no subcommand or an unknown
one prints it as an error.

- **`serve`** reads [commands](wire-protocol.md) from standard input and
  writes events to standard output, one JSON message per line, until `Close`
  or the end of standard input. It holds one conversation at a time; an `Open`
  for another one replaces it. [Getting started](getting-started.md) walks
  through a session.
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
  sent. It works while the conversation is open elsewhere.
- **`rename`** names a saved conversation, and fails while another process
  has it open. A client with the conversation open sends `Rename` instead.
- **`import`** reads a conversation a client kept itself as JSON on standard
  input and saves it under `id`, so that `crab` can open it from then on. It
  refuses a conversation that already has a history.
- **`memory`** prints what a conversation in the folder would start with. See
  [memory](memory.md).
- **`hooks`** prints every hook found, whether it runs, and what an
  untrusted project leaves out. See [hooks](extending/hooks.md).
- **`trust`** prints or records whether a project is trusted, for every
  worktree of its repository. See [project trust](extending/project-trust.md).

How saved conversations, queued prompts and the rest fit together is in
[concepts](concepts.md). The commands and events are defined in
`crates/omc-wire`. `crates/omc-client` is a small blocking client for programs
that start `crab` and talk to it.
