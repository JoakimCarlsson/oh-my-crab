<project-context>
<workstation>
- Working directory: {{directory}}
- OS: {{os}}
- Date: {{date}}
{{#if branch}}- Git branch: {{branch}}{{/if}}
{{#if model}}- Model: {{model}}{{/if}}
</workstation>

{{#if rules}}
<repo-rules>
MUST follow these context files for all tasks; nearer files override farther ones:
{{rules}}
</repo-rules>

Context files above auto-loaded. NEVER `grep`/`glob` for `AGENTS.md`, `CLAUDE.md`, or similar agent/context files: relevant files already in context; others noise.
{{/if}}
</project-context>
