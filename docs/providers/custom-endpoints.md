# Custom endpoints

Any other server that speaks the Chat Completions API or the Responses API,
such as a company gateway, LiteLLM, vLLM, LM Studio or llama.cpp on an address
of its own, can be added without a change to crab. Write it to
`providers.json` in crab's [config folder](../configuration.md#folders)
(`~/.config/oh-my-crab/` on Linux), or in the file `OMC_PROVIDERS_FILE` names:

```json
{
  "providers": [
    {
      "id": "work",
      "label": "Work gateway",
      "base_url": "https://llm.example.com/v1",
      "wire": "chat_completions",
      "api_key_env": "WORK_LLM_KEY",
      "headers": { "X-Team": "infra" },
      "default_model": "qwen-coder",
      "replay_reasoning": false
    },
    {
      "id": "litellm",
      "label": "LiteLLM",
      "base_url": "http://localhost:4000/v1",
      "wire": "responses",
      "api_key": "sk-local",
      "models": ["gpt-5", "claude-sonnet-5-5"]
    }
  ]
}
```

| Field              | Needed | Meaning                                                                                                   |
| ------------------ | ------ | --------------------------------------------------------------------------------------------------------- |
| `id`               | yes    | What the server is chosen by: lowercase letters, digits, `-` and `_`, and not a built-in provider's id.  |
| `base_url`         | yes    | The API address `/chat/completions`, `/responses` and `/models` are appended to; a trailing `/` is dropped. |
| `label`            | no     | The name clients show; the id when left out.                                                              |
| `wire`             | no     | `chat_completions` (the default) or `responses`.                                                          |
| `api_key_env`      | no     | One variable, or a list tried in order, the API key is read from.                                         |
| `api_key`          | no     | The API key itself, for a server whose key is not in the environment. Not with `api_key_env`.             |
| `headers`          | no     | Headers sent on every request.                                                                            |
| `default_model`    | no     | The model a conversation opens with when none is picked; else the first model offered.                   |
| `models`           | no     | The models offered, in order; when left out they are asked of the server at `GET {base_url}/models`.     |
| `replay_reasoning` | no     | Sends the model's earlier reasoning back as `reasoning_content` on Chat Completions, which servers of thinking models such as Kimi and DeepSeek want around tool calls. |

Without `api_key_env` or `api_key` no key is sent. A key goes as
`Authorization: Bearer <key>`. On the `responses` wire a server is sent what
the [OpenAI provider](supported.md#openai) is sent, except that it is asked
for encrypted reasoning only when the model reasons, and is not sent a prompt
cache key.

## Finding models

When a server is given no `models`, crab asks it for them at
`GET {base_url}/models` whenever a client lists the models of the provider,
and leaves out the ids of embedding, speech and image models. A client can
ask a server that is not saved yet the same way, to fill a model picker,
with `FindModels` over [`crab serve`](../wire-protocol.md#findmodels), or
`omc_session::find_models` from Rust.

## Where they show up

The file is read when the providers are first listed, and again whenever a
client saves or removes a server. A server is listed, signed in to and talked
to like a built-in provider: `crab providers` and `omc_session::providers()`
list it after the built-in ones, marked `"server": true` and with the
`models` it was given, and the model setting of a conversation opened with
it offers its models. A file that does not parse, or that defines a provider
wrongly, is ignored as a whole, and crab says why on standard error.

## Editing them from a client

A client offers a settings page for servers with four commands of
[`crab serve`](../wire-protocol.md#servers), which need no open conversation:
`Servers` lists them, `SaveServer` adds one or replaces the one with the same
id, `RemoveServer` takes one out, and `FindModels` asks one for its models.
Crab never sends a saved key back: it says `has_api_key` instead, keeps the
saved key when `SaveServer` sends none, and removes it when `SaveServer` sends
an empty one. From Rust the same is `omc_session::servers`, `save_server`,
`remove_server` and `find_models`.
