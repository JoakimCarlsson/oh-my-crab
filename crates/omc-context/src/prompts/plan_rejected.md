The user did not approve the plan yet. Plan mode is still active: stay read-only.
{{#if note}}
The user said what should change:

<user-note>
{{note}}
</user-note>

Revise the plan to address it, and call `exit_plan_mode` again with the full revised plan when it is decision-complete.
{{else}}
Ask what should change if it is unclear, revise the plan, and call `exit_plan_mode` again with the full revised plan when it is decision-complete.
{{/if}}
