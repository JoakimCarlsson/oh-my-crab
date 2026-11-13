# Skills, agents and instruction files

Crab reads three kinds of context from disk: instruction files the agent must
follow, skills it loads when a task matches, and agent definitions that add
kinds of subagent. It reads them from folders of its own first, then where
Claude Code and Codex keep theirs, so a user's existing files keep working and
a file meant for crab alone changes neither of the other tools. What a project
folder brings is used only once the project is trusted (see
[Project trust](project-trust.md)); the user's own folders are always read.
`<config>` stands for crab's config folder (`~/.config/oh-my-crab` on Linux,
see [Folders](../configuration.md#folders)), which `OMC_CONFIG_DIR` moves;
`~/.claude` for Claude Code's config folder, which `CLAUDE_CONFIG_DIR` moves;
and `~/.codex` for Codex's, which `CODEX_HOME` moves.

Crab's own files are kept in `.oh-my-crab` in a project folder and in
`<config>` for the user, laid out the same way in both: `AGENTS.md` and
`rules/<name>.md` for instruction files, `skills/<name>/SKILL.md` for skills,
`agents/<name>.md` for agent definitions, and `SYSTEM.md` and
`APPEND_SYSTEM.md` for the [system prompt](#the-system-prompt). A client that
writes a new one for crab writes it there.

The project's folders are the folders from the working folder up to the root
of the repository it is in, nearest first. Outside a repository only the
working folder counts. A repository root is the nearest folder holding a
`.git` entry.

## Instruction files

In each of the project's folders, nearest first, crab reads these, in this
order:

- `.oh-my-crab/AGENTS.md`
- every `.md` file under `.oh-my-crab/rules`, folders inside it included, in
  name order
- `CLAUDE.md`
- `.claude/CLAUDE.md`
- `CLAUDE.local.md`
- every `.md` file under `.claude/rules`, folders inside it included, in name
  order
- `AGENTS.md`
- `AGENTS.override.md`

Then the user's own: `<config>/AGENTS.md`, every `.md` file under
`<config>/rules`, `~/.claude/CLAUDE.md`, every `.md` file under
`~/.claude/rules`, `~/.codex/AGENTS.md` and `~/.codex/AGENTS.override.md`.
Every file found is used; `AGENTS.override.md` is read alongside `AGENTS.md`,
not instead of it. A file that is empty or only whitespace is skipped, a file
reached twice (a `CLAUDE.md` that links to `AGENTS.md`, say) is read once, and
only the first 64 KiB of a file is used.

The files go into the system prompt, in the order above, each as a
`<file path="...">` block inside `<repo-rules>`, with the rule that nearer
files override farther ones. Subagents get the same block in their own
prompts. The model is told the files are already loaded, so it does not search
for them.

### Rules for some files only

Two kinds of instruction file stay out of the system prompt and reach the
agent only when it works on the files they are about:

- A rule file (a `.md` file under one of the `rules` folders above) whose
  front matter names paths under `paths` or `globs`:

  ```markdown
  ---
  paths:
    - "src/**/*.{ts,tsx}"
    - "!src/generated/**"
  ---
  Use strict mode.
  ```

  The patterns are written like `.gitignore` lines, one to a line or separated
  by commas: a pattern without a slash matches at any depth, and a pattern
  starting with `!` takes files out again. A project's rule is matched from
  the folder that holds its `.oh-my-crab` or `.claude` folder; the user's own
  rules are
  matched from the repository root, or from the working folder outside a
  repository. `alwaysApply: true` in the front matter keeps a rule in the
  system prompt whatever paths it names. Only what follows the front matter is
  handed over.
- The instruction files listed above, in a folder below the project's folders:
  `crates/ui/AGENTS.md` when crab runs in the repository root, say. They apply
  to every file in their folder and below; a rule file there with `paths` or
  `globs` is matched from that folder. Folders are looked in only below the
  repository root, or below the working folder outside a repository, and never
  inside the folders of version control, builds and vendored packages (`.git`,
  `node_modules`, `vendor`, `target`, `build` and the like).

The first time the agent reads, edits or writes a matching file, the files
that apply are added to the end of that call's result, in a `<path-rules>`
block, nearest folder first, then the rules. Each is handed over once for as
long as the model still sees it: when the result that carried it is pruned,
compacted, rewound or dropped for a fresh context window, the next matching
call carries it again. A subagent is handed them with its own first matching
call. Other tools, `bash` and `grep` among them, bring nothing. Files turned
off in `disabled` and the files of an untrusted project are never handed
over, and a change to a rule's text during a conversation reaches the agent
only with a later hand-over, not as a `<context-changes>` notice.

## Skills

A skill is a folder holding a `SKILL.md`. Crab looks for skill folders in:

- `.oh-my-crab/skills`, `.claude/skills`, `.codex/skills` and
  `.agents/skills` in each of the project's folders, nearest first, one
  location at a time
- `<config>/skills`, `~/.claude/skills`, `~/.codex/skills` and
  `~/.agents/skills`
- installed Claude Code plugins (see below)

Only the folders directly inside a skills folder are skills; one whose name
starts with `.` is skipped. `SKILL.md` starts with front matter:

```markdown
---
name: release
description: Cuts a release. Use when the user asks to tag or publish a version.
---
1. Run `make check`.
2. Bump the version in `Cargo.toml` and tag it.
```

- `name` is what the skill is called; the folder's name when it is missing or
  empty. A name holding `/` or `\` makes the skill unusable and it is skipped.
- `description` is how the model knows when to load the skill.
- `disable-model-invocation: true` or `hide: true` keeps the skill out of the
  list in the system prompt, so only the user starts it.
- `enabled: false` turns the skill off.

Other keys are ignored. The first skill found under a name keeps it. A later
skill of the same name whose front matter and body are identical is dropped;
one that differs is offered as `<namespace>/<name>`, the namespace being the
plugin's name or the name of the folder that holds the skills folder without
its leading dot (`claude/pdf` for a second `pdf` in `~/.claude/skills`), with
`~2`, `~3` and so on added when that is taken too. Since crab's own folders
come first in each scope, a skill in `.oh-my-crab/skills` keeps its name over
one in the same project's `.claude/skills`, and one in `<config>/skills` over
one in `~/.claude/skills`; a project's skill still keeps its name over the
user's.

The system prompt lists each skill that is not hidden as `- name:
description` and tells the model to load a matching skill before it starts.
The model loads one with the `skill` tool: `name` returns the body of
`SKILL.md` without its front matter, and `path` returns a file inside the
skill's folder, given relative to it (absolute paths and `..` are refused).
Each answer is cut at 100,000 bytes and names the skill's folder. A hidden
skill can still be loaded by name. When a conversation has no skills the
`skill` tool is still offered, so the tool list stays the same, and refuses
every call.

Every skill, hidden or not, is also a slash command. The `Commands` event lists
`compact` and then each skill with its description, and is sent again whenever
the skills change. A prompt whose first word is `/name` or `$name` for a skill
starts with the skill loaded: the model reads that the user invoked it, then
the body inside `<skill name="..." path="..." base="...">`, then the rest of
the prompt as the request. The body is passed as written; nothing in it is
filled in. A prompt naming no known skill is sent as it is.

## Agent definitions

Agent definitions add kinds of subagent to the `task` tool (see
[Subagents](../tools/subagents.md)). Crab reads them from:

- every `.md` file under `.oh-my-crab/agents` and `.claude/agents` and every
  `.toml` file under `.codex/agents`, in each of the project's folders,
  nearest first
- every `.md` file under `<config>/agents` and `~/.claude/agents` and every
  `.toml` file under `~/.codex/agents`
- every `.md` file under the `agents` folder of each installed Claude Code
  plugin
- the built-in kinds `task`, `scout` and `reviewer`, last

The first definition of a name wins, so a project's `reviewer.md` replaces the
built-in `reviewer`, and one in `.oh-my-crab/agents` or `<config>/agents`
replaces Claude Code's of the same name in the same scope. A markdown definition has front matter and a body, which
becomes the subagent's role:

```markdown
---
name: auditor
description: Checks a change for unsafe code and reports each finding.
tools: read, grep, glob
output_schema: {"type":"object","properties":{"findings":{"type":"array","items":{"type":"string"}}},"required":["findings"]}
---
Look for `unsafe` blocks in the files named and explain each.
```

- `name`: the kind's name; the file's name without `.md` (and without a
  trailing `.agent`) when it is missing.
- `description`: what the kind is for, shown to the main agent in the `task`
  tool's description. A file with neither a description nor a body is skipped.
- `tools`: the tools the subagent may use, by crab's names, as a comma
  separated list, a bracketed list or `- item` lines; case is ignored. Without
  it the subagent has every tool the main agent has, less the ones subagents
  never get. A kind whose tools all only look (`read`, `grep`, `glob`,
  `ast_grep`, `web_search`, `skill`, `ask`, `todo`, `memory_search`) is marked
  read-only to the main agent.
- `output_schema`: a JSON Schema object, written as JSON on the line. The
  subagent then hands its result to `submit_result` and the main agent gets
  checked JSON. A value that is not a JSON object is ignored.
- `isolated`: `true`, `yes` or `on` runs the subagent in a git worktree of its
  own unless the call says otherwise.
- `model`: the model the subagent asks, as Claude Code writes it: an alias
  (`haiku`, `sonnet`, `opus`), a model id, or `inherit`, the same as leaving it
  out, for the model of the agent that starts it. A provider and a model id as
  `provider/model` picks a model of another signed-in provider. A call's own
  `model` wins over it. See [Subagents](../tools/subagents.md#models) for how
  the name is looked up.
- `tools` decides whether the subagent may start subagents of its own: it may
  when the list is missing or holds `task` (or Claude Code's `agent`), and the
  levels allowed are not used up.

Other keys are ignored. A TOML definition reads the top-level strings `name`,
`description`, `developer_instructions` (or `instructions`, which it must have
one of), `output_schema`, `isolated` and `model`, stopping at the first table;
it cannot limit tools.

## The system prompt

Two files change the main agent's system prompt itself. Crab looks for each in
`.oh-my-crab` in the project's folders, nearest first and only once the
project is trusted, then in `<config>`; the first one found that is not empty
or only whitespace is used, cut at 64 KiB.

- `SYSTEM.md` replaces crab's built-in instructions: its role, its engineering
  and tool policy, its workflow and delivery rules. What crab builds for the
  conversation still follows it: the skills, the tool inventory, the request
  to name an unnamed conversation, the workstation, the outline of the working
  directory, the instruction files and, in plan mode, its rules. The text is
  used as written; nothing in it is filled in.
- `APPEND_SYSTEM.md` is added after the project part, built-in instructions
  or `SYSTEM.md` alike, before the rules of plan mode.

A subagent's prompt starts from its own role, so `SYSTEM.md` does not reach
it; `APPEND_SYSTEM.md` does, after its environment. Both files are turned off
by path in `disabled`, like instruction files, and, as for the rest of the
system prompt, a change to them is used the next time the prompt is made.

## Claude Code plugins

Crab reads the plugins Claude Code installed from
`~/.claude/plugins/installed_plugins.json`. Each entry is used unless its
install record says `"enabled": false` or `enabledPlugins` switches it off. An
install with the scope `project` or `local` counts only when its
`projectPath` is the working folder, unless `enabledPlugins` switches the
plugin on. `enabledPlugins` is read from `~/.claude/settings.json` and then,
when the project is trusted, from the working folder's
`.claude/settings.json` and `.claude/settings.local.json`; a later file wins:

```json
{ "enabledPlugins": { "kit@market": true, "noisy@market": false } }
```

A plugin brings skills, agent definitions and hooks (see [Hooks](hooks.md)).
Its skills are in its `skills` folder and in the folders its
`.claude-plugin/plugin.json` names under `skills`; a folder named there may be
a skill itself. When the plugin's own folder holds a marketplace
(`marketplace.json` or `.claude-plugin/marketplace.json`) that lists it with
`"source": "./"`, the folders that entry names under `skills` are read too, and
the `skills` folder only when neither file names any. Folders outside the
plugin are ignored. A plugin's skill keeps its own name, and is
named `<plugin>/<name>` only when that name is taken.

## Turning things off

The client can leave out what the user turned off with the `disabled` list of
`Open`: instruction files and `SYSTEM.md` or `APPEND_SYSTEM.md` by path (as
found, or with links resolved), and
skills and agent kinds by name, built-in kinds included:

```json
{"Open":{"directory":"/work/app","provider":"claude","model":null,"yolo":false,
 "disabled":["/work/app/CLAUDE.local.md","claude/pdf","reviewer"]}}
```

Skill names are settled before the list is applied, so turning one skill off
never renames another; a renamed skill is turned off by its new name.

## Changes on disk

Before every prompt crab reads all of this again, with the project's trust
assessed again first, so a skill added since the last turn can be invoked at
once and a new agent kind can be started. When the skills change the client
gets a new `Commands` event.

The system prompt itself is not rewritten: it is made once, so that the
provider's prompt cache keeps it, and made again only when something it is
built for changes, such as the model. What changed since it was made reaches
the model as a `<context-changes>` notice at the start of the next prompt:
skills and agent kinds that are new or have a new description, those that are
gone, instruction files that are new or changed, with what they now say, and
those that were removed. Hidden skills are not mentioned. The notice is sent
only when it differs from the last one the model can still see, and when
everything is back as the system prompt describes it, a notice says so.
