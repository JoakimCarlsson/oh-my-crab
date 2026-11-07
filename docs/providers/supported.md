# Supported providers

These providers are built into the `omc` binary. To add one, see
[adding.md](adding.md); to reach a server without changing crab, define it in
`providers.json` as the README's "Custom endpoints" says.

## Providers

| Id           | Wire              | Sign-in                               | Address                                                                        |
| ------------ | ----------------- | ------------------------------------- | ------------------------------------------------------------------------------ |
| `claude`     | AnthropicMessages | OAuth in the browser                  | `https://api.anthropic.com`                                                    |
| `codex`      | OpenAiResponses   | OAuth in the browser                  | `https://chatgpt.com/backend-api/codex`                                        |
| `ollama`     | ChatCompletions   | none                                  | `http://localhost:11434/v1`, or `OMC_OLLAMA_URL`                               |
| `openrouter` | ChatCompletions   | `OPENROUTER_API_KEY`                  | `https://openrouter.ai/api/v1`, or `OMC_OPENROUTER_URL`                        |
| `copilot`    | ChatCompletions   | device code                           | `https://api.githubcopilot.com`, or the account's own                          |
| `kimi-code`  | ChatCompletions   | Kimi account (device code), or `KIMI_CODE_API_KEY` / `KIMI_API_KEY` | `https://api.kimi.ai/coding/v1`, or `OMC_KIMI_CODE_URL`                        |
| `moonshot`   | ChatCompletions   | `MOONSHOT_API_KEY`                    | `https://api.moonshot.ai/v1`, or `OMC_MOONSHOT_URL`                            |
| `zai-coding` | ChatCompletions   | `ZAI_API_KEY`                         | `https://api.z.ai/api/coding/paas/v4`, or `OMC_ZAI_CODING_URL`                 |
| `zai`        | ChatCompletions   | `ZAI_API_KEY`                         | `https://api.z.ai/api/paas/v4`, or `OMC_ZAI_URL`                               |
| `minimax`    | ChatCompletions   | `MINIMAX_API_KEY`                     | `https://api.minimax.io/v1`, or `OMC_MINIMAX_URL`                              |
| `deepseek`   | ChatCompletions   | `DEEPSEEK_API_KEY`                    | `https://api.deepseek.com`, or `OMC_DEEPSEEK_URL`                              |
| `xai`        | ChatCompletions   | `XAI_API_KEY`                         | `https://api.x.ai/v1`, or `OMC_XAI_URL`                                        |
| `groq`       | ChatCompletions   | `GROQ_API_KEY`                        | `https://api.groq.com/openai/v1`, or `OMC_GROQ_URL`                            |
| `cerebras`   | ChatCompletions   | `CEREBRAS_API_KEY`                    | `https://api.cerebras.ai/v1`, or `OMC_CEREBRAS_URL`                            |
| `mistral`    | ChatCompletions   | `MISTRAL_API_KEY`                     | `https://api.mistral.ai/v1`, or `OMC_MISTRAL_URL`                              |
| `openai`     | ChatCompletions   | `OPENAI_API_KEY`                      | `https://api.openai.com/v1`, or `OMC_OPENAI_URL`                               |
| `gemini`     | ChatCompletions   | `GEMINI_API_KEY`                      | `https://generativelanguage.googleapis.com/v1beta/openai`, or `OMC_GEMINI_URL` |
| `together`   | ChatCompletions   | `TOGETHER_API_KEY`                    | `https://api.together.ai/v1`, or `OMC_TOGETHER_URL`                            |
| `fireworks`  | ChatCompletions   | `FIREWORKS_API_KEY`                   | `https://api.fireworks.ai/inference/v1`, or `OMC_FIREWORKS_URL`                |
| `lmstudio`   | ChatCompletions   | none                                  | `http://localhost:1234/v1`, or `OMC_LMSTUDIO_URL`                              |
| `llamacpp`   | ChatCompletions   | none                                  | `http://localhost:9931/v1`, or `OMC_LLAMACPP_URL`                              |
| `vllm`       | ChatCompletions   | none                                  | `http://localhost:8000/v1`, or `OMC_VLLM_URL`                                  |

Each API-key provider opens with a default model when none is picked:
`kimi-for-coding` for `kimi-code`, `kimi-k2.7-code` for `moonshot`, `glm-5.3`
for both Z.ai ids, `MiniMax-M3`, `deepseek-v4-pro`, `grok-4.7`,
`openai/gpt-oss-120b` on Groq, `gpt-oss-120b` on Cerebras,
`mistral-large-latest`, `gpt-6.1-terra` on OpenAI (its largest models take no
tools over Chat Completions), `gemini-3.8-flash`, `MiniMaxAI/MiniMax-M3` on
Together and `accounts/fireworks/models/kimi-k2-thinking` on Fireworks. The
default leads the model list a client is offered; the rest is what the server's
`/models` lists, without embedding, speech and image models. Local servers take
the first model they list.

Kimi Code is the Kimi For Coding subscription. It is signed in to either way:

- With a key from the Kimi Code console, from the environment or saved by a
  client. Requests go to `https://api.kimi.ai/coding/v1` with
  `User-Agent: oh-my-crab/<version>`, as Kimi asks third-party tools to.
  Subscriptions from the China site set
  `OMC_KIMI_CODE_URL=https://api.kimi.com/coding/v1`.
- With a Kimi account, by signing in with a one-time code at `auth.kimi.com`.
  The login refreshes its token by itself and goes to
  `https://api.kimi.com/coding/v1`. Kimi lets only its own clients sign in
  with an account, so this login names itself as Kimi's command-line client:
  its client id, `User-Agent: KimiCLI/<version>` and its `X-Msh-*` device
  headers, kept with the login. Kimi's terms call changing a client's
  identity a violation that can suspend the account's membership; use the
  key when that risk is not wanted.

A key in the environment wins over a saved login or key. Keys from Moonshot's
China platform set `OMC_MOONSHOT_URL=https://api.moonshot.cn/v1`.

Kimi, Moonshot, Z.ai, MiniMax, DeepSeek and Fireworks are sent the model's
earlier reasoning back as `reasoning_content`, which their thinking models want
around tool calls.

Servers crab has no definition for are added in `providers.json`; see
"Custom endpoints" in the [README](../../README.md#custom-endpoints).

The id is what you give when signing in or opening a conversation, for example
`claude` or `copilot`. The definitions live in
`crates/omc-providers/src/providers/`.

## Wires

A wire is a protocol. Several providers can share one.

| Wire                | File                                                  | Serves                                                                                       |
| ------------------- | ----------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `AnthropicMessages` | `crates/omc-providers/src/wires/anthropic_messages.rs` | Anthropic                                                                                    |
| `OpenAiResponses`   | `crates/omc-providers/src/wires/openai_responses.rs`   | the ChatGPT Codex backend                                                                    |
| `ChatCompletions`   | `crates/omc-providers/src/wires/chat_completions.rs`   | anything that copies OpenAI Chat Completions: every provider above but Claude and Codex, and custom endpoints |

## Sign-in methods

| Method        | Used by                  | How it works                                                              |
| ------------- | ------------------------ | ------------------------------------------------------------------------- |
| OAuth         | `claude`, `codex`        | Reports the login page as `LoginUrl`, waits for the callback, refreshes the token when it expires. |
| Device code   | `copilot`                | Reports the page and a short code as `LoginCode`, then polls until approved. |
| API key       | `openrouter`, `kimi-code` and the other hosted APIs | Reads the key from an environment variable, or from one a client saved with `omc_session::save_key`; the variable wins. |
| None          | `ollama`, `lmstudio`, `llamacpp`, `vllm` | A local server that needs no credentials.                                 |

crab never opens a browser itself. The client is told what to open with
`LoginUrl` or `LoginCode` and opens it where the reader is, which also works
when crab runs on another machine.
