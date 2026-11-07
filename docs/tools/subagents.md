# Subagents

The `task` tool hands self-contained assignments to subagents, each running a
loop of its own with the main agent's tools, less the ones that start
subagents or talk to the reader, and less `debug`, whose sessions belong to
the conversation and would be steered by several subagents at once. The kinds come with the harness (`task`,
`scout` and `reviewer`) or from agent definitions in the project's and the
user's agent folders and in installed Claude Code plugins (see
[Skills, agents and instruction files](../extending/skills-and-agents.md)). One call runs one assignment, or up to 16 given as
`tasks`, eight at a time, and their results come back under a heading each; one
failing leaves the others be. With `background` the call returns a handle per
subagent at once and `wait` returns each result as it ends. A call or a
definition can give an `output_schema`: the subagent then hands its result to
`submit_result`, which checks it against the schema, and the main agent gets
the checked JSON instead of prose.

A subagent can work in a git worktree of its own, so that subagents editing at
once never trample each other's files or the main agent's. The call sets
`isolated: true`, for every assignment or for one item of `tasks`, or a
definition sets `isolated: true` in its front matter, which a call can turn off
with `isolated: false`. The worktree is made from the working tree `crab` runs
in, whether that is the repository's own checkout or a linked worktree, such as
the one an editor gives each agent session, and it starts from that tree as it
is: uncommitted changes and new files are copied in, ignored files are not. The
state is written into a commit no branch points at, through an index of its
own so the main agent's index is untouched, and the worktree checks it out on a
detached head, so no branch name is taken. Worktrees are made under
`oh-my-crab/worktrees/<conversation id>` in the user's cache folder, never
inside the working tree, so its status, searches and file watchers never see
them, and they are locked with the reason `oh-my-crab task worktree`, which
tells them apart in `git worktree list --porcelain`. No hook of the repository
runs when one is made. The subagent's tools, `bash` included, are confined to
its worktree; it asks the harness's own language servers, started for the
worktree, when the conversation uses those, and none when the client provides
them, since the client's servers know only the main working tree. Tool-server
tools are shared with the main agent. Submodules are not checked out in a
worktree and changes to them are not handed back; the subagent is told which.

When an isolated subagent ends, what it changed, committed or not, is taken as
a patch against the commit it started from and its worktree is removed. The
patch is applied to the main working tree, uncommitted and without touching the
index, so a client reviewing the session's diff sees it; the subagents of one
call are applied in the order of the list, once all of them ended. A patch that
does not apply cleanly is not applied at all: it is saved in the conversation's
output folder, and the result names the files that conflict and where the
patch is, for the main agent to merge itself. The changes of a subagent that
failed are saved the same way and not applied. Every result ends with a
`# Changes` part listing the files changed or what became of them, and the
`task` call's tool event carries the worktree's folder and the outcome in its
`worktree` field. A worktree whose subagent is stopped is removed at once; one
left behind by a crash is removed when its conversation opens again, and other
conversations' worktrees made more than a day before are removed then too.
Isolation needs `git` and a git working tree; a call asking for it anywhere
else is refused with the reason and starts nothing, rather than running the
subagents unisolated.
