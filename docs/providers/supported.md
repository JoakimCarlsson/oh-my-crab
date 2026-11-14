# Supported providers

These providers are built into the `omc` binary. To add one, see
[adding.md](adding.md); to reach a server without changing crab, define it in
`providers.json` as [custom endpoints](custom-endpoints.md) says.

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
| `openai`     | OpenAiResponses   | `OPENAI_API_KEY`                      | `https://api.openai.com/v1`, or `OMC_OPENAI_URL`                               |
| `gemini`     | Gemini            | `GEMINI_API_KEY` / `GOOGLE_API_KEY`   | `https://generativelanguage.googleapis.com/v1beta`, or `OMC_GEMINI_URL`        |
| `together`   | ChatCompletions   | `TOGETHER_API_KEY`                    | `https://api.together.ai/v1`, or `OMC_TOGETHER_URL`                            |
| `fireworks`  | ChatCompletions   | `FIREWORKS_API_KEY`                   | `https://api.fireworks.ai/inference/v1`, or `OMC_FIREWORKS_URL`                |
| `lmstudio`   | ChatCompletions   | none                                  | `http://localhost:1234/v1`, or `OMC_LMSTUDIO_URL`                              |
| `llamacpp`   | ChatCompletions   | none                                  | `http://localhost:9931/v1`, or `OMC_LLAMACPP_URL`                              |
| `vllm`       | ChatCompletions   | none                                  | `http://localhost:8000/v1`, or `OMC_VLLM_URL`                                  |
| `anthropic`  | AnthropicMessages | `ANTHROPIC_API_KEY`                   | `https://api.anthropic.com`, or `OMC_ANTHROPIC_URL`                            |
| `bedrock`    | Bedrock           | AWS credentials, or `AWS_BEARER_TOKEN_BEDROCK` | `https://bedrock-runtime.<region>.amazonaws.com`, or `OMC_BEDROCK_URL` |
| `vertex`     | Vertex            | Google Cloud credentials              | `https://<region>-aiplatform.googleapis.com`, or `OMC_VERTEX_URL`               |
| `azure`      | OpenAiResponses   | `AZURE_OPENAI_API_KEY`                | `https://<resource>.openai.azure.com/openai/v1`; see [Azure OpenAI](#azure-openai) |

Each API-key provider opens with a default model when none is picked:
`kimi-for-coding` for `kimi-code`, `kimi-k2.7-code` for `moonshot`, `glm-5.3`
for both Z.ai ids, `MiniMax-M3`, `deepseek-v4-pro`, `grok-4.7`,
`openai/gpt-oss-120b` on Groq, `gpt-oss-120b` on Cerebras,
`mistral-large-latest`, `gpt-6.1-terra` on OpenAI, `gemini-3.8-flash`, `MiniMaxAI/MiniMax-M3` on
Together and `accounts/fireworks/models/kimi-k2-thinking` on Fireworks. Gemini
speaks its own API; see [gemini.md](gemini.md). The
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

### Anthropic API

`anthropic` is Claude with an API key from the Anthropic Console, from
`ANTHROPIC_API_KEY` or saved by a client. Unlike `claude`, which signs in to
a Claude subscription and has to present itself as Claude Code, it sends
nothing of Claude Code's: the key in `x-api-key`, `anthropic-version`,
`User-Agent: oh-my-crab/<version>`, and only the standard betas
(interleaved thinking with a thinking budget, the effort beta with an
effort). Prompt caching, both cache lifetimes, cache warming and thinking
work as for `claude`. It opens with `claude-sonnet-5-5`, and lists the models
the key may use.

### Amazon Bedrock

`bedrock` is Claude on Amazon Bedrock. Requests go to
`InvokeModelWithResponseStream`, which takes the Anthropic Messages body with
`anthropic_version: bedrock-2023-05-31` and the betas in `anthropic_beta`, so
cache breakpoints, thinking blocks and their signatures go through as they
do on Anthropic's own API; the Converse API would translate them into its own
blocks, losing redacted thinking. The answer is AWS's binary event stream,
whose chunks each carry one Messages event.

It is authorized with a Bedrock API key when `AWS_BEARER_TOKEN_BEDROCK` is set
or a client saved one, and otherwise signed with Signature Version 4 using
the AWS keys found the way the AWS tools find them: `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY` and `AWS_SESSION_TOKEN`, else the profile
`AWS_PROFILE` names (`default` otherwise) in `~/.aws/credentials` and then
`~/.aws/config`. A profile may give static keys or a `credential_process`,
whose keys are used for five minutes before it runs again. SSO and role
profiles are not run by crab itself; give them
`credential_process = aws configure export-credentials --profile <name> --format process`.
The region is `AWS_REGION`, `AWS_DEFAULT_REGION` or the profile's `region`,
else `us-east-1`. Signing in only checks that keys or a key can be found.

Model ids are Bedrock's, inference profiles included, such as
`global.anthropic.claude-sonnet-5-5` (the default) or
`us.anthropic.claude-sonnet-4-5-20250929-v1:0`. The list a client is offered
is the Claude models the model catalog has for Bedrock: the global profiles,
then those of the region's geography, then the models themselves; Bedrock is
not asked, since listing needs permissions inference does not. Caches are
written with the five-minute lifetime and not kept warm.

### Google Vertex AI

`vertex` is Claude on Google Vertex AI, through `streamRawPredict`, which
takes the Anthropic Messages body with `anthropic_version: vertex-2023-10-16`
and the model in the address, and streams the same events as Anthropic's own
API. The token comes from Google's Application Default Credentials: the file
`GOOGLE_APPLICATION_CREDENTIALS` names, else
`application_default_credentials.json` in gcloud's config folder, as
`gcloud auth application-default login` leaves it. A service account key signs
a JWT that is traded for a token; a user login trades its refresh token.
Without either file crab asks `gcloud auth print-access-token`. Tokens are
reused until a minute before they expire. The project is
`GOOGLE_CLOUD_PROJECT`, `GCLOUD_PROJECT` or `ANTHROPIC_VERTEX_PROJECT_ID`,
else the one the credentials file names; the region is
`GOOGLE_CLOUD_LOCATION` or `CLOUD_ML_REGION`, else `global`. Credentials on a
Google Cloud machine's metadata server are not read.

Model ids are Vertex's, such as `claude-sonnet-5-5` (the default) or
`claude-sonnet-4-5@20250929`; the list is the catalog's Claude models on
Vertex. Gemini on Vertex is not offered; use `gemini`. Caches are written
with the five-minute lifetime and not kept warm.

### OpenAI

`openai` speaks the Responses API at `https://api.openai.com/v1/responses`
with `OPENAI_API_KEY` as a bearer token; `OMC_OPENAI_URL` points it at another
host that serves the same API. Replies stream, tools are function tools, and
images go as `input_image` parts. A model that reasons (the GPT generations
from the fifth on, the `o` series, Codex and the open-weight GPT models) is
sent `reasoning` with the effort picked, clamped to the levels the model takes,
and `summary: "auto"`, so its reasoning summary streams as thinking. Every
request asks for `store: false` and for the encrypted reasoning, which goes
back with later requests in place of stored responses. The model list is the
conversation models `GET /v1/models` lists, after the default.

### Azure OpenAI

`azure` speaks the Responses API to an Azure OpenAI resource, sending
`AZURE_OPENAI_API_KEY` in the `api-key` header. Its address is, first set wins:

- `AZURE_OPENAI_BASE_URL`, the whole API address, such as
  `https://name.openai.azure.com/openai/v1`;
- `AZURE_OPENAI_ENDPOINT`, the resource's endpoint, such as
  `https://name.openai.azure.com`, with `/openai/v1` appended unless it
  already names a path under `/openai`;
- `AZURE_OPENAI_RESOURCE_NAME`, the resource's name, which makes
  `https://<name>.openai.azure.com/openai/v1`.

Requests go to `<address>/responses?api-version=<version>`, the version
being `AZURE_OPENAI_API_VERSION`, or `v1` when unset. The model is the
deployment's name: `AZURE_OPENAI_DEPLOYMENT` names the deployments offered,
separated by commas, the first being the default; without it the resource's
models are listed. A deployment is sent reasoning, as on `openai`, when its
name names a model that reasons, such as `gpt-5-prod`.

### Servers you declare

Servers crab has no definition for, such as LiteLLM, vLLM, LM Studio or
llama.cpp on another address, are added in `providers.json`, over Chat
Completions or the Responses API; see [custom endpoints](custom-endpoints.md).

The id is what you give when signing in or opening a conversation, for example
`claude` or `copilot`. The definitions live in
`crates/omc-providers/src/providers/`.

## Model catalog

How large a model's context window is, how much one reply may hold, which
efforts it takes, what it takes as input and what it costs come from a model
catalog. A snapshot of the [models.dev](https://models.dev) catalog is
compiled into crab; `crab serve` downloads a fresh copy into
`<cache>/models.json` in the background when the one there is missing or a
day old, and the next process uses it. Set `OMC_MODELS_REFRESH=0` to turn the
download off, or `OMC_MODELS_URL` to take the catalog from elsewhere. A
download that fails changes nothing; crab works offline with the snapshot.

A model is looked up under its provider's name in the catalog (`anthropic`
for `claude` and `anthropic`, `amazon-bedrock`, `google-vertex-anthropic`,
`github-copilot`, `openrouter` and so on), as written and then without a
Vertex `@version`, a Bedrock `-v1:0` suffix or a trailing date. A Claude
model a host does not list is looked up as Anthropic serves it. What the
catalog does not describe is worked out from the id as before, and a Chat
Completions model it does not know is given a 128,000-token window.

- The window is the catalog's input limit where the provider sets one below
  the context window, as Copilot does, else the context window. Claude models
  before the fifth generation are held to 200,000 tokens on every host, since
  their longer window needs the long-context beta crab does not send.
- The output limit is what Claude requests ask for as `max_tokens`.
- Efforts are offered as the catalog lists them on the Anthropic wire; a model
  the catalog says reasons only within a token budget gets the ladder of its
  generation. Chat Completions models are offered none, since that wire sends
  no effort.
- Prices are the catalog's base prices; prices for long prompts are not
  modelled. Claude's hour-long cache writes cost twice the input price.
- `codex` keeps its own windows and prices: the ChatGPT backend serves smaller
  windows than the OpenAI API the catalog describes.

## Wires

A wire is a protocol. Several providers can share one.

| Wire                | File                                                  | Serves                                                                                       |
| ------------------- | ----------------------------------------------------- | -------------------------------------------------------------------------------------------- |
| `AnthropicMessages` | `crates/omc-providers/src/wires/anthropic_messages.rs` | Anthropic, with a subscription (`claude`) or an API key (`anthropic`)                        |
| `Bedrock`           | `crates/omc-providers/src/wires/anthropic_messages.rs`, `crates/omc-providers/src/cloud/` | Claude on Amazon Bedrock: the Messages body, signed, over AWS's event stream |
| `Vertex`            | `crates/omc-providers/src/wires/anthropic_messages.rs`, `crates/omc-providers/src/cloud/` | Claude on Google Vertex AI: the Messages body over `streamRawPredict`         |
| `OpenAiResponses`   | `crates/omc-providers/src/wires/openai_responses.rs`   | the ChatGPT Codex backend, the OpenAI API (`openai`), Azure OpenAI (`azure`) and custom endpoints declared with `"wire": "responses"` |
| `ChatCompletions`   | `crates/omc-providers/src/wires/chat_completions.rs`   | anything that copies OpenAI Chat Completions: every provider above but the Claude ones and Codex, and custom endpoints |

## Sign-in methods

| Method        | Used by                  | How it works                                                              |
| ------------- | ------------------------ | ------------------------------------------------------------------------- |
| OAuth         | `claude`, `codex`        | Reports the login page as `LoginUrl`, waits for the callback, refreshes the token when it expires. |
| Device code   | `copilot`                | Reports the page and a short code as `LoginCode`, then polls until approved. |
| API key       | `anthropic`, `openrouter`, `kimi-code` and the other hosted APIs | Reads the key from an environment variable, or from one a client saved with `omc_session::save_key`; the variable wins. |
| Cloud         | `bedrock`, `vertex`      | Reads the cloud's credentials where its own tools keep them, on every request; signing in only checks they can be found. `bedrock` also takes a Bedrock API key, and is listed to clients as an API-key provider; `vertex` takes none and is listed as needing no sign-in. |
| None          | `ollama`, `lmstudio`, `llamacpp`, `vllm` | A local server that needs no credentials.                                 |

crab never opens a browser itself. The client is told what to open with
`LoginUrl` or `LoginCode` and opens it where the reader is, which also works
when crab runs on another machine.
