Runs subagents on self-contained assignments and returns their final messages as the result.

# Delegation
Use most specific agent. Prefer one agent to investigate + edit. Omit `agent` only for the default (`task`); NEVER specify it.
Shared edits need one integration owner. Set interfaces in the task. Every task MUST skip build/lint/tests/formatters mid-flight; run once afterward.
{{nesting}}

# Batches
One subagent: set `task` (with `agent`, `description`). Several independent slices: set `tasks`, a list of `{agent, task, description, output_schema, model}`, instead; up to {{most}} per call, {{parallel}} running at once. They run in parallel and the result labels each one's outcome; one failing does not stop the others. Prefer one call with `tasks` over many `task` calls. A call-level `agent`, `output_schema`, `isolated` or `model` applies to every item that omits it.

# Structured output
Set `output_schema` (a JSON Schema) when you need data back, not prose: findings to merge, lists to act on. The subagent must then hand its result to a `submit_result` tool; the value is checked against the schema and returned to you as JSON. A subagent that never submits a matching value fails.

# Isolation
Set `isolated: true` (for the call, or per item) to run an editing subagent in a git worktree of its own: a copy of your working tree, uncommitted changes included, so parallel subagents never trample each other's files or yours. When it ends, its changes are applied to your working tree as an uncommitted patch, siblings in list order, and the result lists the files changed. A patch that does not apply cleanly is NOT applied: the result names the conflicting files and where the patch is saved, so you merge it yourself. Needs a git repository; read-only agents gain nothing from it. Agents marked (isolated) work this way unless you set `isolated: false`.

# Models
A subagent asks your model unless its kind names another (shown as `model:` below) or you set `model`: an alias such as `haiku`, `sonnet` or `opus` (the provider's newest of that name), `inherit` for yours, an exact model id, or `provider/model` for another signed-in provider. Pick a smaller, faster model for simple lookups and a stronger one for hard reasoning. A model that cannot be used fails the call with the reason.

# Background
Set `background: true` to start the subagents and get a handle for each at once, so you can do other work meanwhile. Call `wait` to receive the result of the next background run to end; call it once per handle. Background runs go on if the user interrupts you, and stop when the conversation ends.

# Follow-ups
A finished subagent is kept for a while with everything it read and did, and its result ends with its handle, such as `sub-3`. To ask it for more ("now also fix X", "why did you rule out Y?"), set `continue` to that handle and `task` to the follow-up alone: it goes on from where it stopped, so NEVER repeat the assignment or start a new subagent that must investigate again. Only `description` and `background` go with `continue`. Subagents that were isolated, returned structured output, failed or were stopped are not kept; when a handle is gone the call says so, and you start a new subagent with a self-contained `task`.

# Format
`task`: self-contained (`# Target` files/non-goals, `# Change` steps/APIs, `# Acceptance` observable result).
`description`: a few words naming the work, shown to the user.

# Available Agents
{{agents}}
