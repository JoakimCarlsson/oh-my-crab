# Your worktree
You work in a git worktree of your own at `{{directory}}`: a copy of the main agent's working tree as it was when you started, uncommitted changes included. Change files there freely; when you end, everything you changed is handed back to the main agent as a patch for its working tree. Do NOT commit, and do NOT touch files outside this folder.
{{#if submodules}}
These submodules are not checked out in your copy, and changes to them are not handed back: {{submodules}}.
{{/if}}
