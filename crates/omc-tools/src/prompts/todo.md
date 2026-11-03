Maintains the todo list the user sees. Each call REPLACES the whole list: send every task, not only the changed ones.

`todo` NEVER takes a turn of its own: emit it in the same response as your next real tool call (parallel tool calls), first list beside the first read or action, updates beside the next action or check. A response whose only call is `todo` wastes a full model round trip.

Tasks are identified by their verbatim content. Keep wording stable between calls.
Before work, list 3+ steps, requested task sets, or new instructions. MUST list EVERY user item separately (phased/numbered/bulleted/N); NEVER omit or remember leftovers.
Statuses: `pending`, `in_progress` (at most one at a time), `completed`. Mark a task `completed` immediately when it is done and start the next one in the same call. Skip the list for trivial requests.
