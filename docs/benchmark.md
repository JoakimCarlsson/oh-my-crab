# Edit benchmark

`crab-bench` measures how well models edit files through crab. It gives a model
a fixed set of small editing tasks, each in a fresh copy of the task's files,
and compares what the model leaves with what the task asks for. Run it before
and after a change to a tool, its description or the system prompt to see
whether edits land more or less often, and with how many calls.

It starts a real `crab serve` for every run and talks to it over the
[wire protocol](wire-protocol.md), so it measures the same code a client uses.
It needs a signed-in provider, and every run spends real tokens.

The same tasks can be given to other harnesses as [baselines](#baselines), so
that crab's numbers stand next to theirs in one table.

## Running it

```sh
make bench ARGS="--model claude/claude-haiku-4-5-20251001 --claude-code claude-haiku-4-5-20251001"
```

This builds `crab` and `crab-bench` and runs every task three times with crab
and three times with Claude Code, on the same model. To run the built program
yourself:

```sh
crab-bench --model claude --model openrouter/vendor/model --runs 5
crab-bench --model claude --task tabs --task unicode --set effort=high
crab-bench --list
```

| Option                       | What it does                                                                 | Default                              |
| ---------------------------- | ---------------------------------------------------------------------------- | ------------------------------------ |
| `--model <provider[/model]>` | Measure crab with this model.                                                | none                                 |
| `--claude-code <model>`      | Measure Claude Code with this model, as a baseline.                          | none                                 |
| `--command <name>=<command>` | Measure another program, as a baseline.                                      | none                                 |
| `--runs <n>`                 | How many times each task is run for each of those.                           | `3`                                  |
| `--jobs <n>`                 | How many runs go on at once.                                                 | `4`                                  |
| `--task <name>`              | Run only this task; repeat it for several.                                   | every task                           |
| `--tasks <folder>`           | The folder the tasks are read from.                                          | `crates/omc-bench/tasks`             |
| `--out <folder>`             | Where the results are written.                                               | `build/bench/<time>`                 |
| `--crab <program>`           | The crab to measure.                                                         | the one beside `crab-bench`, else `PATH` |
| `--timeout <seconds>`        | How long one run may take.                                                   | `300`                                |
| `--tools <a,b,c>`            | The tools the model is given, by crab's names; every other tool is denied.   | `read,grep,glob,edit`                |
| `--set <setting>=<value>`    | A conversation setting for every crab run, such as `effort=high`.            | none                                 |
| `--list`                     | Print the tasks and stop.                                                    |                                      |

At least one of `--model`, `--claude-code` and `--command` is needed, and each
can be repeated. The report lists them in the order they were named.

## Baselines

A baseline gets the same prompt in the same fresh copy of the files, and what
it leaves is scored the same way. Use the same model for crab and the baseline,
or the difference is the model's and not the harness's.

- `--claude-code <model>` runs Claude Code (`claude` on `PATH`) without the
  user's settings, hooks or tool servers, without asking before a tool runs,
  and with the tools of `--tools` it has a counterpart of: `read`, `grep`,
  `glob`, `edit`, `write` and `bash`. An `effort` given with `--set` is passed
  on to it.
- `--command <name>=<command>` runs any other program. The command is split at
  spaces, and the word `{prompt}` is replaced by what the model is asked. The
  program is started in the run's folder with nothing on its input and must
  end when it is done; ending unsuccessfully fails the run. Which model and
  tools it uses is up to the command.

  ```sh
  crab-bench --model claude --command "other=some-agent --print --json {prompt}"
  ```

  Its tool calls are counted when it prints JSON lines of these kinds, named by
  their `type`: `tool_execution_start` with a `toolCallId` and a `toolName`,
  `tool_execution_end` with a `toolCallId` and `isError`, and `message_end`
  with the `usage` of its `message` as `input`, `cacheRead` and `cacheWrite`.
  A program that prints nothing of the kind is still scored on its files and
  its time, with no calls counted.

The numbers compare whole harnesses, not only their edit tools: each brings its
own system prompt and its own `read` and search tools.

## What a run is

- The task's `before` files are copied to a fresh folder, which is made a git
  repository of its own so that the project crab sees ends there.
- The conversation is opened in yolo mode, is not saved, and gets only the
  tools `--tools` names. With the default tools the model cannot fall back on
  `write` or `bash`, so the result says how `edit` did.
- The user's own instruction files, skills, agents, hooks and permission rules
  are left out, and memory, tool servers and extensions are off, so two
  machines measure the same thing.
- No language server runs: files are not reformatted after an edit and no
  diagnostics are reported.
- The run ends when the turn does, or after `--timeout`.

The points about tools, the user's own files and language servers describe
crab's runs; a baseline started with `--command` is as isolated as its command
makes it.

## How it is scored

A run **passed** when every file is what the task's `after` folder holds and no
file was added or removed. Files are compared line by line; white space at the
end of a line and blank lines are ignored, everything else counts.

A run is **clean** when it passed and no edit call failed on the way. A run
that passes only after a rejected edit still costs a round trip and tokens,
which is what the difference between the two columns shows.

The report has one row for each harness and model:

| Column              | What it says                                                              |
| ------------------- | ------------------------------------------------------------------------- |
| `passed`            | The share of runs that left the right files.                              |
| `clean`             | The share that did so without a failed edit call.                         |
| `requests / run`    | How many times the model was asked: one round trip each.                  |
| `tool calls / run`  | How many tool calls a run took, of any tool.                              |
| `failed edit calls` | The share of the calls of `edit`, `write` and the like that did not work. |
| `tokens sent / run` | The tokens sent to the model, added up over a run's requests.             |
| `seconds / run`     | How long a run took.                                                      |
| `errors`            | Runs that ended badly: the turn failed, timed out or the harness stopped. |

`tokens sent` counts what each request carried, cached or not, so it shows how
much a harness makes the model read. It leaves out what the model wrote, which
crab does not report over the wire.

A second table shows, for each task, how many runs of each harness passed, and
the requests, tokens sent and seconds a run of it took.

The output folder holds `results.json`, with every run's outcome and its calls
by tool, and for each run the files the model left (`work/`) and every event
crab sent or line a baseline printed (`events.jsonl`), under
`runs/<name>/<task>/<run>/`.

## Results

Measured on 10 November 2026 on Linux, with crab 0.1.1, Claude Code 2.1.319
and oh-my-pi 18.16.0, all signed in to the same Claude account. Every task was
run three times for each row, with the tools `read`, `grep`, `glob` and `edit`
and medium effort:

```sh
OMP="-p --mode json --no-session --thinking medium --tools read,grep,glob,edit \
  --no-lsp --no-extensions --no-skills --no-rules --no-context-files --no-title \
  --approval-mode yolo {prompt}"
crab-bench --runs 3 --jobs 8 --timeout 900 --set effort=medium \
  --model claude/claude-sonnet-5-5 --claude-code claude-sonnet-5-5 \
  --command "oh-my-pi sonnet-5-5=omp --model anthropic/claude-sonnet-5-5 $OMP" \
  --model claude/claude-fable-5-2 --claude-code claude-fable-5-2 \
  --command "oh-my-pi fable-5-2=omp --model anthropic/claude-fable-5-2 $OMP"
```

All 20 tasks, 60 runs a row:

| Model      | Harness     | Passed | Clean | Requests / run | Tool calls / run | Tokens sent / run | Seconds / run |
| ---------- | ----------- | ------ | ----- | -------------- | ---------------- | ----------------- | ------------- |
| Sonnet 5.5 | crab        | 100%   | 100%  | 3.4            | 3.4              | 28.9k             | 8             |
| Sonnet 5.5 | Claude Code | 100%   | 97%   | 3.6            | 8.1              | 31.3k             | 15            |
| Sonnet 5.5 | oh-my-pi    | 100%   | 95%   | 3.4            | 3.4              | 29.6k             | 9             |
| Fable 5.2  | crab        | 100%   | 95%   | 3.3            | 3.5              | 27.5k             | 16            |
| Fable 5.2  | Claude Code | 97%    | 95%   | 3.6            | 8.6              | 33.3k             | 26            |
| Fable 5.2  | oh-my-pi    | 100%   | 95%   | 3.6            | 3.9              | 41.5k             | 20            |

The `real-` tasks, as tokens sent and seconds a run, three runs a cell; every
cell passed three of three unless it says otherwise.

With Sonnet 5.5:

| Task                     | crab        | Claude Code | oh-my-pi    |
| ------------------------ | ----------- | ----------- | ----------- |
| `real-add-attributes`    | 31.2k · 9s  | 28.2k · 20s | 25.4k · 8s  |
| `real-catalog-values`    | 43.8k · 16s | 94.0k · 29s | 43.4k · 11s |
| `real-deep-edit`         | 23.4k · 5s  | 26.0k · 9s  | 22.2k · 7s  |
| `real-delete-tests`      | 31.3k · 9s  | 73.7k · 53s | 67.3k · 17s |
| `real-header-every-file` | 53.8k · 16s | 35.2k · 32s | 38.0k · 16s |
| `real-move-function`     | 33.9k · 8s  | 29.0k · 13s | 30.2k · 11s |
| `real-rename-method`     | 26.1k · 12s | 27.6k · 10s | 28.0k · 14s |
| `real-rename-type`       | 43.4k · 17s | 70.2k · 45s | 81.3k · 22s |

With Fable 5.2:

| Task                     | crab        | Claude Code          | oh-my-pi     |
| ------------------------ | ----------- | -------------------- | ------------ |
| `real-add-attributes`    | 27.1k · 16s | 38.4k · 39s (2 of 3) | 25.5k · 17s  |
| `real-catalog-values`    | 39.2k · 27s | 48.5k · 46s          | 39.0k · 29s  |
| `real-deep-edit`         | 23.4k · 14s | 28.5k · 22s          | 22.2k · 13s  |
| `real-delete-tests`      | 35.2k · 19s | 52.8k · 75s          | 64.7k · 38s  |
| `real-header-every-file` | 45.0k · 35s | 37.9k · 51s          | 258.7k · 40s |
| `real-move-function`     | 43.0k · 21s | 32.3k · 24s          | 30.6k · 18s  |
| `real-rename-method`     | 26.5k · 22s | 98.1k · 21s          | 28.9k · 26s  |
| `real-rename-type`       | 37.4k · 34s | 73.4k · 76s (2 of 3) | 70.1k · 41s  |

What the runs showed:

- Crab and oh-my-pi passed every run. Claude Code with Fable 5.2 failed two:
  in both it dropped a space from lines it rewrote, such as `history:&mut` for
  `history: &mut`.
- Crab and oh-my-pi are close overall, within a second and a few percent of
  tokens with Sonnet 5.5; both are faster than Claude Code with either model.
- Crab is clearly ahead where a change is large or spread out. Deleting 569
  lines of tests from four files takes it half the time and tokens of
  oh-my-pi, whose first edit call was refused in every run of that task, and a
  quarter of the time of Claude Code or less. Renaming a type in 46 places takes it
  about half the tokens of either.
- Crab does worst on `real-header-every-file`. With Fable 5.2 the first edit
  call was refused in all three runs, because it asked to insert at line 0,
  and with Sonnet 5.5 the task took more tokens than with either baseline.
- oh-my-pi is ahead on `real-catalog-values` with Sonnet 5.5 and on
  `real-add-attributes`, and takes one request fewer than crab on a few small
  tasks. Its Fable 5.2 average is pulled up by one task, 258.7k tokens on
  `real-header-every-file`.
- On the small tasks crab sends about 3,000 more tokens a run than Claude
  Code.

Three runs a cell show the large differences and not the small ones, and
`tokens sent` leaves out what the model wrote. Run the command above to
measure again after a change.

## Tasks

A task is a folder under `crates/omc-bench/tasks`:

```
<name>/
  prompt.md      what the model is asked
  before/        the files it starts from
  after/         the files it should leave
```

A task on a large body of code shares it with others instead of carrying a
copy: a `fixture` file names a folder under `crates/omc-bench/fixtures` to
start from in place of `before/`, and `change.patch` gives the change to make,
as a patch `git apply` takes, in place of `after/`.

Prompts say exactly what to change, so that a failed run is a failed edit and
not a different opinion about the code.

The small tasks each stress one thing an edit can get wrong. Current models
pass nearly all of them with any harness, so they show little between
harnesses; they catch a regression in the edit tool itself.

| Task             | What it stresses                                                    |
| ---------------- | ------------------------------------------------------------------- |
| `single-line`    | One changed line among near-identical ones.                         |
| `long-file`      | A file long enough to be read as an outline first.                  |
| `scattered`      | Three of four identical lines changed in one file.                  |
| `multi-file`     | A rename across three files, with a look-alike name to leave alone. |
| `insert-block`   | A new block between two existing ones.                              |
| `delete-block`   | A block removed, and a second small removal further down.           |
| `repeated-lines` | One of four identical blocks changed.                               |
| `tabs`           | Tab-indented lines.                                                 |
| `unicode`        | Text outside ASCII, including emoji and CJK.                        |
| `shifting-lines` | An insertion, a deletion and a change whose line numbers shift.     |
| `rewrite-body`   | A whole function body replaced by one of another length.            |
| `nested-indent`  | A block added at the right depth of deep nesting.                   |

The `real-` tasks work on real code: `agent`, fifteen Rust files and about
11,000 lines, and `catalog`, a 170 KB JSON file with one model a line. Every
harness is expected to get them right; what differs is how many requests,
tokens and seconds it takes.

| Task                     | What it asks                                                             |
| ------------------------ | ------------------------------------------------------------------------ |
| `real-rename-type`       | Rename a type in 46 places across 10 files, beside a look-alike name.     |
| `real-rename-method`     | Rename a method in 29 places of a 1,900-line file.                        |
| `real-add-attributes`    | Add a line above twelve named functions in six files.                     |
| `real-delete-tests`      | Delete the test module from four files, 569 lines in all.                 |
| `real-header-every-file` | Add a first line to all fifteen files.                                    |
| `real-move-function`     | Move a function and its doc comment from one file to another.             |
| `real-deep-edit`         | Change a line at the top and four lines deep in a 2,100-line file.        |
| `real-catalog-values`    | Change eight values in a large JSON file where other lines look the same. |

To add a task, add a folder in one of those shapes. A test fails when a task
adds or removes files, changes nothing, or has a patch that does not apply.
