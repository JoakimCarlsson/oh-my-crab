# Configuration

Everything a user can configure in crab, in one place: environment variables,
files and the settings of a conversation. Each row links to the page that
says more. Crab has no settings file of its own for its general behaviour;
what is not set here is chosen per conversation by the client.

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
| `OMC_PROVIDERS_FILE` | The file custom providers are read from.                                                                    | `<config>/providers.json`     | [Custom endpoints](providers/custom-endpoints.md)          |
| `OMC_<ID>_URL`       | The address of a built-in provider, such as `OMC_OLLAMA_URL`; listed per provider below.                     | the provider's own address    | [Supported providers](providers/supported.md)              |
| `OMC_HOOKS_FILE`     | The file the user's own hooks for crab are read from.                                                       | `<config>/hooks.json`         | [Hooks](extending/hooks.md)                                |
| `OMC_TRUST_FILE`     | The file trust decisions are kept in.                                                                       | `<data>/trust.json`           | [Project trust](extending/project-trust.md)                |
| `OMC_MCP_FILE`       | The file the user's own tool servers for crab are read from.                                                | `<config>/mcp.json`           | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_CONFIG`     | Set to `0` to read no tool-server file at all, leaving only the servers the client passes.                  | on                            | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_CLAUDE`     | Set to `1` to read the tool servers of Claude Code's `~/.claude.json` too; any other value is ignored.      | unset                         | [Tool servers](extending/mcp.md)                           |
| `OMC_MCP_AUTH_FILE`  | The file sign-ins to tool servers are kept in.                                                              | `<config>/mcp-auth.json`      | [Tool servers](extending/mcp.md#signing-in)                |
| `OMC_TRUST_ALL`      | Set to `1` to trust every project for this process without recording anything; any other value is ignored. | unset                         | [Project trust](extending/project-trust.md)                |
| `OMC_MEMORY`         | Set to `0` to turn memory off for every conversation; the `memory` setting is then not offered.             | on                            | [Memory](memory.md)                                        |
| `OMC_MEMORY_DIR`     | The folder memories are kept in.                                                                            | `<data>/memory`               | [Memory](memory.md)                                        |
| `OMC_DEBUGGERS_FILE` | The file that changes the known debug adapters and adds others.                                             | `<config>/debuggers.json`     | [Debugging](tools/debug.md)                                |
| `OMC_DAP_<ID>`       | Replaces the command of the debug adapter `<ID>`, uppercased with `-` and `.` as `_`: `OMC_DAP_LLDB_DAP`, `OMC_DAP_CODELLDB`, `OMC_DAP_GDB`, `OMC_DAP_DLV`, `OMC_DAP_DEBUGPY`. | the adapter's own search | [Debugging](tools/debug.md) |
| `OMC_SUBAGENT_DEPTH` | How many levels of subagents may run below the main agent: `1` lets no subagent start subagents of its own, and at most `8`. Anything that is not a whole number from 1 counts as unset. | `2`                           | [Subagents](tools/subagents.md#nesting)                    |
| `OMC_BASH_CONDENSE`  | Set to `0` to show `bash` output as it is instead of condensing known noisy commands.                       | on                            | [How tool calls run](tools/calls.md)                       |
| `OMC_BASH_INTERCEPT` | Set to `0` to run every command instead of turning away shell uses a dedicated tool does better.            | on                            | [How tool calls run](tools/calls.md)                       |
| `OMC_SEARCH`         | The search services `web_search` tries, in order, comma separated: `brave`, `tavily`, `exa`, `perplexity`, `kagi`, `searxng` (or `searx`), `duckduckgo` (or `ddg`). | every service set up, DuckDuckGo last | [Reading the web and GitHub](tools/web-and-github.md) |
| `OMC_SEARXNG_URL`    | The SearXNG instance `web_search` asks, when `SEARXNG_URL` is not set.                                      | unset                         | [Reading the web and GitHub](tools/web-and-github.md)      |

An empty `OMC_PROVIDERS_FILE`, `OMC_HOOKS_FILE`, `OMC_TRUST_FILE`,
`OMC_MEMORY_DIR`, `OMC_DEBUGGERS_FILE`, `OMC_MCP_FILE` or `OMC_MCP_AUTH_FILE`
counts as unset. An empty or blank
`OMC_<ID>_URL` does too, and a trailing `/` is dropped.

### Provider keys and addresses

A key in the environment wins over one a client saved. Where two key
variables are listed, the first that is set wins. `claude`, `codex` and
`copilot` sign in to an account and read no variable. A custom provider reads
its key from the variables its `api_key_env` names.

| Provider     | Key                                   | Address variable     | Default address                                           |
| ------------ | ------------------------------------- | -------------------- | --------------------------------------------------------- |
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
| `gemini`     | `GEMINI_API_KEY`                      | `OMC_GEMINI_URL`     | `https://generativelanguage.googleapis.com/v1beta/openai` |
| `together`   | `TOGETHER_API_KEY`                    | `OMC_TOGETHER_URL`   | `https://api.together.ai/v1`                              |
| `fireworks`  | `FIREWORKS_API_KEY`                   | `OMC_FIREWORKS_URL`  | `https://api.fireworks.ai/inference/v1`                   |
| `ollama`     | none                                  | `OMC_OLLAMA_URL`     | `http://localhost:11434/v1`                               |
| `lmstudio`   | none                                  | `OMC_LMSTUDIO_URL`   | `http://localhost:1234/v1`                                |
| `llamacpp`   | none                                  | `OMC_LLAMACPP_URL`   | `http://localhost:9931/v1`                                |
| `vllm`       | none                                  | `OMC_VLLM_URL`       | `http://localhost:8000/v1`                                |

More on each provider is in [Supported providers](providers/supported.md).

### Search and GitHub

| Variable                                | What it does                                                         | Details                                               |
| --------------------------------------- | -------------------------------------------------------------------- | ----------------------------------------------------- |
| `BRAVE_API_KEY`, `BRAVE_SEARCH_API_KEY` | Key for Brave search in `web_search`.                                | [Reading the web and GitHub](tools/web-and-github.md) |
| `TAVILY_API_KEY`                        | Key for Tavily search.                                               | [Reading the web and GitHub](tools/web-and-github.md) |
| `EXA_API_KEY`                           | Key for Exa search.                                                  | [Reading the web and GitHub](tools/web-and-github.md) |
| `PERPLEXITY_API_KEY`, `PPLX_API_KEY`    | Key for Perplexity search.                                           | [Reading the web and GitHub](tools/web-and-github.md) |
| `KAGI_API_KEY`                          | Key for Kagi search.                                                 | [Reading the web and GitHub](tools/web-and-github.md) |
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
| `<config>/auth.json`                                          | Logins, saved API keys and the provider last signed in to. A client can keep it elsewhere by opening the conversation with a `home` folder: then it is `<home>/auth.json`. | crab, when signing in or saving a key | [Providers](providers/overview.md)            |
| `<config>/providers.json`                                     | Custom providers. Read once, when the providers are first listed. Moved by `OMC_PROVIDERS_FILE`, not by `home`. | the user                     | [Custom endpoints](providers/custom-endpoints.md) |
| `<config>/hooks.json`                                         | The user's own hooks. Moved by `OMC_HOOKS_FILE`.                                                         | the user                            | [Hooks](extending/hooks.md)                       |
| `<config>/mcp.json`                                           | The user's own tool servers, in the shape of `.mcp.json`. Read when a conversation opens. Moved by `OMC_MCP_FILE`. | the user                  | [Tool servers](extending/mcp.md)                  |
| `<config>/mcp-auth.json`                                      | Sign-ins to tool servers: client registrations and tokens, by server address, readable by the user alone. `<home>/mcp-auth.json` for a conversation opened with a `home` folder. Moved by `OMC_MCP_AUTH_FILE`. | crab, when signing in to a tool server | [Tool servers](extending/mcp.md#signing-in) |
| `<config>/debuggers.json`                                     | Changes to the known debug adapters, and others added. Moved by `OMC_DEBUGGERS_FILE`.                    | the user                            | [Debugging](tools/debug.md)                       |
| `<data>/conversations/<id>.jsonl`                             | The saved history of the conversation `<id>`. A client can keep them elsewhere by opening the conversation with a `history` folder. | crab                | [Commands and conversations](usage.md)            |
| `<data>/memory/projects/<name>-<hash>/memories.jsonl`         | A project's memories, one JSON object a line, beside `project.json` (which project it is) and `memories.lock`. Moved by `OMC_MEMORY_DIR`. | crab; the user may edit it | [Memory](memory.md)                    |
| `<data>/memory/user/memories.jsonl`                           | The user's memories, kept across every project.                                                          | crab; the user may edit it          | [Memory](memory.md)                               |
| `<data>/trust.json`                                           | Trust decisions per project, with the hook commands each was trusted with. Moved by `OMC_TRUST_FILE`.    | crab, on a `Trust` command or `crab trust` | [Project trust](extending/project-trust.md) |
| `<cache>/output/<conversation id>/`                           | The whole output of tool calls that were cut short; `run-<pid>-<seconds>` for a conversation without an id. Removed after seven days unwritten. | crab               | [How tool calls run](tools/calls.md)              |
| `<cache>/worktrees/<conversation id>/`                        | The git worktrees of isolated subagents, named like the conversation's output folder. Removed a day after they were made. | crab                     | [Subagents](tools/subagents.md)                   |

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
| `~/.claude/settings.json`                                  | Hooks, and the plugins `enabledPlugins` switches on or off.           | [Hooks](extending/hooks.md)                 |
| `~/.claude/plugins/installed_plugins.json`                 | The installed Claude Code plugins, whose skills, agents and `hooks/hooks.json` are read. | [Claude Code plugins](extending/skills-and-agents.md#claude-code-plugins) |
| `~/.claude.json`                                           | Only with `OMC_MCP_CLAUDE=1`: the tool servers under `mcpServers`, and under `projects` for the working folder or repository root. | [Tool servers](extending/mcp.md) |

### The project

Read only once the project is trusted, from every folder from the working
folder up to the repository root (just the working folder outside a
repository), except where noted.

| Path                                                                       | What                                                            | Details                                     |
| -------------------------------------------------------------------------- | --------------------------------------------------------------- | ------------------------------------------- |
| `AGENTS.md`, `AGENTS.override.md`, `CLAUDE.md`, `.claude/CLAUDE.md`, `CLAUDE.local.md`, `.claude/rules/<name>.md` | Instruction files.  | [Skills and agents](extending/skills-and-agents.md) |
| `.claude/skills/<name>/SKILL.md`, `.codex/skills/<name>/SKILL.md`, `.agents/skills/<name>/SKILL.md` | Skills.                           | [Skills and agents](extending/skills-and-agents.md) |
| `.claude/agents/<name>.md`, `.codex/agents/<name>.toml`                    | Agent definitions.                                              | [Skills and agents](extending/skills-and-agents.md) |
| `.oh-my-crab/hooks.json`, `.claude/settings.json`, `.claude/settings.local.json` | Hooks.                                                    | [Hooks](extending/hooks.md)                 |
| `.claude/settings.json`, `.claude/settings.local.json` of the working folder only | The plugins `enabledPlugins` switches on or off.         | [Project trust](extending/project-trust.md) |
| `.oh-my-crab/mcp.json`, `.mcp.json`                                        | Tool servers; in each folder the first wins, and the nearer folder wins. | [Tool servers](extending/mcp.md)            |

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
| `model`         | the models the provider lists, and the one in use          | the model the client opened with, else the provider's default | always                                                        | [Supported providers](providers/supported.md) |
| `mode`          | `default` (ask before changing things), `yolo`, `plan`     | `default`, or `yolo` when the client opened with yolo          | always                                                        | [Long conversations](long-conversations.md)   |
| `effort`        | those of `low`, `medium`, `high`, `xhigh`, `max` the model takes | `medium`, else the model's lightest                     | when the model takes an effort                                | [Supported providers](providers/supported.md) |
| `cache`         | `5m`, `1h`                                                 | `5m`                                                          | when the provider lets the cache lifetime be picked (Claude)  | [Long conversations](long-conversations.md)   |
| `cache_warming` | `on`, `off`                                                | `on`                                                          | when the provider's cache can be kept warm                    | [Long conversations](long-conversations.md)   |
| `memory`        | `on`, `off`                                                | `on`                                                          | unless `OMC_MEMORY` turns memory off or there is no `<data>`  | [Memory](memory.md)                           |
| `checkpoints`   | `on`, `off`                                                | `off`                                                         | always                                                        | [Long conversations](long-conversations.md)   |
