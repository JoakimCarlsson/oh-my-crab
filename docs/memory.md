# Memory

The agent keeps a memory that outlives conversations. With `remember` it saves
one durable fact or lesson at a time, such as a build quirk, a convention the
code does not spell out or a preference the user stated, with a few tags if it
likes; `memory_search` finds memories by the words of their text and tags,
forgiving typos and word endings, and by their meaning, best match first (see
[searching by meaning](#searching-by-meaning)); and `memory_edit` corrects
a memory or forgets it by its id. Each memory records when it was saved and
the conversation that saved it, and when and by which conversation it was last
changed. Saving text that is already kept, whatever its case and spacing, adds
the new tags to the memory that is there instead of keeping it twice.

Memories belong to a project by default. In a git repository the project is
the repository: the main checkout and every linked worktree of it, such as the
one an editor gives each agent session, share one memory, while a submodule
and an unrelated repository with the same folder name each have their own.
Outside a repository the project is the folder. A memory saved with
`scope: "user"` is kept for the user across every project instead.

The first prompt of a conversation starts with a digest of the memories in a
`<memory>` notice, those that share the most words with the prompt first and
the most recently changed among equals, at most 4,000 bytes with each memory
cut to 300; a last line counts the ones left out, which `memory_search` still
finds. With nothing saved yet, the notice says so. The digest is told once:
memories saved later in the conversation reach the model through the tool
call that saved them, and the system prompt and the tool list never change
for them, so the prompt cache holds. After a compaction or a fresh context
window has dropped the digest, the next prompt carries a new one.

## Searching by meaning

Once the embedding model of the [code index](tools/code-search.md) is on disk,
`memory_search` also finds memories by meaning: the query and each memory's
text and tags are embedded with the same model, and the memories whose cosine
with the query is at least 0.25 are ranked by it. That ranking and the ranking
by words are fused by reciprocal rank, so `spelling preferences` finds
`User prefers British English in answers.` though the two share no word, and a
memory that matches both ways comes first. Tags and scope filter
both rankings alike. Memories are few and short, so they are embedded at each
search rather than kept in an index.

`memory_search` never downloads the model itself: the first `code_search` of
any conversation does. Until then, and with `OMC_CODE_INDEX=0`, it searches by
words alone, as it always did, and a query without words that count lists the
most recent memories either way.

## Subagents, storage and settings

Subagents can search the memory but not change it: `memory_search` is among
the tools they get, so what one of them learns comes back in its result for
the main agent to judge before anything is saved.

Memories are kept as JSON lines, one memory to a line, under
`oh-my-crab/memory` in the user's data folder, or in the folder
`OMC_MEMORY_DIR` names: a project's in `projects/<name>-<hash>/memories.jsonl`,
beside a `project.json` saying which project it is for, and the user's in
`user/memories.jsonl`. They can be read and edited by hand; a line that is not
a memory is left as it is. Every change holds a lock on `memories.lock` beside
the file, reads it again and replaces it whole through a temporary file, so
conversations writing at once never lose each other's memories and a reader
never sees half a file. On Unix the folders and files are made readable by
the user alone.

The `memory` setting (`on` by default, or `off`) is offered in each
conversation; off, nothing is loaded and the agent has none of the three
tools. Turning it either way changes the tool list from the next turn on.
Set `OMC_MEMORY=0` to turn memory off for every conversation of the process;
the setting is then not offered. `crab memory [folder]` prints the memories a
conversation in the folder, or in the current one, would start with, as JSON:
the project's name and folder, where its memories and the user's are kept,
whether memory is on, and every memory with its scope.
