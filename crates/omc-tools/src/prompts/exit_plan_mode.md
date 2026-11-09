Submits your plan to the user for approval and ends plan mode when they approve. Only available in plan mode.

Call it ONLY when the plan is decision-complete: `plan` is the full plan as markdown, every time, never a diff against an earlier version. The user may:
- approve it: you get full tool access and MUST execute the plan, here or in a fresh context window that starts from the plan alone;
- send it back with a note: you stay in plan mode; revise the plan to address the note and call this again;
- reject it: you stay in plan mode; stop and wait for the user.
The submitted plan is saved and stays in your context through compaction.
NEVER ask for approval in prose.
