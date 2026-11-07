# Custom endpoints

Any other server that speaks the Chat Completions API, such as a company
gateway or a local server on another port, can be added without a change to
crab. Write it to `providers.json` in crab's
[config folder](../configuration.md#folders) (`~/.config/oh-my-crab/` on
Linux), or in the file `OMC_PROVIDERS_FILE` names:

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
    }
  ]
}
```

Only `id` and `base_url` are needed. The id is lowercase letters, digits, `-`
and `_`, and cannot be a built-in one. The label is the id when left out.
`wire` takes only `chat_completions`, its default. `api_key_env` is one
variable or a list tried in order; without it no key is sent. `default_model`
is the model a conversation opens with when none is picked; without it crab
takes the first model the server lists. `replay_reasoning` sends the model's
earlier reasoning back as `reasoning_content`, which servers of thinking
models such as Kimi and DeepSeek want around tool calls.

The file is read once, when the providers are first listed. A custom provider
is listed, signed in to and talked to like a built-in one. A file that does not
parse, or that defines a provider wrongly, is ignored as a whole, and crab says
why on standard error.

A client lists them with `crab providers`, or `omc_session::providers()` when it
links the harness, so a provider added to crab shows up in every client without
a change there.
