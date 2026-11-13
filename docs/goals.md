# Goals

A goal is an objective the agent keeps working toward across turns, so that a
long job such as "make every test in the workspace pass" does not stop after
each turn to wait for "continue". The reader sets it with `/goal`; crab then
starts each next turn itself until the agent says the objective is achieved,
the turns stop making progress, the token budget runs out, or the reader stops
it.

## Setting and changing a goal

Every form is sent as a `Prompt` whose text starts with `/goal`:

| Text                         | What it does                                                                                     |
| ---------------------------- | ------------------------------------------------------------------------------------------------ |
| `/goal <objective>`          | Sets the goal, in place of any there was, and starts a turn toward it.                           |
| `/goal 500k <objective>`     | The same, with a budget of 500,000 tokens. A budget is digits with `k` or `m` after them, or a bare number of at least 1000. |
| `/goal`                      | Shows the goal.                                                                                  |
| `/goal pause`                | Pauses it: the turn running finishes, and no turn follows it.                                    |
| `/goal resume`               | Makes a paused goal active again and starts a turn toward it.                                    |
| `/goal clear`                | Forgets the goal.                                                                                |
| `/goal budget <tokens\|off>` | Changes the budget. A goal that had run out and now has some left is paused, for `/goal resume`. |

Each sends a [`Goal`](wire-protocol.md#goal) event when the goal changed or
is shown. Setting and resuming start a turn under the reader's prompt. The
other forms start no turn: between turns they are answered as a short turn of
their own, a few words on the goal or on why the command cannot be done, then
`Finished`; while a turn runs they take effect at once, and one that cannot be
done is answered with `Failed`. Setting and resuming are refused while a turn
runs; pausing, clearing, showing and budgeting are not. `/goal` takes no
files. Any other
prompt, such as `/goal pause the build`, sets that text as the objective.

The goal is saved with the conversation. When a saved conversation with an
active goal is opened again, the goal comes back paused, so that nothing runs
until the reader says `/goal resume`.

## Working out a goal with the agent

`/guided-goal`, optionally followed by a rough idea, has the agent work the
goal out with the reader before anything is built. In a turn under the reader's
prompt, without changing files, the agent pins down five things, asking with
the `ask` tool, at most six questions with an answer it recommends for each:

1. what done means, as checks that pass or fail;
2. the command or steps that verify it;
3. how many attempts, or how much effort, is reasonable before stopping;
4. what is in scope and what must not be touched;
5. when to stop and ask instead of going on.

It then sets the goal itself with the `goal` tool's `create`, laid out under
the headings Objective, Success criteria, Verification, Boundaries and Stop
conditions, with a budget only when the reader gave one, and ends the turn;
crab starts the work from the next turn as for `/goal`. A goal that was active
pauses while this happens. Only after `/guided-goal` may the agent create a
goal; it is refused otherwise. Like setting, `/guided-goal` is refused while a
turn runs.

The verification the agent writes down is part of the objective: the agent is
held to it by its prompts, and crab does not run it.

## How crab works toward it

- **The first turn** is started with a prompt that states the objective and
  asks the agent to say what done means and how it will check it.
- **Every turn** while the goal is active starts with a short reminder of the
  objective and how much of the budget is used, before the prompt.
- **After a turn ends** with no prompt of the reader's waiting, crab starts
  the next turn itself and tells the client with a
  [`Continued`](wire-protocol.md#continued) event carrying the prompt. That
  prompt asks the agent to audit where the objective stands with real
  evidence, list what is missing, take the next step, and end the turn without
  calling tools when only the reader can unblock it. The prompt is saved as the
  turn's, so the conversation reads back as turns like any other.
- A prompt the reader sends while a turn runs waits for it and starts the next
  turn instead, as any queued prompt does; crab goes on after that turn.

## How it ends

- **Complete.** Only the agent ends the goal as achieved, by calling the
  [`goal`](tools/README.md#conversation-control) tool with `complete` and a
  summary of its evidence. The goal becomes `Complete` with the summary as its
  note, and no further turn starts.
- **No progress.** crab digests the tool calls each turn made, with their
  arguments and results. When a turn crab started made no tool calls, or the
  same calls with the same results as the turn crab started before it, the goal
  pauses with a note saying so. A prompt from the reader starts the judging
  afresh.
- **Budget.** The budget counts the input, output and cache writes of every
  request while the goal is active, subagents and summaries included; cache
  reads, and the requests that keep the cache warm between turns, are not
  counted. When it is used up the goal becomes `BudgetLimited` and the agent is
  told, once, to start no new work, leave things coherent and report: at the
  next safe point of the turn running, or in one last turn crab starts for it.
  No turn follows that.
- **Stopped or failed.** Cancelling a turn, a turn that fails, and a turn that
  needs signing in again pause the goal.

## The `goal` tool

The main agent has the `goal` tool in every conversation, so that the tool
list never changes when a goal is set. `status` reads the objective, its state
and its budget; `create` sets the goal worked out after `/guided-goal`, and is
refused otherwise; `complete` with `summary` ends it as achieved, and is refused
when no goal is being worked toward. Subagents and scripts do not get it.
