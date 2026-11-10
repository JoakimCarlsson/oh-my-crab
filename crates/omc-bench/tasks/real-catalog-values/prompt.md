In `models.json`, each model is one line under its provider. Set the output limit of these eight models (the `output` that follows `context`, not the one inside `cost`) to the number given, only under the provider named; other providers list models with the same id, and those lines stay as they are. Change nothing else.

- `claude-fable-5` under `anthropic`: `129024`
- `claude-sonnet-5` under `github-copilot`: `129024`
- `gemini-4-argon` under `github-copilot`: `66560`
- `glm-4.7` under `zai-coding-plan`: `132096`
- `glm-5-turbo` under `zai`: `132096`
- `glm-5.2` under `zai-coding-plan`: `132096`
- `glm-5.3` under `zai`: `132096`
- `glm-5.3-flash` under `zai-coding-plan`: `132096`
