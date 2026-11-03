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
crab serve    hold a conversation over standard input and output
crab tools    print the tools the agent can have, as a JSON list
```

The commands and events are defined in `crates/omc-wire`. `crates/omc-client`
is a small blocking client for programs that start `crab` and talk to it.

## Providers

`claude`, `codex`, `copilot`, `openrouter` and `ollama`. See
[docs/providers](docs/providers/supported.md) for sign-in and how to add one.

## Development

```sh
make fmt
make lint
cargo test --workspace
```

## License

MIT, see [LICENSE](LICENSE).
