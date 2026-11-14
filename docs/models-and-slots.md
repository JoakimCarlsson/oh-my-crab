# Models and slots

A conversation asks more than one model. Its own model answers the prompts,
and other work can go to other models: summaries to a fast, cheap one, plans
to a strong one. Crab names each of these jobs a **slot**, and each slot holds
the model it asks, with backups for when that model keeps failing.

## The slots

| Slot       | What asks it                                                                                                        |
| ---------- | ------------------------------------------------------------------------------------------------------------------- |
| `main`     | The turns of the conversation. It is always the conversation's model, the `model` setting.                          |
| `fast`     | Compaction summaries, questions on the side (`/btw`) and the prompt `/handoff` writes.                              |
| `plan`     | Turns in plan mode, unless the `plan_model` setting names another model.                                            |
| `subagent` | Subagents started by `task` and `agent_spawn` whose call and kind name no model.                                     |
| `review`   | Reserved for reviews, such as judging a goal; crab asks no model for one yet.                                       |
| `vision`   | Reserved for describing images to a model that cannot read them; crab does not do that yet.                          |

A slot holds an assignment for each provider, such as `claude` or `codex`,
and one for any provider. A conversation uses the assignment of its own
provider when a file sets one, else the any-provider assignment, so a Claude
conversation and a Codex one can each have their own `fast` model.

A slot that sets no model resolves to `main`, the conversation's model, so a
conversation with no slots set behaves as if slots did not exist. Crab does
not name conversations with a model, so no slot is used for titles.

Users may add slots of their own, named with letters, digits, `-` and `_`,
and use them anywhere a model is named, such as in an agent definition.

## Selectors

A slot's model and backups are **selectors**: `model` or `provider/model`,
with `:effort` after it to set how hard it thinks, as in `claude/haiku:low`
or `codex/gpt-5:high`. The part after the last `:` is an effort only when it
is one (`low`, `medium`, `high`, `xhigh`, `max`), so ids such as
`ollama/llama3:8b` read as they are. The model is looked up as subagent models
are (see [Subagents](tools/subagents.md#models)): an id of the provider's
list, an alias such as `haiku`, or a model of another provider the reader is
signed in to, after its id and a `/`.

## `@name`

Wherever a model is named, `@name` names the model of the slot `name`:

- the `model` and `plan_model` settings, through `Set` and `Open`'s
  `defaults` in the [wire protocol](wire-protocol.md#settings-round-trips);
- `Open`'s `model`, and so `crab exec --model @fast`;
- an agent definition's front matter, as `model: @fast`
  ([skills and agents](extending/skills-and-agents.md));
- a `task` or `agent_spawn` call's `model`.

A slot that does not exist is an error naming the slots there are. For the
conversation's `model`, the slot's model must be one of the conversation's
provider; `@main` names the model the files set for `main`.

## Where slots are kept

| Layer       | Where                                                                                      | Wins over       |
| ----------- | ------------------------------------------------------------------------------------------ | --------------- |
| Environment | `OMC_MODEL_<SLOT>`, as `OMC_MODEL_FAST=claude/haiku`: the any-provider model only          | every file      |
| Project     | `.oh-my-crab/slots.json` in the working folder, read once the project is trusted            | the user's file |
| User        | `<config>/slots.json`, or the file `OMC_SLOTS_FILE` names                                  | nothing         |

The project wins assignment by assignment: an assignment the project's file
sets takes its model and backups from there, and the others come from the
user's file. Resolution then goes, for the conversation's provider: that
provider's assignment, then the any-provider assignment (with the
environment's model over it), then `main`. The backups are those of the
assignment that applies. The files
are read again for every turn, so a change applies from the next request.

```json
{
  "slots": {
    "fast": {
      "model": "openai/gpt-5-mini",
      "providers": {
        "claude": { "model": "claude/haiku:low", "backups": ["claude/sonnet"] },
        "codex": { "model": "codex/gpt-5-mini" }
      }
    },
    "plan": { "model": "claude/opus:high" },
    "main": { "backups": ["openai/gpt-5"] },
    "docs": { "model": "claude/sonnet" }
  },
  "presets": {
    "cheap": {
      "slots": { "fast": { "model": "claude/haiku" } },
      "effort": "low"
    }
  }
}
```

A slot's own `model` and `backups` are its any-provider assignment, so files
written before slots had providers read as they are. `providers` holds the
assignment of each provider by id.

A `main` model in a file is the model a conversation opened without one
starts with. A client changes the files with the `SetSlot` and `ClearSlot`
commands, giving a `provider` to change one provider's assignment, and reads
every slot, each provider's assignment, the model it resolves to and where
that comes from with `Slots`.

## Backups

When a request fails, crab first sends it again to the same model, with
growing waits, as [troubleshooting](troubleshooting.md) describes. When it
still fails, and the failure is one another model may not meet (the provider
is overloaded, rate limited or failing inside, or the model cannot be had),
the turn moves to the slot's next backup that can be found, tells the client
with a `ModelFallback` event, and asks it for the rest of the turn. The next
turn starts with the slot's own model again. Sign-in failures, refused or
malformed requests and context overflows never move to a backup.

Backups are tried for the conversation's turns, under `main`, or `plan` while
the plan model plans, and for subagents whose model came from `subagent`.

## Presets

A preset is a named snapshot of every slot's assignments, per provider and
for any provider, and of the
default effort. `SavePreset` keeps the slots as they stand now, with the
conversation's effort or one given, in the user's file; `ApplyPreset`
replaces every slot of one file with the preset's and sets the conversation's
effort, and its model when the preset sets `main`; `DeletePreset` forgets it.
A project's file may keep presets too; one of the same name wins.

## Suggestions

For each slot that sets no model, `Slots` may carry a `suggested` model from
the [model catalog](providers/supported.md#model-catalog), never applied by
itself, for the conversation's provider and, under `providers`, for each
provider the reader is signed in to that the slot sets no model for:

- `fast`: the cheapest current model of that provider that takes tool calls,
  other than the conversation's;
- `vision`, when the conversation's model takes no images: the cheapest such
  model that takes images, of that provider first, then of the
  other providers the reader is signed in to.
