# Long conversations

When a conversation's context nears the model's window, `crab` first leaves
old tool outputs out of what the model is sent: every output of at least 50
tokens outside the most recent 40,000 tokens of tool output, as long as that
saves at least 20,000 tokens. Each is replaced with a short note. A read made
again with the same arguments makes the earlier copy out of date, and that copy
is left out as soon as doing so costs little of the provider's prompt cache.
When leaving outputs out is not enough, `crab` shakes the conversation, still
without asking the model anything: outside the most recent messages it would
keep in a summary, every tool output of at least 100 tokens and every fenced
block of at least 400 tokens in an earlier prompt or reply is replaced with a
note such as `[120 lines of bash output removed to save room. Call recall with
call_id "c1" to read it again.]`, as long as that saves at least 4,000 tokens.
Outputs of `skill`, `context_notes` and `todo` are kept. A shake is saved like
the leaving out of outputs, so a reopened conversation gives the model the same
thing. When the context is still over the threshold, `crab` compacts the
conversation before the next request. It asks the model for a structured summary of the
older messages, building on the previous summary when there is one, and keeps
about 20,000 tokens of the most recent messages as they are. The threshold is
the window less 15% of it; on a window over 65,536 tokens it is at least 16,384
tokens below the window. When a provider still refuses a request as too long,
`crab` compacts and sends the request again, once a turn. Sending `/compact`,
optionally followed by what the summary should keep in particular, compacts on
demand; it takes no files. `PreCompact` [hooks](extending/hooks.md) run before
every summary, and may block it or add to what it should keep.

A compaction deletes nothing. It is recorded at the point it happened, the
summary goes to the model in place of the older messages, and the whole
conversation stays saved and is still shown when it is opened again, with the
compaction as a finished `compact` step holding the summary. When the todo
list the model last wrote is hidden by a compaction and still has open items,
they are listed again after the summary; a list the model wrote after that,
which it still sees, takes its place. A `/compact` the
reader sent comes back as a turn of its own, and one that failed comes back
with its reason.

Nothing left out or summarized is lost to the model either. Once a
conversation has been pruned or compacted, the model can call `recall` to read
the whole history again: the full output of one tool call, the lines that
contain some text, or a range of messages by number. `recall` is on the tool
list from the first turn, so the list does not change and cost the prompt
cache when it becomes useful; called before anything was left out, it says
there is nothing to read yet.

## Prompt cache

Providers cache the start of a request, tools then system prompt then
messages, only while it stays byte for byte the same. `crab` therefore makes
the system prompt and the tool list once per conversation, when its first
turn starts, and sends them unchanged after that; they are made again only
when the model changes or the checkpoint tools or memory are turned on or
off. What changes later reaches the agent as a short notice at the start of
the next prompt instead:

- today's date and the git branch, on the first prompt, again when either
  changes, and again after a compaction or a fresh context window has dropped
  the earlier notice;
- plan mode turning on, with its rules and the tools it allows, and turning
  off;
- skills, subagent kinds and instruction files that were added, changed or
  removed on disk since the system prompt was written; the notice only points
  at tools the agent is offered;
- the digest of the memories earlier conversations saved, on the first prompt
  and again after a compaction or a fresh context window has dropped it.
- what the `SessionStart` [hooks](extending/hooks.md) said when the
  conversation opened or resumed, on the first prompt, and what they say after
  each compaction, on the prompt after it.

The `skill` tool is on the list even when no skill exists yet, so that a skill
added later can be loaded without changing the list; until then a call to it
says there are no skills.

The system prompt carries an outline of the working directory, so the agent
need not list folders before it starts: three levels deep, folders first and
by name, at most 25 entries for the working directory itself and 15 for any
folder below it, with a line counting the rest. Entries that `.gitignore`,
`.ignore` or git's exclude files leave out are not shown, in a repository or
not, nor are hidden entries or the folders of version control, builds and
vendored packages (`.git`, `target`, `build`, `dist`, `node_modules`,
`vendor`, `.venv` and the like). When the outline would pass 120 lines, the
deepest folders are left closed, a level at a time from the top, so the upper
levels are always whole. A walk that finds more than 20,000 entries is tried
again a level shallower, and one that takes longer than 300 milliseconds, on
a network mount say, gives no outline at all; the walk runs on a thread of its
own, so it never holds up the rest of `crab`. The outline is made once, when
the conversation's first turn starts, and kept unchanged for the rest of it,
also when the system prompt is made again for another model; the prompt says
it is a snapshot from the start of the conversation. Subagents get the same
outline in their own system prompts.

Rarely used tools are kept off the list without changing it: they are
[hidden until looked up](tools/README.md#hidden-tools). `load_tools`, which
names each hidden tool with a one-line summary, is on the list from the first
request, and loading a tool only adds a tool result to the conversation, so
the list and everything before the new messages stay cached.

Plan mode no longer takes tools off the list. The tools that change things
stay listed, and a call to one while plan mode is on is refused with a
message naming the tools that are allowed; it is never run and never asks for
approval. Outside plan mode `exit_plan_mode` is refused the same way.

The plan the agent submits is kept in its context through all of this: an
`exit_plan_mode` output is never pruned or shaken out, and a compaction that
summarizes the part holding the latest plan restates it whole after the
summary. A plan approved with `fresh` starts a fresh context window seeded
with the plan, which is never shaken out either. See [plans](concepts.md#plans).

Two settings shape the cache on Claude:

- `cache` (`5m` or `1h`) is how long cached context lives after each request.
  An hour costs twice the input price to write instead of a quarter more, and
  pays off when replies come more than five minutes apart.
- `cache_warming` (`on` by default, or `off`). Shortly before cached context
  would expire, at nine tenths of its life, `crab` sends the last request again
  asking for no output, which keeps the cache alive at the price of reading it.
  It does so only when the expected saving, the chance of another request
  before expiry times the extra cost of a miss, less the cost of the refresh,
  is at least $0.02: the chance is taken as certain while a turn runs (a long
  tool call) and as 15% while the conversation waits for the reader. Warming
  stops an hour after the last real request while a turn runs and half an hour
  after it while idle; when the model, the effort, the checkpoint setting or a
  cache setting changes; when the turn is cancelled or fails, before a
  compaction the reader asked for, and when another conversation is opened;
  and for models that think within a fixed token budget, whose requests cannot
  be replayed without output. Nothing is warmed while a request is on its way,
  since that request reads the cache itself. What warming cost is saved with
  the conversation under `cache_warming`.

Prices per model and cache lifetimes are in `crates/omc-providers/src/models.rs`.
The requests that summarize a conversation for a compaction write nothing to
the cache, since they are never sent again.

## What a conversation costs

Every model request is priced as it is answered and added up for each model:
the conversation's model, the plan model, the models of subagents, summaries
and cache warming. The client is told the total with a
[`Spent`](wire-protocol.md#spent) event after each request, and it is saved
with the conversation under `spent`, so a conversation opened again goes on
counting from where it was. Going back to before a turn does not take back
what the turns left behind cost, and a fork starts at nothing.

The prices are list prices, so a conversation paid for by a subscription shows
what it would have cost with an API key. A model whose prices are not known is
counted in tokens only.

The total also says how much was paid for cache misses. A request of the main
loop that reads from the cache less of the conversation than the request
before it sent, by more than 2,048 tokens, missed: the cached context expired
while the conversation waited, or a compaction rewrote it. What the part that
was not read cost beyond reading it from the cache counts as the miss. Requests
of subagents and of the plan model are not watched for misses.
