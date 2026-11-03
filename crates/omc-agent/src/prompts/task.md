Runs a subagent on a self-contained assignment and returns its final message as the result. Several `task` calls in one response run in parallel.

# Delegation
Use most specific agent. Prefer one agent to investigate + edit. Omit `agent` only for the default (`task`); NEVER specify it.
Shared edits need one integration owner. Set interfaces in the task. Every task MUST skip build/lint/tests/formatters mid-flight; run once afterward.
Children start blank and cannot ask the user or start subagents of their own; give them everything they need.

# Format
`task`: self-contained (`# Target` files/non-goals, `# Change` steps/APIs, `# Acceptance` observable result).
`description`: a few words naming the work, shown to the user.

# Available Agents
{{agents}}
