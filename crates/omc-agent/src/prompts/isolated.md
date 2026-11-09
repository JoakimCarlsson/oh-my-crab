# Your worktree
You work in a worktree of your own at `{{directory}}`: a copy of the main agent's working tree as it was when you started, uncommitted changes included. Change files there freely; when you end, everything you changed is handed back to the main agent as a patch for its working tree. Do NOT commit, and do NOT touch files outside this folder.
{{#if cloned}}
It is a copy-on-write clone, so files git ignores, such as installed dependencies and build output, are there too and builds and tests can run at once; changes to ignored files are not handed back.
{{/if}}
{{#if submodules}}
These submodules are not checked out in your copy, and changes to them are not handed back: {{submodules}}.
{{/if}}
