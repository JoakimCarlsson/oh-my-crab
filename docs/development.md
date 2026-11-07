# Development

## Building and checking

```sh
make fmt                              # format every crate
make lint                             # check formatting, then clippy with warnings as errors
CARGO_TARGET_DIR=build cargo test --workspace
make install                          # install crab under ~/.local/bin, or under PREFIX
```

`build/` is git-ignored; putting build output there keeps it beside the
checkout. CI runs the same steps on every push to `main` and every pull
request: `cargo fmt --check` on Linux, and clippy, `cargo test --workspace
--locked` and `cargo build --workspace --locked` on Linux, macOS and Windows.
Every text file is checked out with LF line endings on every system, as
`.gitattributes` says, since prompts and test fixtures are compiled in.

## Crates

| Crate           | What it holds                                                                     |
| --------------- | --------------------------------------------------------------------------------- |
| `omc`           | The `crab` binary and its subcommands.                                            |
| `omc-wire`      | The commands and events between crab and a client; see [wire protocol](wire-protocol.md). |
| `omc-client`    | A small blocking client for programs that start crab and talk to it.              |
| `omc-session`   | One conversation: turns, settings, saving, the toolset, translating agent events to wire events. |
| `omc-agent`     | The agent loop: model requests, tool calls, approvals, hooks, compaction, subagents and their worktrees. |
| `omc-providers` | The provider definitions, their three wire formats, model lists and custom endpoints. |
| `omc-auth`      | OAuth, device-code and API-key sign-in, credential storage and token refresh.     |
| `omc-tools`     | The tool trait, the registry and the built-in tools.                              |
| `omc-context`   | The system prompt and what goes into it: instruction files, skills, agents, notices. |
| `omc-memory`    | Memory across conversations.                                                      |
| `omc-hooks`     | Hooks and project trust.                                                          |
| `omc-mcp`       | Tool servers as tools of the agent.                                               |
| `omc-lsp`       | Language servers crab starts itself.                                              |
| `omc-dap`       | The `debug` tool and its debug adapters.                                          |
| `omc-script`    | The `script` tool and its QuickJS sandbox.                                        |

pandemonium depends on `omc-session`, `omc-wire` and `omc-context` by relative
path, so a change to their public API must build there too.

## Tests

Unit tests sit beside the code. Integration tests that start real processes
live under each crate's `tests/`:

- `omc/tests/serve.rs` starts `crab serve` and checks it exits on `Close`.
- `omc-agent/tests/worktrees.rs` runs isolated subagents against real git
  repositories.
- `omc-lsp/tests/real_server.rs` and `omc-dap/tests/real_adapter.rs` drive a
  real language server and real debuggers when they are installed, and skip
  themselves otherwise.

Tests that need a scratch folder make it under the workspace's `build/tests/`,
never in the system's temporary folder.

Some tests keep the documentation honest: they fail when a wire type or an
environment variable the code reads is missing from
[wire protocol](wire-protocol.md) or [configuration](configuration.md). When
one fails, document what you added.

## Adding things

- A provider: [providers/adding.md](providers/adding.md).
- A tool: implement `omc_tools::Tool` (name, description, JSON schema, `call`,
  and `read_only` or `approval_scope` as it needs), register it where the
  session builds its registry in `omc-session`, and add it to
  [tools](tools/README.md).

## Releasing

A release is a git tag named after `version` in the root `Cargo.toml`, with
no `v` in front: `0.1.0`, or `0.2.0-rc.1` for a release candidate.

1. Set the version in `[workspace.package]` and in the `omc-*` entries of
   `[workspace.dependencies]` and of each crate's `Cargo.toml`, then commit.
2. Tag the commit and push the tag: `git tag -a 0.1.0 -m 0.1.0 && git push origin 0.1.0`.

The release workflow checks that the tag matches the version, runs fmt,
clippy and the tests, builds `crab` for Linux (x86-64 and ARM), macOS (Intel
and Apple silicon) and Windows, and publishes a GitHub release with the
archives and generated notes. A tag with a `-` in it, such as `-rc.1`, is
published as a pre-release.

The workflow publishes the release with GitHub's generated notes. Then replace the body by hand with `gh release edit <version> --notes-file <file>`: `## What's new` and `## Fixes`, written from the issues closed and pull requests merged since the previous release with their numbers, then `## Install` and the **Full changelog** link. No intro paragraph, and no hard line wrapping.

pandemonium depends on crab's crates by git tag, so a pandemonium release
names the crab tag it was built with. Every crate is ready for crates.io too
(`cargo package --workspace` succeeds); publishing there is not part of the
release yet.
