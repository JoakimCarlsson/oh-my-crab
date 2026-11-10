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
| `--codex <model>`            | Measure the Codex CLI with this model, as a baseline.                        | none                                 |
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

At least one of `--model`, `--claude-code`, `--codex` and `--command` is needed, and each
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
- `--codex <model>` runs the Codex CLI (`codex` on `PATH`) without asking
  before a command runs or a file changes. It takes no list of tools, so it
  works with its own, a shell and patches, whatever `--tools` says, and it
  reads the user's own Codex settings and instructions, which is where its
  sign-in is. It reports the tokens of a run in all and not its requests, so
  its requests show as a dash. An `effort` given with `--set` is passed on to
  it.
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

A run is **exact** when it passed and every file is the expected one byte for
byte, so a run that passed but is not exact got a blank line or the white
space at the end of a line wrong. A task scored by a command alone is exact
when it passed.

A run is **clean** when it passed and no edit call failed on the way. A run
that passes only after a rejected edit still costs a round trip and tokens,
which is what the difference between the two columns shows.

The report has one row for each harness and model:

| Column              | What it says                                                              |
| ------------------- | ------------------------------------------------------------------------- |
| `passed`            | The share of runs that left the right files.                              |
| `exact`             | The share that left them byte for byte, blank lines included.             |
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

Measured on 10 November 2026 on Linux, with crab as it stands at the commit
that added these results (after 0.1.1), Claude Code 2.1.319, oh-my-pi 18.16.0
and the Codex CLI 0.166.0, signed in to one Claude account and one ChatGPT
account. Every task was run three times for each row, with medium effort and
the tools `read`, `grep`, `glob` and `edit`, plus `bash` for the `cmd-` tasks
and `write` for `cmd-slow-command`; the Codex CLI works with its own tools.
oh-my-pi was not signed in to OpenAI, so it has no GPT row.

```sh
OMP="-p --mode json --no-session --thinking medium --tools {tools} \
  --no-lsp --no-extensions --no-skills --no-rules --no-context-files --no-title \
  --approval-mode yolo {prompt}"
crab-bench --runs 3 --jobs 8 --timeout 900 --set effort=medium \
  --model claude/claude-sonnet-5-5 --claude-code claude-sonnet-5-5 \
  --command "oh-my-pi sonnet-5-5=omp --model anthropic/claude-sonnet-5-5 $OMP" \
  --model claude/claude-fable-5-2 --claude-code claude-fable-5-2 \
  --command "oh-my-pi fable-5-2=omp --model anthropic/claude-fable-5-2 $OMP" \
  --model codex/gpt-6.1-sol --codex gpt-6.1-sol
```

All 24 tasks, 72 runs a row:

| Model       | Harness     | Passed | Exact | Clean | Requests / run | Tool calls / run | Tokens sent / run | Tokens written / run | Seconds / run | Seconds / run, no slow command |
| ----------- | ----------- | ------ | ----- | ----- | -------------- | ---------------- | ----------------- | -------------------- | ------------- | ------------------------------ |
| Sonnet 5.5  | crab        | 100%   | 92%   | 99%   | 3.3            | 2.7              | 25.8k             | 739                  | 12            | 7                              |
| Sonnet 5.5  | Claude Code | 100%   | 92%   | 97%   | 3.7            | 7.5              | 29.6k             | 1955                 | 19            | 14                             |
| Sonnet 5.5  | oh-my-pi    | 96%    | 88%   | 89%   | 3.5            | 3.6              | 34.3k             | 767                  | 11            | 9                              |
| Fable 5.2   | crab        | 100%   | 92%   | 100%  | 3.2            | 2.9              | 25.4k             | 763                  | 20            | 14                             |
| Fable 5.2   | Claude Code | 100%   | 93%   | 99%   | 3.6            | 7.5              | 32.3k             | 1964                 | 29            | 25                             |
| Fable 5.2   | oh-my-pi    | 96%    | 88%   | 92%   | 3.5            | 3.9              | 34.8k             | 858                  | 21            | 19                             |
| GPT-6.1 Sol | crab        | 100%   | 92%   | 100%  | 3.8            | 3.5              | 20.1k             | 401                  | 23            | 18                             |
| GPT-6.1 Sol | Codex CLI   | 100%   | 92%   | 100%  | -              | 3.3              | 64.0k             | 361                  | 21            | 16                             |

`cmd-slow-command` waits 130 seconds by design, and oh-my-pi's runs of it
ended after about 67, so the last column leaves that task out. The runs that
passed and were not exact are those of `real-delete-tests` and
`real-move-function`, in every row: where a block is cut out or put in, how
many blank lines stay is a matter of taste the expected files settle one way.

Every task, as tokens sent and seconds a run, three runs a cell; every cell
passed three of three unless it says otherwise.

With Sonnet 5.5:

| Task                     | crab         | Claude Code  | oh-my-pi             |
| ------------------------ | ------------ | ------------ | -------------------- |
| `real-add-attributes`    | 25.7k · 8s   | 31.3k · 18s  | 25.4k · 8s           |
| `real-catalog-values`    | 29.5k · 13s  | 55.1k · 30s  | 42.6k · 12s          |
| `real-deep-edit`         | 22.1k · 5s   | 26.1k · 10s  | 22.2k · 7s           |
| `real-delete-tests`      | 35.5k · 10s  | 62.6k · 52s  | 70.3k · 16s          |
| `real-header-every-file` | 22.4k · 7s   | 41.6k · 35s  | 145.2k · 19s         |
| `real-move-function`     | 32.2k · 8s   | 26.4k · 12s  | 30.6k · 8s           |
| `real-rename-method`     | 24.7k · 12s  | 29.7k · 10s  | 28.1k · 12s          |
| `real-rename-type`       | 41.9k · 17s  | 65.7k · 46s  | 63.9k · 19s          |
| `cmd-fix-failing-test`   | 24.9k · 4s   | 24.3k · 6s   | 30.5k · 8s           |
| `cmd-lint-loop`          | 39.2k · 10s  | 33.4k · 10s  | 49.2k · 16s          |
| `cmd-slow-command`       | 24.5k · 135s | 23.5k · 135s | 21.1k · 65s (0 of 3) |
| `cmd-stale-file`         | 40.0k · 7s   | 38.3k · 11s  | 39.3k · 9s           |
| `delete-block`           | 21.2k · 4s   | 19.6k · 7s   | 21.2k · 5s           |
| `insert-block`           | 20.6k · 5s   | 25.3k · 7s   | 21.1k · 5s           |
| `long-file`              | 20.8k · 5s   | 19.0k · 6s   | 21.4k · 6s           |
| `multi-file`             | 27.0k · 6s   | 19.6k · 9s   | 21.8k · 6s           |
| `nested-indent`          | 21.1k · 5s   | 25.8k · 8s   | 20.8k · 6s           |
| `repeated-lines`         | 21.1k · 4s   | 19.2k · 6s   | 21.3k · 4s           |
| `rewrite-body`           | 21.3k · 4s   | 19.4k · 6s   | 21.4k · 5s           |
| `scattered`              | 20.7k · 5s   | 26.5k · 9s   | 20.8k · 6s           |
| `shifting-lines`         | 22.2k · 6s   | 20.5k · 9s   | 21.9k · 6s           |
| `single-line`            | 20.3k · 4s   | 18.1k · 5s   | 20.3k · 5s           |
| `tabs`                   | 20.6k · 5s   | 18.9k · 7s   | 20.8k · 6s           |
| `unicode`                | 20.8k · 4s   | 21.1k · 7s   | 21.4k · 5s           |

With Fable 5.2:

| Task                     | crab         | Claude Code  | oh-my-pi             |
| ------------------------ | ------------ | ------------ | -------------------- |
| `real-add-attributes`    | 25.7k · 13s  | 36.9k · 36s  | 25.6k · 17s          |
| `real-catalog-values`    | 45.3k · 28s  | 50.2k · 46s  | 43.8k · 24s          |
| `real-deep-edit`         | 22.1k · 13s  | 28.6k · 23s  | 22.2k · 17s          |
| `real-delete-tests`      | 30.7k · 16s  | 52.8k · 73s  | 67.6k · 34s          |
| `real-header-every-file` | 22.7k · 17s  | 38.0k · 52s  | 162.6k · 39s         |
| `real-move-function`     | 31.5k · 17s  | 32.3k · 24s  | 30.6k · 17s          |
| `real-rename-method`     | 25.1k · 20s  | 104.3k · 19s | 30.5k · 23s          |
| `real-rename-type`       | 36.5k · 33s  | 52.8k · 69s  | 55.0k · 39s          |
| `cmd-fix-failing-test`   | 27.3k · 11s  | 24.9k · 12s  | 24.5k · 12s          |
| `cmd-lint-loop`          | 27.2k · 18s  | 36.5k · 25s  | 36.1k · 23s          |
| `cmd-slow-command`       | 24.4k · 140s | 24.0k · 140s | 15.6k · 69s (0 of 3) |
| `cmd-stale-file`         | 34.7k · 14s  | 33.1k · 17s  | 40.2k · 17s          |
| `delete-block`           | 21.5k · 12s  | 20.2k · 15s  | 26.7k · 12s          |
| `insert-block`           | 20.8k · 12s  | 19.3k · 12s  | 26.0k · 17s          |
| `long-file`              | 20.9k · 9s   | 27.1k · 16s  | 21.5k · 13s          |
| `multi-file`             | 21.6k · 12s  | 29.7k · 22s  | 22.2k · 14s          |
| `nested-indent`          | 21.1k · 10s  | 19.6k · 11s  | 20.8k · 10s          |
| `repeated-lines`         | 21.1k · 8s   | 19.7k · 12s  | 21.3k · 11s          |
| `rewrite-body`           | 21.3k · 8s   | 24.0k · 16s  | 24.1k · 16s          |
| `scattered`              | 20.9k · 12s  | 20.9k · 14s  | 21.5k · 14s          |
| `shifting-lines`         | 22.2k · 12s  | 20.9k · 15s  | 30.8k · 19s          |
| `single-line`            | 20.4k · 9s   | 19.2k · 11s  | 20.3k · 11s          |
| `tabs`                   | 20.6k · 12s  | 19.4k · 14s  | 20.7k · 13s          |
| `unicode`                | 23.6k · 13s  | 19.7k · 13s  | 23.9k · 15s          |

With GPT-6.1 Sol:

| Task                     | crab         | Codex CLI     |
| ------------------------ | ------------ | ------------- |
| `real-add-attributes`    | 24.7k · 23s  | 63.9k · 21s   |
| `real-catalog-values`    | 26.1k · 27s  | 72.7k · 22s   |
| `real-deep-edit`         | 19.9k · 15s  | 51.6k · 16s   |
| `real-delete-tests`      | 26.9k · 18s  | 56.6k · 17s   |
| `real-header-every-file` | 12.9k · 17s  | 56.5k · 15s   |
| `real-move-function`     | 25.1k · 27s  | 69.5k · 15s   |
| `real-rename-method`     | 56.0k · 28s  | 52.4k · 14s   |
| `real-rename-type`       | 19.9k · 36s  | 58.9k · 16s   |
| `cmd-fix-failing-test`   | 27.3k · 20s  | 70.4k · 18s   |
| `cmd-lint-loop`          | 35.4k · 34s  | 77.7k · 32s   |
| `cmd-slow-command`       | 14.4k · 138s | 159.2k · 140s |
| `cmd-stale-file`         | 25.6k · 18s  | 85.4k · 17s   |
| `delete-block`           | 12.4k · 10s  | 50.8k · 12s   |
| `insert-block`           | 16.5k · 16s  | 51.1k · 14s   |
| `long-file`              | 16.9k · 14s  | 54.3k · 15s   |
| `multi-file`             | 23.7k · 23s  | 68.5k · 15s   |
| `nested-indent`          | 12.3k · 10s  | 56.3k · 12s   |
| `repeated-lines`         | 12.3k · 11s  | 51.1k · 12s   |
| `rewrite-body`           | 12.4k · 10s  | 68.5k · 18s   |
| `scattered`              | 13.2k · 10s  | 51.7k · 12s   |
| `shifting-lines`         | 13.3k · 11s  | 51.9k · 15s   |
| `single-line`            | 12.1k · 8s   | 50.7k · 13s   |
| `tabs`                   | 11.9k · 10s  | 50.8k · 12s   |
| `unicode`                | 12.3k · 9s   | 56.3k · 16s   |

What the runs showed:

- Crab passed every run with all three models, as did Claude Code and the
  Codex CLI. oh-my-pi failed all six runs of `cmd-slow-command`: it moved the
  command to the background after a minute and, run without a reader as here,
  ended the run before the command finished.
- Crab sends the fewest tokens with every model: an eighth to a fifth fewer than
  Claude Code and a quarter fewer than oh-my-pi with the Claude models, and a
  third of what the Codex CLI sends with GPT-6.1 Sol.
- With the Claude models crab is the fastest once the slow command is left
  out, and makes the fewest tool calls.
- With GPT-6.1 Sol crab is a little slower than the Codex CLI, 18 seconds a
  run against 16. The renames and the move are where it loses the time: the
  Codex CLI does them with a shell command, and crab's model writes each
  changed line. On `real-rename-method` crab also sends more tokens.
- Crab is clearly ahead where a change is large or spread out: deleting 569
  lines of tests from four files, adding a first line to all fifteen files,
  which it does without reading them, and renaming a type in 46 places.
- The model writes about two and a half times as much with Claude Code as
  with crab or oh-my-pi.
- With Sonnet 5.5 crab is behind Claude Code on `multi-file`,
  `real-move-function` and `cmd-lint-loop`, and on the smallest tasks it sends
  about 2,000 more tokens a run than Claude Code, while level with oh-my-pi.

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
