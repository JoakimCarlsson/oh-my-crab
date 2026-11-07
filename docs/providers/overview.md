# Providers

Signed in with OAuth or a device code: `claude`, `codex` and `copilot`.

Signed in with a Kimi account's one-time code or with an API key: `kimi-code`
(Kimi Code, the Kimi For Coding subscription). The account login presents
itself as Kimi's own command-line client, which Kimi's terms forbid third-party
tools to do; see [supported providers](supported.md).

Claude in the clouds, with the credentials the cloud's own tools find:
`bedrock` (Amazon Bedrock, also with a Bedrock API key) and `vertex` (Google
Vertex AI).

With an API key from the environment, or one a client saved: `anthropic` (the
Anthropic API, without the Claude Code identity `claude` presents),
`moonshot`, `zai-coding` (the
Z.ai GLM Coding Plan), `zai`, `minimax`, `deepseek`, `xai`, `groq`,
`cerebras`, `mistral`, `openai`, `gemini`, `together`, `fireworks` and
`openrouter`.

Local servers that need no key: `ollama`, `lmstudio`, `llamacpp` and `vllm`.

See [supported providers](supported.md) for each one's key variable,
address and default model, for the model catalog that gives each model's
window, limits, efforts and prices, and for how to add one.
