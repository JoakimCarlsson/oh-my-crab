<project-context>
<workstation>
- Working directory: {{directory}}
- OS: {{os}}
{{#if model}}- Model: {{model}}{{/if}}
</workstation>

{{#if tree}}
<workspace-tree>
Working directory layout, snapshot from the start of this conversation; NOT updated as files change. Folders end in `/`; depth ≤ 3; ignored, hidden, build and vendored folders omitted; long folders and deep levels cut. Use it to skip listing folders; read or search before relying on a path.
{{tree}}
</workspace-tree>
{{/if}}

{{#if rules}}
<repo-rules>
MUST follow these context files for all tasks; nearer files override farther ones:
{{rules}}
</repo-rules>

Context files above auto-loaded; those of one folder or kind of file arrive in a `<path-rules>` block at the end of the first read or edit of a matching file. NEVER `grep`/`glob` for `AGENTS.md`, `CLAUDE.md`, or similar agent/context files: relevant files already in context; others noise.
{{/if}}
</project-context>
