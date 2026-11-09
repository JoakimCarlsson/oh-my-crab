Plan approved. Plan mode is over and full tool access is restored.

<instruction>
Execute the plan step-by-step; MUST verify each step before the next.
{{#if has_todo}}Before execution: initialize todo tracking with `todo`. After each completed step: immediately update `todo`.{{/if}}
{{#if note}}The user's note below comes with the approval: follow it where it adds to or changes the plan.{{/if}}
</instruction>

<plan>
{{plan}}
</plan>
{{#if note}}

<user-note>
{{note}}
</user-note>
{{/if}}
