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
  spaces, and the word `{prompt}` is replaced by what the model is asked;
  `{tools}`, wherever it stands, is replaced by the task's tools, by crab's
  names with commas between them. The program is started in the run's folder with nothing on its input and must
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

- The task's `before` files are copied to a fresh folder and committed in a
  git repository of its own, so that the project crab sees ends there and
  starts clean.
- The conversation is opened in yolo mode, is not saved, and gets only the
  tools `--tools` names, or the ones the task names in its `tools` file. With
  the default tools the model cannot fall back on
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
end of a line and blank lines are ignored, everything else counts. A task with
a `check` command passes when that command ends successfully, in place of the
comparison or on top of it.

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
| `tokens written / run` | The tokens the model wrote, reasoning included.                        |
| `seconds / run`     | How long a run took.                                                      |
| `errors`            | Runs that ended badly: the turn failed, timed out or the harness stopped. |

`tokens sent` counts what each request carried, cached or not, so it shows how
much a harness makes the model read. `tokens written` is what the model wrote
in reply; a crab older than the `written` field of the wire's `Used` event
reports 0.

A second table shows, for each task, how many runs of each harness passed, and
the requests, tokens sent and seconds a run of it took.

The output folder holds `results.json`, with every run's outcome and its calls
by tool, and for each run the files the model left (`work/`) and every event
crab sent or line a baseline printed (`events.jsonl`), under
`runs/<name>/<task>/<run>/`.

## Results

Measured on 10 November 2026 on Linux, with crab at commit `2401b05` (after
0.1.1), Claude Code 2.1.319 and oh-my-pi 18.16.0, all signed in to the same
Claude account. Every task was run three times for each row, with medium
effort and the tools `read`, `grep`, `glob` and `edit`, plus `bash` for the
`cmd-` tasks and `write` for `cmd-slow-command`:

```sh
OMP="-p --mode json --no-session --thinking medium --tools {tools} \
  --no-lsp --no-extensions --no-skills --no-rules --no-context-files --no-title \
  --approval-mode yolo {prompt}"
crab-bench --runs 3 --jobs 8 --timeout 900 --set effort=medium \
  --model claude/claude-sonnet-5-5 --claude-code claude-sonnet-5-5 \
  --command "oh-my-pi sonnet-5-5=omp --model anthropic/claude-sonnet-5-5 $OMP" \
  --model claude/claude-fable-5-2 --claude-code claude-fable-5-2 \
  --command "oh-my-pi fable-5-2=omp --model anthropic/claude-fable-5-2 $OMP"
```

All 24 tasks, 72 runs a row:

| Model      | Harness     | Passed | Clean | Requests / run | Tool calls / run | Tokens sent / run | Tokens written / run | Seconds / run |
| ---------- | ----------- | ------ | ----- | -------------- | ---------------- | ----------------- | -------------------- | ------------- |
| Sonnet 5.5 | crab        | 100%   | 99%   | 3.4            | 2.8              | 28.4k             | 740                  | 13            |
| Sonnet 5.5 | Claude Code | 100%   | 96%   | 3.8            | 7.3              | 30.6k             | 1924                 | 19            |
| Sonnet 5.5 | oh-my-pi    | 96%    | 86%   | 3.5            | 3.7              | 33.3k             | 766                  | 11            |
| Fable 5.2  | crab        | 100%   | 100%  | 3.3            | 3.1              | 27.7k             | 804                  | 21            |
| Fable 5.2  | Claude Code | 100%   | 100%  | 3.6            | 7.6              | 32.5k             | 2003                 | 30            |
| Fable 5.2  | oh-my-pi    | 96%    | 92%   | 3.6            | 3.9              | 36.3k             | 874                  | 22            |

`cmd-slow-command` waits 130 seconds by design, and oh-my-pi's runs of it
ended after about 67, so it bends the seconds. Without that task, a run takes
7, 14 and 9 seconds with Sonnet 5.5 for crab, Claude Code and oh-my-pi, and
15, 25 and 20 seconds with Fable 5.2.

The `real-` and `cmd-` tasks, as tokens sent and seconds a run, three runs a
cell; every cell passed three of three unless it says otherwise.

With Sonnet 5.5:

| Task                     | crab         | Claude Code  | oh-my-pi             |
| ------------------------ | ------------ | ------------ | -------------------- |
| `real-add-attributes`    | 27.0k · 9s   | 28.9k · 21s  | 25.4k · 8s           |
| `real-catalog-values`    | 41.0k · 15s  | 76.2k · 34s  | 42.6k · 12s          |
| `real-deep-edit`         | 23.3k · 6s   | 26.0k · 9s   | 22.2k · 6s           |
| `real-delete-tests`      | 34.3k · 9s   | 55.6k · 49s  | 69.3k · 17s          |
| `real-header-every-file` | 23.7k · 8s   | 35.8k · 31s  | 112.6k · 18s         |
| `real-move-function`     | 40.7k · 10s  | 33.0k · 12s  | 30.8k · 9s           |
| `real-rename-method`     | 25.9k · 13s  | 30.0k · 10s  | 28.1k · 12s          |
| `real-rename-type`       | 36.8k · 16s  | 77.7k · 44s  | 71.0k · 20s          |
| `cmd-fix-failing-test`   | 26.1k · 4s   | 24.3k · 5s   | 30.5k · 7s           |
| `cmd-lint-loop`          | 44.2k · 10s  | 32.4k · 10s  | 52.6k · 14s          |
| `cmd-slow-command`       | 25.7k · 135s | 23.5k · 135s | 23.9k · 67s (0 of 3) |
| `cmd-stale-file`         | 42.3k · 6s   | 37.9k · 10s  | 36.8k · 9s           |

With Fable 5.2:

| Task                     | crab         | Claude Code  | oh-my-pi             |
| ------------------------ | ------------ | ------------ | -------------------- |
| `real-add-attributes`    | 27.0k · 16s  | 39.9k · 36s  | 25.6k · 15s          |
| `real-catalog-values`    | 45.7k · 26s  | 48.4k · 43s  | 43.7k · 27s          |
| `real-deep-edit`         | 23.3k · 11s  | 28.5k · 22s  | 22.2k · 15s          |
| `real-delete-tests`      | 35.1k · 21s  | 53.9k · 81s  | 77.2k · 48s          |
| `real-header-every-file` | 23.9k · 14s  | 37.8k · 54s  | 195.0k · 39s         |
| `real-move-function`     | 42.5k · 22s  | 32.4k · 26s  | 30.8k · 22s          |
| `real-rename-method`     | 26.0k · 23s  | 98.8k · 20s  | 44.1k · 27s          |
| `real-rename-type`       | 37.4k · 34s  | 65.9k · 69s  | 35.8k · 39s          |
| `cmd-fix-failing-test`   | 28.6k · 9s   | 24.9k · 11s  | 24.3k · 14s          |
| `cmd-lint-loop`          | 28.2k · 19s  | 36.6k · 25s  | 36.5k · 31s          |
| `cmd-slow-command`       | 25.7k · 142s | 24.1k · 141s | 15.6k · 70s (0 of 3) |
| `cmd-stale-file`         | 39.3k · 19s  | 33.4k · 19s  | 39.4k · 20s          |

What the runs showed:

- Crab and Claude Code passed every run. oh-my-pi failed all six runs of
  `cmd-slow-command`: it moved the command to the background after a minute
  and, run without a reader as here, ended the run before the command
  finished.
- Crab sends the fewest tokens and makes the fewest tool calls with both
  models, and without the slow command it is the fastest with both.
- Crab is clearly ahead where a change is large or spread out: deleting 569
  lines of tests from four files, renaming a type in 46 places with
  Sonnet 5.5, and adding a first line to all fifteen files, which it does
  without reading them.
- The model writes about two and a half times as much with Claude Code as
  with the other two.
- Crab is behind on `real-move-function`, where it takes a request and about
  10,000 tokens more than either baseline, on `insert-block` and, with
  Sonnet 5.5, on `cmd-lint-loop` against Claude Code.
- On the small tasks crab sends about 3,000 more tokens a run than Claude
  Code.

Three runs a cell show the large differences and not the small ones. Run the
command above to measure again after a change.

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

A task that needs more than the usual tools names them in a `tools` file,
separated by commas or lines. A task whose result cannot be given as files has
a `check` file instead of `after/` or `change.patch`, or beside them: one
command, run by `bash` in the folder the model worked in once the run is over,
that ends successfully when the model did what was asked. The folder is a git
repository with the starting files committed, so the command can use `git diff`
to see that a file was left alone.

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

The `cmd-` tasks need commands to be run, so they give the model `bash` as
well, and are scored by a check command. They need `python3`.

| Task                   | What it asks                                                                    |
| ---------------------- | ------------------------------------------------------------------------------- |
| `cmd-fix-failing-test` | Run the tests, find the bug behind the one that fails, and fix it.               |
| `cmd-lint-loop`        | Run a checker and fix the seven problems it lists in three files, until it passes. |
| `cmd-slow-command`     | Run a command that takes 130 seconds and write down what it prints at the end.   |
| `cmd-stale-file`       | Read a file, run a script that rewrites it, then edit the rewritten file.        |

To add a task, add a folder in one of those shapes. A test fails when a task
scored on files adds or removes files, changes nothing, or has a patch that
does not apply.
