<approved-plan>
{{plan}}
</approved-plan>

The user approved the plan above and opened this fresh context window to carry it out. The conversation that produced the plan is not in this window: plan mode is over and full tool access is restored.

<instruction>
Execute the plan step-by-step; MUST verify each step before the next.
Re-read every file before changing it: what you learned while planning is not in this window.
{{#if has_todo}}Before execution: initialize todo tracking with `todo`. After each completed step: immediately update `todo`.{{/if}}
{{#if note}}The user's note below comes with the approval: follow it where it adds to or changes the plan.{{/if}}
</instruction>
{{#if note}}

<user-note>
{{note}}
</user-note>
{{/if}}
