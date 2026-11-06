Runs subagents on self-contained assignments and returns their final messages as the result.

# Delegation
Use most specific agent. Prefer one agent to investigate + edit. Omit `agent` only for the default (`task`); NEVER specify it.
Shared edits need one integration owner. Set interfaces in the task. Every task MUST skip build/lint/tests/formatters mid-flight; run once afterward.
Children start blank and cannot ask the user or start subagents of their own; give them everything they need.

# Batches
One subagent: set `task` (with `agent`, `description`). Several independent slices: set `tasks`, a list of `{agent, task, description, output_schema}`, instead; up to {{most}} per call, {{parallel}} running at once. They run in parallel and the result labels each one's outcome; one failing does not stop the others. Prefer one call with `tasks` over many `task` calls. A call-level `agent` or `output_schema` applies to every item that omits it.

# Structured output
Set `output_schema` (a JSON Schema) when you need data back, not prose: findings to merge, lists to act on. The subagent must then hand its result to a `submit_result` tool; the value is checked against the schema and returned to you as JSON. A subagent that never submits a matching value fails.

# Isolation
Set `isolated: true` (for the call, or per item) to run an editing subagent in a git worktree of its own: a copy of your working tree, uncommitted changes included, so parallel subagents never trample each other's files or yours. When it ends, its changes are applied to your working tree as an uncommitted patch, siblings in list order, and the result lists the files changed. A patch that does not apply cleanly is NOT applied: the result names the conflicting files and where the patch is saved, so you merge it yourself. Needs a git repository; read-only agents gain nothing from it. Agents marked (isolated) work this way unless you set `isolated: false`.

# Background
Set `background: true` to start the subagents and get a handle for each at once, so you can do other work meanwhile. Call `wait` to receive the result of the next background run to end; call it once per handle. Background runs go on if the user interrupts you, and stop when the conversation ends.

# Format
`task`: self-contained (`# Target` files/non-goals, `# Change` steps/APIs, `# Acceptance` observable result).
`description`: a few words naming the work, shown to the user.

# Available Agents
{{agents}}
