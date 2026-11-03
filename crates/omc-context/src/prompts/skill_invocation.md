The user invoked the skill `{{name}}`. Follow its instructions for this request.

<skill name="{{name}}" path="{{path}}" base="{{base}}">
{{body}}
</skill>

{{#if request}}
{{request}}
{{/if}}
