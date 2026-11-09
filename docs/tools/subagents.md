# Subagents

The `task` tool hands self-contained assignments to subagents, each running a
loop of its own with the main agent's tools, less the ones that talk to the
reader, and less `debug`, whose sessions belong to the conversation and would
be steered by several subagents at once. The kinds come with the harness (`task`,
`scout` and `reviewer`) or from agent definitions in the project's and the
user's agent folders and in installed Claude Code plugins (see
[Skills, agents and instruction files](../extending/skills-and-agents.md)). One call runs one assignment, or up to 32 given as
`tasks`, and their results come back under a heading each; one failing leaves
the others be. Thirty-two subagents run at once across the whole conversation,
whatever call or level started them (see [Nesting](#nesting)). With
`background` the call returns a handle per subagent at once and `wait` returns
each result as it ends; only the main agent can start subagents in the
background. A call or a
definition can give an `output_schema`: the subagent then hands its result to
`submit_result`, which checks it against the schema, and the main agent gets
the checked JSON instead of prose.

## Models

A subagent asks the model of the agent that starts it, unless its definition
names a `model` or the call gives one, for the single assignment or for an
item of `tasks`; the call's wins, and a call saying `inherit` keeps the model
of the agent starting it whatever the definition says. A name is looked up
among the models the conversation's provider listed when it opened:

- an id equal to the name, in any case;
- otherwise the first listed id holding the name, so that an alias such as
  `haiku`, `sonnet` or `opus` picks the provider's best model of that family,
  the provider listing its best models first;
- otherwise, when the name starts with the id of a provider and a `/`, such
  as `codex/gpt-5`, the model is looked up the same way among that provider's
  models, listed for it then. The reader must be signed in to it.

When the provider listed no models, the name is used as it is. A name that
matches nothing fails the call before any subagent starts, and the error the
model reads lists some of the models there are. The subagent then thinks at
the conversation's effort when its model takes that level, and otherwise at
its model's default: medium when it offers that, else its lightest.

A subagent's system prompt names the model it asks. Its own model requests do
not count towards the conversation's context use: its `Used` events are not
sent, whichever model it asks. In plan mode, a subagent that would ask the
main agent's model asks the `plan_model` instead, when one is set.

## The approved plan

While the main agent carries out a plan the reader approved, every subagent
it starts, at every level, gets the plan at the end of its system prompt, in
a `<parent-plan>` block that tells it its assignment is one part of that plan.
The plan is the latest approved one in the conversation's history, so it
follows a reopened conversation and the branch it is on. Subagents started in
plan mode, while a new plan is made, get none. See
[plans](../concepts.md#plans).

## Nesting

A subagent may start subagents of its own with a `task` tool of its own, two
levels below the main agent by default: the main agent's subagents can start
subagents, and those cannot. `OMC_SUBAGENT_DEPTH` sets how many levels may run
below the main agent (`1` lets no subagent start subagents; at most 8). A
subagent at the deepest level, or whose definition lists tools without `task`,
gets no `task` tool, and the `task` tool's description tells each agent whether
its children may start more. A nested subagent asks its parent's model unless
its definition or the call names another, draws its tools from its parent's,
and its work is reported under its parent's call, the ids chained as in
[Tool events are updates](../wire-protocol.md#tool-events-are-updates).

All subagents of a conversation share 32 seats, however deep they are: a
subagent takes a seat to run and holds it until it ends, and the rest wait.
A subagent waiting for its own subagents gives its seat up while they run and
takes one again once they ended, so a tree of subagents waiting on each other
can never hold every seat and stall. Approvals are shared with the main agent
at every level, as are the hooks, which see each subagent under its own id.

## Worktrees

A subagent can work in a copy of its own of the working tree, so that
subagents editing at once never trample each other's files or the main
agent's. The call sets `isolated: true`, for every assignment or for one item
of `tasks`, or a definition sets `isolated: true` in its front matter, which a
call can turn off with `isolated: false`. The copy is made from the working
tree `crab` runs in, whether that is the repository's own checkout, a linked
worktree, such as the one an editor gives each agent session, or a folder
outside any repository, and it starts from that tree as it is, uncommitted
changes and new files included.

### Clones and checkouts

`OMC_ISOLATION` picks how the copy is made:

| Value      | Inside a git repository                                                        | Outside one                |
| ---------- | ------------------------------------------------------------------------------ | -------------------------- |
| `auto`     | A copy-on-write clone where the file system can make one, else a checkout. The default. | A copy-on-write clone, or refused. |
| `cow`      | A copy-on-write clone, or refused.                                              | A copy-on-write clone, or refused. |
| `worktree` | A checkout, as before clones were made.                                         | Refused.                   |

A copy-on-write clone shares the storage of the files it was made from until
either side writes, so it is made in moments whatever the tree holds, and it
holds everything the tree does, ignored files included: installed
dependencies such as `node_modules` and build output such as `target` are
there, so the subagent can build and run tests at once. Changes to ignored
files are not handed back. Clones are made with `clonefile(2)` on APFS
(macOS), and with the `FICLONE` ioctl on Linux file systems that share
extents (btrfs, XFS, bcachefs, OCFS2); clones need the cache folder on the
same volume as the working tree. A checkout writes every file anew from the
repository and leaves ignored files out. Under `auto` a working tree that
cannot be cloned is checked out instead, without saying so; the subagent is
told which it got.

Inside a repository, either way, the state is written into a commit no branch
points at, through an index of its own so the main agent's index is untouched,
and the copy is a git worktree on a detached head at that commit, so no branch
name is taken. A clone is a worktree registered without a checkout and filled
with clones of the working tree's own files, its `.git` left out. Worktrees are made under
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
A clone is moved aside and forgotten by git at once when its subagent ends,
and its files are deleted in the background, so a clone holding many ignored
files does not hold the result up.

### Outside a repository

Outside a git repository, where `git` is not installed too, the folder is
cloned once, untouched, as the state the subagents of a call start from, and
each subagent works in a clone of that. When one ends, its clone is compared
with the untouched one file by file: a file whose size and modification time
are the same is taken as unchanged, any other is compared byte for byte, and
files a `.gitignore` names are left out on both sides. The changed, added and
removed files are applied to the folder only if every one of them is there
still as it was when the subagent started, or already as the subagent left
it; otherwise none is applied, and they are saved in the conversation's output
folder as a unified diff, which `git apply` and `patch -p1` read, with the
files that conflict named, just as a patch that does not apply is inside a
repository. Where clones cannot be made, or with `OMC_ISOLATION=worktree`, a
call asking for isolation outside a repository is refused with the reason and
starts nothing, rather than running the subagents unisolated.

### Nesting

Isolation composes with nesting. A subagent working in a worktree of its own
that starts an isolated subagent has that subagent's worktree made from its
own worktree, as it is then, its changes so far included, and what that
subagent changed is applied to its parent's worktree when it ends. The parent
hands both its own changes and its children's back when it ends in turn. A
nested subagent that is not isolated works in its parent's folder, whatever
that is.
