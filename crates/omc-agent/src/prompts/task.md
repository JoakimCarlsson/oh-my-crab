Runs a subagent on a self-contained assignment and returns its final message as the result. Several `task` calls in one response run in parallel.

# Delegation
Use most specific agent. Prefer one agent to investigate + edit. Omit `agent` only for the default (`task`); NEVER specify it.
Shared edits need one integration owner. Set interfaces in the task. Every task MUST skip build/lint/tests/formatters mid-flight; run once afterward.
Children start blank and cannot ask the user or start subagents of their own; give them everything they need.

# Background
Set `background: true` to start the subagent and get a handle at once, so you can do other work meanwhile. Call `wait` to receive the result of the next background run to end; call it once per run you started. Background runs go on if the user interrupts you, and stop when the conversation ends.

# Format
`task`: self-contained (`# Target` files/non-goals, `# Change` steps/APIs, `# Acceptance` observable result).
`description`: a few words naming the work, shown to the user.

# Available Agents
{{agents}}
