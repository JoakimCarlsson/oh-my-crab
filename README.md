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
```

A conversation opened with an id is saved by `crab`: its messages, its turns,
its title, the settings chosen for it and the list of what can be set, the commands it offers, the context window, its notebook, the files each tool call changed, and the turns that were stopped or failed, which the model never sees but a client still shows. It comes back as it was
when the same id is opened again, so a client does not need to keep a copy. The
agent names an unnamed conversation itself with `set_title`; a name the client
sends with `Rename` always wins.

The commands and events are defined in `crates/omc-wire`. `crates/omc-client`
is a small blocking client for programs that start `crab` and talk to it.

## Long conversations

When a conversation's context nears the model's window, `crab` first leaves
old tool outputs out of what the model is sent: every output of at least 50
tokens outside the most recent 40,000 tokens of tool output, as long as that
saves at least 20,000 tokens. Each is replaced with a short note. A read made
again with the same arguments makes the earlier copy out of date, and that copy
is left out as soon as doing so costs little of the provider's prompt cache.
When leaving outputs out is not enough, `crab` compacts the conversation
before the next request. It asks the model for a structured summary of the
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
compaction as a finished `compact` step holding the summary. A `/compact` the
reader sent comes back as a turn of its own, and one that failed comes back
with its reason.

Nothing left out or summarized is lost to the model either. Once a
conversation has been pruned or compacted, the model is offered a `recall` tool
that reads the whole history again: the full output of one tool call, the
lines that contain some text, or a range of messages by number.

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

## Providers

`claude`, `codex`, `copilot`, `openrouter` and `ollama`. See
[docs/providers](docs/providers/supported.md) for sign-in and how to add one.

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
