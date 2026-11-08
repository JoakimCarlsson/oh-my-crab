# oh-my-crab

`crab` is a coding-agent harness with no interface of its own. It runs the
agent loop, the tools and the model providers for one conversation, and talks
to the program that started it with JSON lines on standard input and output.

Inspired by [Pi](https://github.com/earendil-works/pi) and
[oh-my-pi](https://github.com/can1357/oh-my-pi).

It is primarily used by
[pandemonium](https://github.com/JoakimCarlsson/pandemonium), which starts it
for each agent session and draws the conversation itself. Any other front end,
such as a terminal UI, can use the same protocol.

## What it does

- **Tools:** reading, editing and searching code with outlines and
  syntax-aware search, `bash`, web pages, web search, GitHub pull requests and
  issues, a debugger over the Debug Adapter Protocol, and JavaScript scripts
  that call the other tools in a sandbox.
- **Subagents:** parallel subagents, each optionally in a git worktree of its
  own, handing back typed results.
- **Long conversations:** pruning, compaction and recall, with a prompt cache
  that stays stable across turns.
- **Memory** that lasts across conversations and is shared by every worktree
  of a repository.
- **Hooks** in Claude Code's format, and **project trust**, so a cloned
  repository cannot steer the agent until you trust it.
- **Providers:** Claude, Codex, Copilot, Kimi Code, OpenRouter, a dozen hosted
  APIs, local servers, and any Chat Completions endpoint you add.

## Install

```sh
make install
```

This installs `crab` under `~/.local/bin`. Set `PREFIX` to install elsewhere.

## Quick start

```sh
crab providers                 # which providers exist and how each signs in
crab serve                     # hold a conversation over standard input and output
```

A client sends `Open`, then `Prompt`, and reads events until `Finished`:

```json
{"Open":{"directory":"/path/to/project","provider":"claude","model":null,"yolo":false}}
{"Prompt":{"text":"What does this project do?","files":[]}}
```

[Getting started](docs/getting-started.md) walks through signing in and a
first conversation.

## Documentation

The full documentation is in [docs/](docs/README.md):

- [Getting started](docs/getting-started.md) and [concepts](docs/concepts.md)
- [Commands](docs/usage.md) and the [wire protocol](docs/wire-protocol.md)
- [Tools](docs/tools/README.md)
- [Configuration](docs/configuration.md): every file and environment variable
- [Providers](docs/providers/overview.md)
- [Hooks](docs/extending/hooks.md) and [project trust](docs/extending/project-trust.md)
- [Troubleshooting](docs/troubleshooting.md)
- [Development](docs/development.md)

## License

MIT, see [LICENSE](LICENSE).
