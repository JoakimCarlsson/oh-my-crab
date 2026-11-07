# Hooks

Hooks are commands the user sets up to run at points of a conversation: when
it opens, resumes or has been compacted, before and after a tool call, when a
prompt is about to start a turn, when a subagent starts and ends, when the
agent is about to end its turn, before a compaction, when crab waits on the
reader, and when the conversation ends. A hook is a shell
command line, run with `bash -c` (`cmd /C` on Windows) in the conversation's
folder, that reads a JSON payload on standard input and answers on standard
output and with its exit code. The format is Claude Code's, as its hooks
reference at <https://code.claude.com/docs/en/hooks> describes it, so hooks a
user already has keep working:

```json
{
  "hooks": {
    "PreToolUse": [
      {
        "matcher": "Bash",
        "hooks": [{ "type": "command", "command": "~/bin/no-rm.sh", "timeout": 10 }]
      }
    ]
  }
}
```

They are read from crab's own `hooks.json` in its
[config folder](../configuration.md#folders) (`~/.config/oh-my-crab/` on
Linux), or the file `OMC_HOOKS_FILE` names; from Claude Code's `settings.json` in `~/.claude`
(or `CLAUDE_CONFIG_DIR`); from `hooks/hooks.json` of each installed Claude Code
plugin, run with `CLAUDE_PLUGIN_ROOT` set to the plugin's folder; and, once the
project is trusted, from `.oh-my-crab/hooks.json`, `.claude/settings.json` and
`.claude/settings.local.json` in the folders from the working folder up to the
repository root. Hooks from every file run; a file that sets
`disableAllHooks` turns them all off. The events crab runs are in the table
below; other events and other hook types than `command` are skipped, so one
file can serve both programs. A hook may run for `timeout` seconds, 60 by
default, 30 for `UserPromptSubmit` and 1.5 for `SessionEnd`, which holds up
closing and may be given at most 60; past that it is killed with everything it
started. Hooks of one event that match run at once, and the same command
listed twice runs once.

| Event | When | Matcher | Adds to the payload | Can block |
| ----- | ---- | ------- | ------------------- | --------- |
| `SessionStart` | The conversation opened (`startup`), a saved one was opened again (`resume`), or it was just compacted (`compact`) | the source | `source`, `model` | no |
| `UserPromptSubmit` | A prompt is about to start a turn | none | `prompt` | yes |
| `PreToolUse` | Before a tool call is asked about or run | the tool | `tool_name`, `tool_input`, `tool_use_id` | yes |
| `PostToolUse` | After a tool call succeeded | the tool | the same, and `tool_response` | no, adds a note |
| `PostToolUseFailure` | After a tool call ran and failed; a `bash` command that exits with another code than 0 is a result, not a failure | the tool | the same as `PreToolUse`, and `error`, `is_interrupt` (always `false`), `duration_ms` | no, adds a note |
| `SubagentStart` | A subagent is about to start | the kind of subagent | `agent_id`, `agent_type` | no |
| `SubagentStop` | A subagent is about to end its work | the kind of subagent | `stop_hook_active`, `last_assistant_message` | yes |
| `Stop` | The agent is about to end its turn | none | `stop_hook_active`, `last_assistant_message` | yes |
| `PreCompact` | Before the conversation is summarized: on `/compact` (`manual`) or when the context nears the window (`auto`) | the trigger | `trigger`, `custom_instructions` (what followed `/compact`, or `null`) | yes |
| `Notification` | Crab waits on the reader: a permission question or a plan to approve (`permission_prompt`), a form of questions (`elicitation_dialog`), or no command for 60 seconds after a turn ended (`idle_prompt`) | the kind | `message`, `title`, `notification_type` | no |
| `SessionEnd` | The client closed the conversation while no turn ran (`prompt_input_exit`), hung up or closed it during a turn (`other`), or opened a new conversation (`clear`) or a saved one (`resume`) in its place | the reason | `reason` | no |

A matcher names the tools a hook runs for. Empty or `*` is every tool; names
made of letters, digits, `_` and `-` are exact, several split by `|` or `,`;
a pattern whose only other characters are `*` and `?` is a glob over the whole
name, such as `mcp__*`; anything else is a regular expression that may match
anywhere in the name, such as `Edit.*` or `^mcp__github__`. A tool matches by
crab's name (`bash`, `edit`) and by Claude Code's name for the same tool
(`Bash`, `Edit` or `MultiEdit`). The other events match the word the table
names, such as `startup|resume` or `auto`, and `UserPromptSubmit` and `Stop`
take no matcher.

Every payload holds `session_id` (the conversation's id, or empty), `cwd`,
`hook_event_name` and `permission_mode` (`default`, `plan` or
`bypassPermissions` in yolo mode), and `agent_id` and `agent_type` when a
subagent runs the hook: subagents run the same hooks as the main agent, under
the id of the `task` call that started them, followed by `/` and their place
when the call started several. A tool call adds `tool_name`, `tool_input` (the
arguments, already put right against the tool's schema) and `tool_use_id`, and
after the call `tool_response` with the `output` the model will read. A hook
from one of Claude Code's files is told Claude Code's name for the tool, and
`tool_input.file_path` holds the `path` of the call made absolute. A prompt
adds `prompt`, and the end of a turn `stop_hook_active` and
`last_assistant_message`. The project folder is in `CLAUDE_PROJECT_DIR`.

A hook answers with exit code 0 and, when it has something to say, a JSON
object on standard output, or with exit code 2 and a reason on standard error:

- Before a tool call, exit code 2, `"permissionDecision": "deny"` in
  `hookSpecificOutput` or the older `"decision": "block"` refuses the call, and
  the model reads the reason as the call's failure. `"allow"` (or `"approve"`)
  runs it without asking the reader. `updatedInput` replaces the arguments,
  which are put right against the schema again and fail the call when they
  do not fit, and `additionalContext` is added to the result. Every denial
  wins over every allowance, and the last rewrite wins.
- After a tool call, `additionalContext`, the `reason` of `"decision":
  "block"` and what exit code 2 writes to standard error are added to the
  result as a `<hook-note>`, which the model reads and the client shows.
- When a prompt is submitted, exit code 2, `"decision": "block"` or
  `"continue": false` keeps it from the model; the turn ends with the reason as
  its failure and is saved as one that failed. Plain text on standard output
  and `additionalContext` are told to the model before the prompt. While a
  hook runs on prompts, a prompt sent during a turn waits for the turn to end
  and starts a turn of its own, so it is checked too.
- When the agent is about to end its turn, exit code 2, `"decision": "block"`
  or `additionalContext` has it go on with the reason as a notice; at most 8
  times in a row before the model calls a tool again and 24 times a turn, and
  `"continue": false` always lets the turn end.
- After a tool call that failed, `additionalContext`, the `reason` of
  `"decision": "block"` and what exit code 2 writes to standard error are
  added to the failure as a `<hook-note>`, as after a call that succeeded.
- When the conversation starts, plain text on standard output and
  `additionalContext` are told to the model in a `<session-start-hook>` notice
  at the start of the next prompt, like the other notices described in
  [long conversations](../long-conversations.md#prompt-cache), so the system
  prompt and the tool list stay as they were and the prompt cache keeps them.
  The hooks run on their own when the conversation opens, and the first prompt
  waits for them. After a compaction they run again for `compact`, since the
  summary drops the earlier notice, and the next prompt carries what they say
  then. A notice that was told in a turn that failed or was stopped is told
  again with the next prompt.
- When a subagent starts, `additionalContext` comes before its assignment in a
  `<subagent-start-hook>` block.
- Before a compaction, exit code 2 or `"decision": "block"` keeps it from
  happening. One the reader asked for fails with the reason; an automatic one
  is skipped and the turn goes on without it, and is not tried again in that
  turn; when the provider had already refused the request as too long, the turn
  fails. Plain text on standard output is added to what the summary should give
  particular attention to, after the reader's own focus.
- `Notification` and `SessionEnd` hooks cannot change anything: their answer
  is ignored, and what exit code 2 writes to standard error is written to
  crab's standard error for the reader. A conversation that closes waits for
  its `SessionEnd` hooks before it ends. At `SessionStart` and `SubagentStart`
  exit code 2 blocks nothing either.

A hook that fails with another exit code is ignored, with a line on standard
error, and what it guarded goes on. A hook that times out or cannot be started
gave no answer: before a tool call that refuses the call, since its check never
ran, and the model is told so; everywhere else the conversation goes on as
if it had not been there. A turn never waits on a hook longer than its
timeout, and stopping the turn kills the hooks it runs.

The hooks that run at an event are the ones that apply at that moment: a
project that is trusted later in the conversation adds its hooks from then on,
so its `SessionStart` hooks first run after the next compaction or when the
conversation is opened again.
