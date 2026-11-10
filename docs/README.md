# crab documentation

## Start here

- [Getting started](getting-started.md): install, sign in, a first conversation.
- [Concepts](concepts.md): conversations, turns, approvals, modes and the rest
  of the model the other pages assume.

## Using crab

- [Commands](usage.md): the `crab` subcommands.
- [Wire protocol](wire-protocol.md): every command and event a client sends and
  receives.
- [Configuration](configuration.md): every environment variable, file and
  setting.
- [Troubleshooting](troubleshooting.md): retries, recovery, and what crab does
  when a request or the model goes wrong.

## Tools

- [Tool index](tools/README.md): every tool, in one list.
- [How tool calls run](tools/calls.md): argument repair, long output, approvals.
- [Reading files](tools/files.md): outlines, conflicts, generated files,
  images, databases, archives and JSON.
- [Reading the web and GitHub](tools/web-and-github.md) and `web_search`.
- [Subagents](tools/subagents.md), including worktree isolation.
- [Scripts](tools/script.md).
- [Debugging](tools/debug.md).
- [Language services](language-services.md).

## Conversations over time

- [Long conversations](long-conversations.md): pruning, compaction, recall and
  the prompt cache.
- [Memory](memory.md).

## Extending

- [Skills, agents and instruction files](extending/skills-and-agents.md).
- [Tool servers (MCP)](extending/mcp.md).
- [Hooks](extending/hooks.md).
- [Extensions](extending/extensions.md): long-running programs that watch and
  steer a conversation and give the model tools, and their protocol.
- [Project trust](extending/project-trust.md).
- [Permission rules](extending/permissions.md): allow, ask and deny rules, and
  where "always allow" answers are kept.

## Providers

- [Overview](providers/overview.md).
- [Supported providers](providers/supported.md): addresses, keys and default
  models.
- [Custom endpoints](providers/custom-endpoints.md).
- [Adding a provider](providers/adding.md).

## Working on crab

- [Development](development.md).
- [Edit benchmark](benchmark.md): measuring how well models edit through crab.
