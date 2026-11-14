# Configuration

Everything a user can configure in crab, in one place: environment variables,
files and the settings of a conversation. Each row links to the page that
says more. Crab has no settings file of its own for its general behaviour;
what is not set here is chosen per conversation by the client. Its
instructions, skills, agent definitions and system prompt files have folders
of their own, `<config>` for the user and `.oh-my-crab` in a project, read
before Claude Code's and Codex's.

## Folders

Crab keeps its files in three folders of its own, named `oh-my-crab` inside
the user's config, data and cache folders. The tables below write them as
`<config>`, `<data>` and `<cache>`.

| Folder     | Linux                                                 | macOS                                                | Windows                                    |
| ---------- | ----------------------------------------------------- | ---------------------------------------------------- | ------------------------------------------ |
| `<config>` | `$XDG_CONFIG_HOME/oh-my-crab`, else `~/.config/oh-my-crab` | `~/Library/Application Support/oh-my-crab`      | `%APPDATA%\oh-my-crab` (`AppData\Roaming`) |
| `<data>`   | `$XDG_DATA_HOME/oh-my-crab`, else `~/.local/share/oh-my-crab` | `~/Library/Application Support/oh-my-crab`   | `%APPDATA%\oh-my-crab` (`AppData\Roaming`) |
| `<cache>`  | `$XDG_CACHE_HOME/oh-my-crab`, else `~/.cache/oh-my-crab` | `~/Library/Caches/oh-my-crab`                     | `%LOCALAPPDATA%\oh-my-crab` (`AppData\Local`) |

On Linux an `XDG_*` variable counts only when it holds an absolute path. On
macOS and Windows `<config>` and `<data>` are the same folder. When a folder
cannot be found, what lives in it is not kept: no logins, saved
conversations, memory, recorded trust, saved output or worktrees.

## Environment variables

Crab reads these from its own environment, so they are set where the client
starts `crab`. A switch that is off when set to `0` is also off with `false`,
`off` or `no`, in any case.

### Crab's own

| Variable             | What it does                                                                                                | Default                       | Details                                                    |
| -------------------- | ----------------------------------------------------------------------------------------------------------- | ----------------------------- | ---------------------------------------------------------- |
| `OMC_CONFIG_DIR`     | The folder the user's own instruction files, skills, agent definitions and system prompt files for crab are read from. Crab's other files in `<config>` keep their own variables. | `<config>`                    | [Skills and agents](extending/skills-and-agents.md)        |
| `OMC_PROVIDERS_FILE` | The file custom providers are read from.                                                                    | `<config>/providers.json`     | [Custom endpoints](providers/custom-endpoints.md)          |
| `OMC_SLOTS_FILE`     | The file the user's model slots and presets are read from and written to.                                  | `<config>/slots.json`         | [Models and slots](models-and-slots.md)                    |
| `OMC_MODEL_<SLOT>`   | The model of the slot `<slot>`, as `OMC_MODEL_FAST=claude/haiku:low`; wins over the slot files. The backups still come from the files. | unset | [Models and slots](models-and-slots.md) |
| `OMC_<ID>_URL`       | The address of a built-in provider, such as `OMC_OLLAMA_URL`; listed per provider below.                     | the provider's own address    | [Supported providers](providers/supported.md)              |
| `OMC_HOOKS_FILE`     | The file the user's own hooks for crab are read from.                                                       | `<config>/hooks.json`         | [Hooks](extending/hooks.md)                                |
| `OMC_TRUST_FILE`     | The file trust decisions are kept in.                                                                       | `<data>/trust.json`           | [Project trust](extending/project-trust.md)                |
| `OMC_MCP_FILE`       | The file the user's own tool servers for crab are read from.                                                | `<config>/mcp.json`           | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_CONFIG`     | Set to `0` to read no tool-server file at all, leaving only the servers the client passes.                  | on                            | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_CLAUDE`     | Set to `1` to read the tool servers of Claude Code's `~/.claude.json` too; any other value is ignored.      | unset                         | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_AUTH_FILE`  | The file sign-ins to tool servers are kept in.                                                              | `<config>/mcp-auth.json`      | [Tool servers](extending/mcp.md#signing-in)                |
| `OMC_EXTENSIONS_FILE` | The file the user's own extensions are read from.                                                         | `<config>/extensions.json`    | [Extensions](extending/extensions.md)                      |
| `OMC_EXTENSIONS`     | Set to `0` to start no extension at all, the user's or a project's.                                         | on                            | [Extensions](extending/extensions.md)                      |
| `OMC_TRUST_ALL`      | Set to `1` to trust every project for this process without recording anything; any other value is ignored. | unset                         | [Project trust](extending/project-trust.md)                |
| `OMC_PERMISSIONS_FILE` | The file permission rules and kept "always allow" answers are read from and written to.                  | `<config>/permissions.json`   | [Permission rules](extending/permissions.md)               |
| `OMC_MEMORY`         | Set to `0` to turn memory off for every conversation; the `memory` setting is then not offered.             | on                            | [Memory](memory.md)                                        |
| `OMC_MEMORY_DIR`     | The folder memories are kept in.                                                                            | `<data>/memory`               | [Memory](memory.md)                                        |
| `OMC_DEBUGGERS_FILE` | The file that changes the known debug adapters and adds others.                                             | `<config>/debuggers.json`     | [Debugging](tools/debug.md)                                |
| `OMC_DAP_<ID>`       | Replaces the command of the debug adapter `<ID>`, uppercased with `-` and `.` as `_`: `OMC_DAP_LLDB_DAP`, `OMC_DAP_CODELLDB`, `OMC_DAP_GDB`, `OMC_DAP_DLV`, `OMC_DAP_DEBUGPY`. | the adapter's own search | [Debugging](tools/debug.md) |
| `OMC_PLANS_DIR`      | The folder plans are saved in while the `plan_autosave` setting is on, under the working folder unless it is absolute. An empty value counts as unset. | `.oh-my-crab/plans`           | [Plans](concepts.md#plans)                                 |
| `OMC_SUBAGENT_DEPTH` | How many levels of subagents may run below the main agent: `1` lets no subagent start subagents of its own, and at most `8`. Anything that is not a whole number from 1 counts as unset. | `2`                           | [Subagents](tools/subagents.md#nesting)                    |
| `OMC_ISOLATION`      | How isolated subagents get their copy of the working tree: `auto` (a copy-on-write clone where the file system can make one, else a git worktree checkout), `cow` (clones only) or `worktree` (checkouts only, inside a git repository). Anything else counts as unset. | `auto` | [Subagents](tools/subagents.md#clones-and-checkouts) |
| `OMC_DEFERRED_TOOLS` | The tools hidden from the tool list until the model loads them with `load_tools`: comma-separated names and name prefixes ending in `*`, `default` for the built-in set; `0`, `none` or an empty value hides nothing. | `ast_edit`, `ast_grep`, `debug`, `mcp_resources`, `memory_edit` | [Hidden tools](tools/README.md#hidden-tools) |
| `OMC_BASH_CONDENSE`  | Set to `0` to show `bash` output as it is instead of condensing known noisy commands.                       | on                            | [How tool calls run](tools/calls.md)                       |
| `OMC_BASH_INTERCEPT` | Set to `0` to run every command instead of turning away shell uses a dedicated tool does better.            | on                            | [How tool calls run](tools/calls.md)                       |
| `OMC_EVAL_PYTHON`    | The Python interpreter the `eval` tool starts its Python kernel with. An empty value counts as unset.       | `python3`                     | [eval](tools/eval.md)                                      |
| `OMC_EVAL_NODE`      | The Node.js interpreter the `eval` tool starts its JavaScript kernel with. An empty value counts as unset.  | `node`                        | [eval](tools/eval.md)                                      |
| `OMC_EVAL_MARKER`    | Set by crab inside `eval` kernels for their driver; setting it yourself has no effect.                      | set per kernel                | [eval](tools/eval.md)                                      |
| `OMC_SEARCH`         | The search services `web_search` tries, in order, comma separated: `brave`, `tavily`, `exa`, `perplexity` (or `pplx`), `kagi`, `parallel`, `jina`, `firecrawl`, `tinyfish`, `kimi`, `deepseek`, `synthetic`, `ollama`, `keenable`, `searxng` (or `searx`), `duckduckgo` (or `ddg`). | every service with a key, then those that need none | [Reading the web and GitHub](tools/web-and-github.md) |
| `OMC_SEARXNG_URL`    | The SearXNG instance `web_search` asks, when `SEARXNG_URL` is not set.                                      | unset                         | [Reading the web and GitHub](tools/web-and-github.md)      |
| `OMC_MODELS_REFRESH` | Set to `0` to stop `crab serve` from downloading the model catalog into `<cache>/models.json` once a day; the bundled snapshot and any copy already cached are still used. | on                            | [Model catalog](providers/supported.md#model-catalog)      |
| `OMC_MODELS_URL`     | Where the model catalog is downloaded from, in the models.dev format.                                       | `https://models.dev/api.json` | [Model catalog](providers/supported.md#model-catalog)      |

An empty `OMC_CONFIG_DIR`, `OMC_PROVIDERS_FILE`, `OMC_HOOKS_FILE`, `OMC_TRUST_FILE`,
`OMC_PERMISSIONS_FILE`, `OMC_MEMORY_DIR`, `OMC_DEBUGGERS_FILE`, `OMC_MCP_FILE`,
`OMC_EXTENSIONS_FILE` or `OMC_MCP_AUTH_FILE` counts as unset. An empty or blank
`OMC_<ID>_URL` does too, and a trailing `/` is dropped.

### Provider keys and addresses

A key in the environment wins over one a client saved. Where two key
variables are listed, the first that is set wins. `claude`, `codex` and
`copilot` sign in to an account and read no variable. A custom provider reads
its key from the variables its `api_key_env` names. `bedrock` and `vertex`
find their cloud's credentials as below.

| Provider     | Key                                   | Address variable     | Default address                                           |
| ------------ | ------------------------------------- | -------------------- | --------------------------------------------------------- |
| `anthropic`  | `ANTHROPIC_API_KEY`                   | `OMC_ANTHROPIC_URL`  | `https://api.anthropic.com`                               |
| `bedrock`    | `AWS_BEARER_TOKEN_BEDROCK`, or AWS credentials | `OMC_BEDROCK_URL` | `https://bedrock-runtime.<region>.amazonaws.com`  |
| `vertex`     | Google Cloud credentials              | `OMC_VERTEX_URL`     | `https://<region>-aiplatform.googleapis.com`, or `https://aiplatform.googleapis.com` for `global` |
| `openrouter` | `OPENROUTER_API_KEY`                  | `OMC_OPENROUTER_URL` | `https://openrouter.ai/api/v1`                            |
| `kimi-code`  | `KIMI_CODE_API_KEY`, `KIMI_API_KEY`, or a Kimi account | `OMC_KIMI_CODE_URL` | `https://api.kimi.ai/coding/v1`                |
| `moonshot`   | `MOONSHOT_API_KEY`                    | `OMC_MOONSHOT_URL`   | `https://api.moonshot.ai/v1`                              |
| `zai-coding` | `ZAI_API_KEY`                         | `OMC_ZAI_CODING_URL` | `https://api.z.ai/api/coding/paas/v4`                     |
| `zai`        | `ZAI_API_KEY`                         | `OMC_ZAI_URL`        | `https://api.z.ai/api/paas/v4`                            |
| `minimax`    | `MINIMAX_API_KEY`                     | `OMC_MINIMAX_URL`    | `https://api.minimax.io/v1`                               |
| `deepseek`   | `DEEPSEEK_API_KEY`                    | `OMC_DEEPSEEK_URL`   | `https://api.deepseek.com`                                |
| `xai`        | `XAI_API_KEY`                         | `OMC_XAI_URL`        | `https://api.x.ai/v1`                                     |
| `groq`       | `GROQ_API_KEY`                        | `OMC_GROQ_URL`       | `https://api.groq.com/openai/v1`                          |
| `cerebras`   | `CEREBRAS_API_KEY`                    | `OMC_CEREBRAS_URL`   | `https://api.cerebras.ai/v1`                              |
| `mistral`    | `MISTRAL_API_KEY`                     | `OMC_MISTRAL_URL`    | `https://api.mistral.ai/v1`                               |
| `openai`     | `OPENAI_API_KEY`                      | `OMC_OPENAI_URL`     | `https://api.openai.com/v1`                               |
| `gemini`     | `GEMINI_API_KEY`, else `GOOGLE_API_KEY` | `OMC_GEMINI_URL`   | `https://generativelanguage.googleapis.com/v1beta`        |
| `together`   | `TOGETHER_API_KEY`                    | `OMC_TOGETHER_URL`   | `https://api.together.ai/v1`                              |
| `fireworks`  | `FIREWORKS_API_KEY`                   | `OMC_FIREWORKS_URL`  | `https://api.fireworks.ai/inference/v1`                   |
| `ollama`     | none                                  | `OMC_OLLAMA_URL`     | `http://localhost:11434/v1`                               |
| `lmstudio`   | none                                  | `OMC_LMSTUDIO_URL`   | `http://localhost:1234/v1`                                |
| `llamacpp`   | none                                  | `OMC_LLAMACPP_URL`   | `http://localhost:9931/v1`                                |
| `vllm`       | none                                  | `OMC_VLLM_URL`       | `http://localhost:8000/v1`                                |
| `azure`      | `AZURE_OPENAI_API_KEY`                | see below            | `https://<resource>.openai.azure.com/openai/v1`           |

More on each provider is in [Supported providers](providers/supported.md).

`azure` reads where its resource is on every request:

| Variable                     | What it does                                                                                   | Details |
| ---------------------------- | ---------------------------------------------------------------------------------------------- | ------- |
| `AZURE_OPENAI_ENDPOINT`      | The resource's endpoint, such as `https://name.openai.azure.com`; `/openai/v1` is appended.    | [Azure OpenAI](providers/supported.md#azure-openai) |
| `AZURE_OPENAI_RESOURCE_NAME` | The resource's name, used when no endpoint is set.                                             | [Azure OpenAI](providers/supported.md#azure-openai) |
| `AZURE_OPENAI_BASE_URL`      | The whole API address, which wins over both.                                                   | [Azure OpenAI](providers/supported.md#azure-openai) |
| `AZURE_OPENAI_API_VERSION`   | The `api-version` every request names; `v1` when unset.                                        | [Azure OpenAI](providers/supported.md#azure-openai) |
| `AZURE_OPENAI_DEPLOYMENT`    | The deployments offered as models, separated by commas, the first the default.                 | [Azure OpenAI](providers/supported.md#azure-openai) |

### Cloud credentials

`bedrock` and `vertex` read their cloud's credentials where the cloud's own
tools keep them, on every request, so a refreshed login or rotated key is
picked up without signing in again.

| Variable                                                   | What it does                                                                                     | Details |
| ---------------------------------------------------------- | ------------------------------------------------------------------------------------------------ | ------- |
| `AWS_BEARER_TOKEN_BEDROCK`                                 | A Bedrock API key, sent as a bearer token instead of signing; a key a client saved does the same. | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN` | The AWS keys requests are signed with; they win over a profile.                           | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `AWS_PROFILE`                                              | The profile read from the credentials and config files; `default` when unset.                   | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `AWS_SHARED_CREDENTIALS_FILE`, `AWS_CONFIG_FILE`           | The credentials and config files; `~/.aws/credentials` and `~/.aws/config` when unset.           | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `AWS_REGION`, `AWS_DEFAULT_REGION`                         | The region, first set wins, else the profile's `region`, else `us-east-1`.                       | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `GOOGLE_APPLICATION_CREDENTIALS`                           | A service account key or user login file; else gcloud's `application_default_credentials.json`, else `gcloud auth print-access-token`. | [Google Vertex AI](providers/supported.md#google-vertex-ai) |
| `CLOUDSDK_CONFIG`                                          | gcloud's config folder, where its default credentials are looked for; `~/.config/gcloud` (`%APPDATA%\gcloud` on Windows) when unset. | [Google Vertex AI](providers/supported.md#google-vertex-ai) |
| `GOOGLE_CLOUD_PROJECT`, `GCLOUD_PROJECT`, `ANTHROPIC_VERTEX_PROJECT_ID` | The project, first set wins, else the one the credentials file names.               | [Google Vertex AI](providers/supported.md#google-vertex-ai) |
| `GOOGLE_CLOUD_LOCATION`, `CLOUD_ML_REGION`                 | The region, first set wins, else `global`.                                                       | [Google Vertex AI](providers/supported.md#google-vertex-ai) |

### Search and GitHub

| Variable                                | What it does                                                         | Details                                               |
| --------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------- |
| `BRAVE_API_KEY`, `BRAVE_SEARCH_API_KEY` | Key for Brave search in `web_search`.                                | [Reading the web and GitHub](tools/web-and-github.md) |
| `TAVILY_API_KEY`                        | Key for Tavily search.                                               | [Reading the web and GitHub](tools/web-and-github.md) |
| `EXA_API_KEY`                           | Key for Exa search.                                                  | [Reading the web and GitHub](tools/web-and-github.md) |
| `PERPLEXITY_API_KEY`, `PPLX_API_KEY`    | Key for Perplexity search.                                           | [Reading the web and GitHub](tools/web-and-github.md) |
| `KAGI_API_KEY`                          | Key for Kagi search.                                                 | [Reading the web and GitHub](tools/web-and-github.md) |
| `PARALLEL_API_KEY`                      | Key for Parallel search; without it Parallel's public MCP server is asked. | [Reading the web and GitHub](tools/web-and-github.md) |
| `JINA_API_KEY`                          | Key for Jina search.                                                 | [Reading the web and GitHub](tools/web-and-github.md) |
| `FIRECRAWL_API_KEY`                     | Key for Firecrawl search.                                            | [Reading the web and GitHub](tools/web-and-github.md) |
| `TINYFISH_API_KEY`                      | Key for TinyFish search.                                             | [Reading the web and GitHub](tools/web-and-github.md) |
| `MOONSHOT_SEARCH_API_KEY`, `KIMI_SEARCH_API_KEY` | Kimi Code key for Kimi search.                              | [Reading the web and GitHub](tools/web-and-github.md) |
| `DEEPSEEK_API_KEY`                      | Key for DeepSeek search.                                             | [Reading the web and GitHub](tools/web-and-github.md) |
| `SYNTHETIC_API_KEY`                     | Key for Synthetic search.                                            | [Reading the web and GitHub](tools/web-and-github.md) |
| `OLLAMA_API_KEY`, `OLLAMA_CLOUD_API_KEY` | Key for Ollama's web search.                                        | [Reading the web and GitHub](tools/web-and-github.md) |
| `KEENABLE_API_KEY`                      | Key for Keenable search; without it Keenable's public endpoint is asked. | [Reading the web and GitHub](tools/web-and-github.md) |
| `SEARXNG_URL`                           | The SearXNG instance to ask; wins over `OMC_SEARXNG_URL`.            | [Reading the web and GitHub](tools/web-and-github.md) |
| `GH_TOKEN`, `GITHUB_TOKEN`              | Token for GitHub's API when the `gh` tool is not installed and signed in; without one GitHub is asked anonymously. | [Reading the web and GitHub](tools/web-and-github.md) |

### Other programs' folders and the system

| Variable                                           | What it does                                                                                                   | Default            | Details                                     |
| -------------------------------------------------- | -------------------------------------------------------------------------------------------------------------- | ------------------ | ------------------------------------------- |
| `CLAUDE_CONFIG_DIR`                                | Claude Code's config folder, read for instructions, skills, agents, hooks, settings and plugins, and for `.claude.json` with `OMC_MCP_CLAUDE=1`. | `~/.claude`        | [Skills and agents](extending/skills-and-agents.md)                 |
| `CODEX_HOME`                                       | Codex's config folder, read for instructions, skills and agents.                                               | `~/.codex`         | [Skills and agents](extending/skills-and-agents.md) |
| `HOME`                                             | The home folder: the default of the two above and `~/.agents/skills`; a project that is this folder is the user's own. | the system's | [Project trust](extending/project-trust.md) |
| `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_CACHE_HOME` | On Linux, where `<config>`, `<data>` and `<cache>` are.                                                      | see [Folders](#folders) | [Folders](#folders)                    |
| `PATH`                                             | Where language servers, debug adapters and other programs crab starts are looked for.                          | the system's       | [Language services](language-services.md)   |
| `PATHEXT`                                          | On Windows, the extensions a program found on `PATH` may have.                                                 | `.EXE;.CMD;.BAT`   | [Language services](language-services.md)   |
| `HOSTNAME`, `COMPUTERNAME`                         | The machine name `kimi-code` sends, when the system does not give one.                                         | empty              | [Supported providers](providers/supported.md) |

### Set for hooks

Crab does not read these; it sets them for the hook commands it runs.

| Variable             | What it holds                                         | Details                     |
| -------------------- | ----------------------------------------------------- | --------------------------- |
| `CLAUDE_PROJECT_DIR` | The project folder.                                   | [Hooks](extending/hooks.md) |
| `CLAUDE_PLUGIN_ROOT` | The plugin's folder, for a hook a plugin brought.     | [Hooks](extending/hooks.md) |

## Files

### Crab's own

| Path                                                          | What                                                                                                    | Written by                          | Details                                           |
| ------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------- | ----------------------------------- | ------------------------------------------------- |
| `<config>/auth.json`                                          | Logins, saved API keys and the provider last signed in to; for `bedrock` and `vertex` an empty entry marks them signed in, their credentials staying with the cloud's own tools. A client can keep it elsewhere by opening the conversation with a `home` folder: then it is `<home>/auth.json`. | crab, when signing in or saving a key | [Providers](providers/overview.md)            |
| `<config>/providers.json`                                     | Custom providers and OpenAI-compatible servers. Read when the providers are first listed, and again when a client saves or removes a server. Moved by `OMC_PROVIDERS_FILE`, not by `home`. | the user                     | [Custom endpoints](providers/custom-endpoints.md) |
| `<config>/slots.json`                                         | The user's model slots, each a model and its backups, and the presets. Read for every turn. Moved by `OMC_SLOTS_FILE`. | the user, or crab on `SetSlot`, `ClearSlot` and the preset commands | [Models and slots](models-and-slots.md) |
| `<config>/hooks.json`                                         | The user's own hooks. Moved by `OMC_HOOKS_FILE`.                                                         | the user                            | [Hooks](extending/hooks.md)                       |
| `<config>/mcp.json`                                           | The user's own tool servers, in the shape of `.mcp.json`. Read when a conversation opens. Moved by `OMC_MCP_FILE`. | the user                  | [Tool servers](extending/mcp.md)                  |
| `<config>/extensions.json`                                    | The user's own extensions. Read when a conversation opens. Moved by `OMC_EXTENSIONS_FILE`.               | the user                            | [Extensions](extending/extensions.md)             |
| `<config>/mcp-auth.json`                                      | Sign-ins to tool servers: client registrations and tokens, by server address, readable by the user alone. Shared by every conversation, including those opened with their own `home` folder for a provider login. Moved by `OMC_MCP_AUTH_FILE`. | crab, when signing in to a tool server | [Tool servers](extending/mcp.md#signing-in) |
| `<config>/permissions.json`                                   | Permission rules: the user's under `permissions`, each project's under `projects`, and the "always allow" answers kept for the project or every project. Moved by `OMC_PERMISSIONS_FILE`. | crab, on an "always allow" kept beyond the conversation or `crab permissions`; the user | [Permission rules](extending/permissions.md) |
| `<config>/AGENTS.md`, `<config>/rules/<name>.md`              | The user's own instruction files, before Claude Code's and Codex's. This and the three rows below are moved by `OMC_CONFIG_DIR`. | the user, or a client | [Skills and agents](extending/skills-and-agents.md) |
| `<config>/skills/<name>/SKILL.md`                             | The user's own skills; one keeps its name over Claude Code's and Codex's.                                | the user, or a client               | [Skills and agents](extending/skills-and-agents.md) |
| `<config>/agents/<name>.md`                                   | The user's own agent definitions; one wins its name over Claude Code's and Codex's.                      | the user, or a client               | [Skills and agents](extending/skills-and-agents.md) |
| `<config>/SYSTEM.md`, `<config>/APPEND_SYSTEM.md`             | Text that replaces crab's built-in instructions in the main agent's system prompt, and text added to the end of every system prompt, unless the project has its own. | the user, or a client | [The system prompt](extending/skills-and-agents.md#the-system-prompt) |
| `<config>/debuggers.json`                                     | Changes to the known debug adapters, and others added. Moved by `OMC_DEBUGGERS_FILE`.                    | the user                            | [Debugging](tools/debug.md)                       |
| `<data>/conversations/<id>.jsonl`                             | The saved history of the conversation `<id>`. A client can keep them elsewhere by opening the conversation with a `history` folder. | crab                | [Commands and conversations](usage.md)            |
| `<data>/conversations/<id>.plans/plan-<n>.md`                 | Each plan the agent of the conversation `<id>` submitted, numbered in order, beside its history, readable by the user alone. | crab             | [Plans](concepts.md#plans)                        |
| `<data>/memory/projects/<name>-<hash>/memories.jsonl`         | A project's memories, one JSON object a line, beside `project.json` (which project it is) and `memories.lock`. Moved by `OMC_MEMORY_DIR`. | crab; the user may edit it | [Memory](memory.md)                    |
| `<data>/memory/user/memories.jsonl`                           | The user's memories, kept across every project.                                                          | crab; the user may edit it          | [Memory](memory.md)                               |
| `<data>/trust.json`                                           | Trust decisions per project, with the hook commands each was trusted with. Moved by `OMC_TRUST_FILE`.    | crab, on a `Trust` command or `crab trust` | [Project trust](extending/project-trust.md) |
| `<cache>/output/<conversation id>/`                           | The whole output of tool calls that were cut short; `run-<pid>-<seconds>` for a conversation without an id. Removed after seven days unwritten. | crab               | [How tool calls run](tools/calls.md)              |
| `<cache>/models.json`                                         | The model catalog downloaded from models.dev: context windows, output limits, efforts, input kinds and prices. Its providers replace those of the snapshot compiled into crab. Downloaded again when a day old, unless `OMC_MODELS_REFRESH` is `0`. | crab | [Model catalog](providers/supported.md#model-catalog) |
| `<cache>/worktrees/<conversation id>/`                        | The copies isolated subagents work in, git worktrees or copy-on-write clones, and the untouched clones of folders outside a repository they start from, named like the conversation's output folder. Removed a day after they were made. | crab | [Subagents](tools/subagents.md)                   |

On Unix the memory and output folders and their files are readable by the
user alone.

### The user's other tools

Always read, trusted or not. `~/.claude` is `CLAUDE_CONFIG_DIR` when set, and
`~/.codex` is `CODEX_HOME`.

| Path                                                       | What                                                                  | Details                                     |
| ---------------------------------------------------------- | --------------------------------------------------------------------- | ------------------------------------------- |
| `~/.claude/CLAUDE.md`, `~/.claude/rules/<name>.md`         | Instruction files.                                                    | [Skills and agents](extending/skills-and-agents.md) |
| `~/.codex/AGENTS.md`, `~/.codex/AGENTS.override.md`        | Instruction files.                                                    | [Skills and agents](extending/skills-and-agents.md) |
| `~/.claude/skills/<name>/SKILL.md`, `~/.codex/skills/<name>/SKILL.md`, `~/.agents/skills/<name>/SKILL.md` | Skills.                  | [Skills and agents](extending/skills-and-agents.md) |
| `~/.claude/agents/<name>.md`, `~/.codex/agents/<name>.toml` | Agent definitions.                                                   | [Skills and agents](extending/skills-and-agents.md) |
| `~/.claude/settings.json`                                  | Hooks, the plugins `enabledPlugins` switches on or off, and permission rules. | [Hooks](extending/hooks.md), [permission rules](extending/permissions.md) |
| `~/.aws/credentials`, `~/.aws/config`                      | AWS keys, `credential_process` and region of the profile in use, for `bedrock`. Moved by `AWS_SHARED_CREDENTIALS_FILE` and `AWS_CONFIG_FILE`. | [Amazon Bedrock](providers/supported.md#amazon-bedrock) |
| `~/.config/gcloud/application_default_credentials.json`    | Google's Application Default Credentials, for `vertex`. `GOOGLE_APPLICATION_CREDENTIALS` names another file. | [Google Vertex AI](providers/supported.md#google-vertex-ai) |
| `~/.claude/plugins/installed_plugins.json`                 | The installed Claude Code plugins, whose skills, agents and `hooks/hooks.json` are read. | [Claude Code plugins](extending/skills-and-agents.md#claude-code-plugins) |
| `~/.claude.json`                                           | Only with `OMC_MCP_CLAUDE=1`: the tool servers under `mcpServers`, and under `projects` for the working folder or repository root. | [Tool servers](extending/mcp.md) |

### The project

Read only once the project is trusted, from every folder from the working
folder up to the repository root (just the working folder outside a
repository), except where noted.

| Path                                                                       | What                                                            | Details                                     |
| -------------------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------- |
| `.oh-my-crab/AGENTS.md`, `.oh-my-crab/rules/<name>.md`, `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `.claude/rules/<name>.md` | Instruction files.  | [Skills and agents](extending/skills-and-agents.md) |
| `.oh-my-crab/skills/<name>/SKILL.md`, `.claude/skills/<name>/SKILL.md`, `.codex/skills/<name>/SKILL.md`, `.agents/skills/<name>/SKILL.md` | Skills; crab's own keep their names first. | [Skills and agents](extending/skills-and-agents.md) |
| `.oh-my-crab/agents/<name>.md`, `.claude/agents/<name>.md`, `.codex/agents/<name>.toml` | Agent definitions; crab's own win their names first. | [Skills and agents](extending/skills-and-agents.md) |
| `.oh-my-crab/SYSTEM.md`, `.oh-my-crab/APPEND_SYSTEM.md`                    | The system prompt's replaced instructions and added text; the nearest folder wins, then `<config>`'s. | [The system prompt](extending/skills-and-agents.md#the-system-prompt) |
| `.oh-my-crab/hooks.json`, `.claude/settings.json`, `.claude/settings.local.json` | Hooks.                                                    | [Hooks](extending/hooks.md)                 |
| `.claude/settings.json`, `.claude/settings.local.json` of the working folder only | The plugins `enabledPlugins` switches on or off.         | [Project trust](extending/project-trust.md) |
| `.oh-my-crab/mcp.json`, `.mcp.json`                                        | Tool servers; in each folder the first wins, and the nearer folder wins. | [Tool servers](extending/mcp.md)            |
| `.oh-my-crab/extensions.json`                                              | Extensions; the nearer folder wins.                                       | [Extensions](extending/extensions.md)       |
| `.oh-my-crab/slots.json` of the working folder                             | Model slots that win over the user's slot by slot, and presets.          | [Models and slots](models-and-slots.md)     |
| `.oh-my-crab/plans/<title>.md` of the working folder                       | Written, not read: each submitted plan while `plan_autosave` is on, named after its title. Moved by `OMC_PLANS_DIR`. | [Plans](concepts.md#plans)                  |
| `.claude/settings.json`, `.claude/settings.local.json`                     | Permission rules: deny and ask rules are read whether the project is trusted or not, allow rules only while its hooks would run. | [Permission rules](extending/permissions.md) |

Tool servers come from the client, the project, crab's own `mcp.json` and,
when asked for, Claude Code's `~/.claude.json`, in that order of precedence
([Tool servers](extending/mcp.md)). What a project brings is not used until
it is trusted: see [Project trust](extending/project-trust.md).

## Conversation settings

A conversation offers these settings to its client, which changes one with
the `Set` command of the [wire protocol](wire-protocol.md). The `defaults` of the command that opens a conversation
give the values the saved conversation has no choice for; a choice that was
saved wins. Each conversation saves its own choices.

| Setting         | Values                                                     | Default                                                       | Offered                                                       | Details                                       |
| --------------- | ---------------------------------------------------------- | ------------------------------------------------------------- | ------------------------------------------------------------- | --------------------------------------------- |
| `model`         | the models the provider lists, and the one in use; `@slot` names a slot's model | the model the client opened with, else the `main` slot's, else the provider's default | always                                                        | [Supported providers](providers/supported.md) |
| `mode`          | `default` (ask before changing things), `yolo`, `plan`     | `default`, or `yolo` when the client opened with yolo          | always                                                        | [Long conversations](long-conversations.md)   |
| `effort`        | those of `low`, `medium`, `high`, `xhigh`, `max` the model takes | `medium`, else the model's lightest                     | when the model takes an effort                                | [Supported providers](providers/supported.md) |
| `cache`         | `5m`, `1h`                                                 | `5m`                                                          | when the provider lets the cache lifetime be picked (Claude)  | [Long conversations](long-conversations.md)   |
| `cache_warming` | `on`, `off`                                                | `on`                                                          | when the provider's cache can be kept warm                    | [Long conversations](long-conversations.md)   |
| `memory`        | `on`, `off`                                                | `on`                                                          | unless `OMC_MEMORY` turns memory off or there is no `<data>`  | [Memory](memory.md)                           |
| `plan_model`    | `same`, the models the provider lists, or any model as `model`, `provider/model` or `@slot` | `same`: plan with the `plan` slot's model, else `model` | always                                                        | [Plans](concepts.md#plans)                    |
| `plan_autosave` | `on`, `off`                                                | `off`                                                         | always                                                        | [Plans](concepts.md#plans)                    |
| `snapshots`     | `on`, `off`                                                | `on`                                                          | always                                                        | [Files of a turn](wire-protocol.md#files-of-a-turn) |
| `checkpoints`   | `on`, `off`                                                | `off`                                                         | always                                                        | [Long conversations](long-conversations.md)   |
