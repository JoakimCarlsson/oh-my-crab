# oh-my-crab

**A coding-agent harness with no interface of its own.**

[![CI](https://img.shields.io/github/actions/workflow/status/JoakimCarlsson/oh-my-crab/ci.yml?style=flat&colorA=222222&colorB=3FB950)](https://github.com/JoakimCarlsson/oh-my-crab/actions)
[![License](https://img.shields.io/github/license/JoakimCarlsson/oh-my-crab?style=flat&colorA=222222&colorB=58A6FF)](LICENSE)
[![Rust](https://img.shields.io/badge/Rust-DEA584?style=flat&colorA=222222&logo=rust&logoColor=white)](https://www.rust-lang.org)

Inspired by [Pi](https://github.com/earendil-works/pi) and
[oh-my-pi](https://github.com/can1357/oh-my-pi).

`crab` runs the agent loop, the tools and the model providers for one
conversation, and talks to whatever started it with JSON lines on standard
input and output. Bring your own front end: an editor, a terminal UI, a bot,
a script.

Subscriptions, cloud and local providers · LSP and DAP built in · Typed subagents

It is the harness behind
[pandemonium](https://github.com/JoakimCarlsson/pandemonium), which starts one
`crab` per agent session and draws the conversation itself.

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

## What's in the box

### 01 · A wire protocol, not a UI

Everything crab does arrives as typed events: text, tool calls, approvals,
questions, plans and todos. A client draws them however it likes. The format
lives in `omc-wire` and is described in
[the wire protocol](docs/wire-protocol.md).

### 02 · Edits that know the language

`edit` works by line number against the tag of the last read, so a stale edit
is refused instead of landing in the wrong place. After every write the file
is formatted and its language-server diagnostics come back with the result.
`lsp` gives definitions, references, hovers and cross-file renames, and
`ast_grep` and `ast_edit` search and rewrite code by syntax tree. See
[language services](docs/language-services.md).

### 03 · A real debugger

`debug` drives lldb-dap, gdb, Delve or debugpy over the Debug Adapter
Protocol: set breakpoints, step, read frames and evaluate expressions instead
of adding print statements. See [debugging](docs/tools/debug.md).

### 04 · Subagents with typed results

`task` runs many subagents at once, each optionally in its own git worktree or
in the background. Give a call an `output_schema` and every subagent hands back
a checked object rather than prose. See [subagents](docs/tools/subagents.md).

### 05 · Scripts that call tools

`script` runs a short JavaScript program in a sandbox that calls the other
tools. Only what it prints and returns goes back to the model, so a sweep over
many files costs one result. See [scripts](docs/tools/script.md).

### 06 · Conversations that don't fall over

Pruning, compaction and `recall` keep long conversations going while the
prompt cache stays stable across turns. Checkpoints, rewinds and a notebook
that survives a fresh context window let the agent manage its own context.
See [long conversations](docs/long-conversations.md).

### 07 · Memory per repository

`remember`, `memory_search` and `memory_edit` keep facts and lessons across
conversations, shared by every worktree of a repository. See
[memory](docs/memory.md).

### 08 · Web and GitHub as paths

`read` takes a URL or a `pr://`, `issue://` or `gh://` path the same way it
takes a file, and `web_search` returns ranked results. See
[web and GitHub](docs/tools/web-and-github.md).

### 09 · Hooks, permissions and project trust

Hooks in Claude Code's format, permission rules for any tool, plan mode, and
project trust, so a cloned repository cannot steer the agent until you trust
it. See [hooks](docs/extending/hooks.md),
[permissions](docs/extending/permissions.md) and
[project trust](docs/extending/project-trust.md).

### 10 · MCP, skills and agents

Tool servers over MCP, skills, agent definitions and instruction files are
picked up from disk and from the client. See [MCP](docs/extending/mcp.md) and
[skills and agents](docs/extending/skills-and-agents.md).

## Providers

- **Subscriptions:** `claude`, `codex`, `copilot`, `kimi-code`
- **Claude in the cloud:** `bedrock`, `vertex`
- **API keys:** `anthropic`, `openai`, `gemini`, `openrouter`, `xai`,
  `deepseek`, `mistral`, `groq`, `cerebras`, `together`, `fireworks`,
  `moonshot`, `zai`, `zai-coding`, `minimax`
- **Local servers:** `ollama`, `lmstudio`, `llamacpp`, `vllm`
- **Any Chat Completions endpoint** you add

Model windows, limits and prices come from a bundled models.dev catalog. See
[supported providers](docs/providers/supported.md) and
[custom endpoints](docs/providers/custom-endpoints.md).

## Why it's built this way

- **Headless on purpose.** The harness and the interface change at different
  speeds, so they are separate programs.
- **One conversation per process.** Easy to start, stop and isolate.
- **Nothing runs on trust by default.** A project's hooks, tool servers and
  instructions wait until you trust the folder.

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

## Crates

| Crate           | What it holds                                                                |
| --------------- | ---------------------------------------------------------------------------- |
| `omc`           | The `crab` binary                                                            |
| `omc-wire`      | The protocol between crab and the program that shows it                      |
| `omc-session`   | One conversation, driven by commands and reported as events                  |
| `omc-agent`     | The agent loop: model requests, tool calls, approvals, compaction, subagents |
| `omc-tools`     | The tool trait, registry and built-in tools                                  |
| `omc-providers` | Model providers, the model catalog and custom endpoints                      |
| `omc-auth`      | OAuth, device-code and API-key sign-in                                       |
| `omc-context`   | The system prompt, instruction files, skills and agents                      |
| `omc-lsp`       | Language servers crab starts itself                                          |
| `omc-dap`       | The debug tool, over the Debug Adapter Protocol                              |
| `omc-mcp`       | MCP tool servers as crab tools                                               |
| `omc-hooks`     | Hooks and project trust                                                      |
| `omc-memory`    | Memory that lasts across conversations                                       |
| `omc-script`    | The script tool: JavaScript in an embedded QuickJS sandbox                   |
| `omc-client`    | A small blocking client for programs that start crab                         |

## License

MIT, see [LICENSE](LICENSE).
