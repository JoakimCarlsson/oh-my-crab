# Supported providers

Every provider is built into the `omc` binary. To add one, see
[adding.md](adding.md).

## Providers

| Id           | Wire              | Sign-in              | Address                                                  |
| ------------ | ----------------- | -------------------- | -------------------------------------------------------- |
| `claude`     | AnthropicMessages | OAuth in the browser | `https://api.anthropic.com`                              |
| `codex`      | OpenAiResponses   | OAuth in the browser | `https://chatgpt.com/backend-api/codex`                  |
| `ollama`     | ChatCompletions   | none                 | `http://localhost:11434/v1`, or `OMC_OLLAMA_URL`         |
| `openrouter` | ChatCompletions   | `OPENROUTER_API_KEY` | `https://openrouter.ai/api/v1`, or `OMC_OPENROUTER_URL`  |
| `copilot`    | ChatCompletions   | device code          | `https://api.githubcopilot.com`, or the account's own    |

The id is what you give when signing in or opening a conversation, for example
`claude` or `copilot`. The definitions live in
`crates/omc-providers/src/providers/`.

## Wires

A wire is a protocol. Several providers can share one.

| Wire                | File                                                  | Serves                                                                                       |
| ------------------- | ----------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `AnthropicMessages` | `crates/omc-providers/src/wires/anthropic_messages.rs` | Anthropic                                                                                    |
| `OpenAiResponses`   | `crates/omc-providers/src/wires/openai_responses.rs`   | the ChatGPT Codex backend                                                                    |
| `ChatCompletions`   | `crates/omc-providers/src/wires/chat_completions.rs`   | anything that copies OpenAI Chat Completions: Ollama, OpenRouter, Copilot, vLLM, LM Studio, Groq, Together |

## Sign-in methods

| Method        | Used by                  | How it works                                                              |
| ------------- | ------------------------ | ------------------------------------------------------------------------- |
| OAuth         | `claude`, `codex`        | Opens a browser, waits for the callback, refreshes the token when it expires. |
| Device code   | `copilot`                | Shows a short code to enter on a web page, then polls until approved.     |
| API key       | `openrouter`             | Reads the key from an environment variable; signing in fails when it is not set. |
| None          | `ollama`                 | A local server that needs no credentials.                                 |
