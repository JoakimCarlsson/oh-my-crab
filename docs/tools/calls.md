# How tool calls run

Arguments the model gets slightly wrong are put right against the tool's
schema before the tool runs, for built-in tools and tool-server tools alike: a
number or boolean sent as a string, a single value where a list is wanted, an
object or list sent as JSON text, and `null` for an optional argument.
Arguments that are one item of the list a tool requires become that list: an
`edit` call that sends one file's `path`, `tag` and `ops` without the `files`
list around them means that one file. When the arguments still do not fit, the
call is turned away with every place that is wrong and what was expected
there.

A call whose name is a tool's name with stray characters around it, such as a
space or an underscore in front, is taken as a call of that tool.

## Which calls run at once

When one reply asks for several calls, every call is checked, shown to the
hooks and, when it needs approval, asked about first, in the model's order.
Only once every question is answered do the calls run, still in the model's
order, each tool having a class:

- **Shared** calls only look and may overlap: a run of shared calls next to
  each other runs at once.
- **Exclusive** calls change files or have some other effect, and run alone:
  each waits for every call before it to end and holds back every call after
  it.

So `read a`, `read b`, `edit a`, `read a` reads both files at once, then edits,
then reads again, and the edit sees nothing the reads have not finished with.
A call that was denied or refused never runs and holds nothing back. The
results come back in the model's order whatever order the calls ended in.

| Class     | Tools |
| --------- | ----- |
| Shared    | `read` (files and the web), `grep`, `glob`, `ast_grep`, `web_search`, `lsp` except the actions that write files (rename, applying a code action, format, rename_file), `memory_search`, `skill`, `wait`, `mcp_resources`, `recall`, and `task` when every subagent it starts is of a kind that only looks (or in plan mode) |
| Exclusive | `bash`, `edit`, `write`, `ast_edit`, `lsp` renames, `debug`, `script` outside plan mode, other `task` calls, tool-server tools, and the tools that steer the conversation or talk to the reader: `ask`, `todo`, `context_notes`, `new_context`, `checkpoint`, `rewind`, `remember`, `memory_edit`, `set_title`, `exit_plan_mode` |

A tool-server tool is shared when its server marks it with the `readOnlyHint`
annotation; the hint only lets its calls overlap, and it still asks first and
plan mode still refuses it. A `script` whose tools all only look, as in plan
mode, is shared. The calls a script makes follow the same classes among
themselves: shared calls started with `Promise.all` overlap, while an
exclusive call, or one that needs approval, waits for the calls started before
it and runs alone.

## Output

Long output is not lost. `bash` shows the first 4,000 and the last 26,000 bytes
of each of stdout and stderr, and a tool server's text is cut to its first and
last 10 KB; the whole output is saved to a file the result names, which the
model can read in parts with `read`. The files go under
`oh-my-crab/output/<conversation id>` in the user's cache folder, so they stay
readable when the conversation is opened again. On Unix the folders are made
readable by the user alone (`0700`) and the files too (`0600`). When a
conversation opens, other conversations' folders none of whose files was
written for seven days are removed; the folder of the conversation being
opened is always kept.

`bash` condenses the output of noisy commands before the model reads it: git
(status hints, and push, pull, fetch and clone progress), cargo, npm, pnpm,
yarn and bun, make and ninja, gcc and clang, go, pytest, docker and podman, and
apt and dnf. Build steps, downloads and passing tests are counted on one line
("… 143 Compiling lines"), progress and bookkeeping lines are dropped, colour
codes and progress bars redrawn in place are cleaned up, and a line repeated
straight after itself is counted. Lines that report an error or a warning are
always kept, and every line that is kept is shown byte for byte as printed.
`git log`, `git show` and `git diff` are never condensed, nor are the version
changes `cargo update` and `cargo add` report or compiler include chains. The
command is recognised from the last command of the last pipeline only, past
variable assignments, `sudo`, `env`, `time` and the like: `cd x && cargo build`
is condensed, while `cargo build && ./target/debug/app` and
`cargo test | grep FAILED` are left alone. Whenever condensing leaves a line
out, the raw output is saved whole, however short, and the result ends with a
note such as `[stderr condensed: 412 lines → 38; the whole output is in <path>;
…]`; when it only cleaned up colour codes, redraws or blank lines, the raw
output is shown instead. The rules are TOML files, one per family of tools, in
`crates/omc-tools/src/tools/bash/rules`. Set `OMC_BASH_CONDENSE=0` to show all
output as it is.

`bash` also steers the model to the dedicated tools, turning a command away
only when the tool gives exactly the same result. A command line that is
nothing but `cat`, `less` or `more` of one to four text files, `head` or `tail`
of a number of lines of one text file, or `echo`, `printf` or a `cat`
here-document writing text the shell does not expand over a file (the
delimiter quoted, or no `$`, backtick or backslash in the body) is not run; the
result names the call that does the same (`read` with `path:1-` or a line
range, or `write`). Every word must be literal, every path inside the working
directory, every file text rather than binary, and every option one the tool
reproduces. Everything else runs: searches with `grep` or `rg`, `find`,
`ls -R` and `sed -i` (the dedicated tools skip ignored and binary files, match
only files, or replace exact text, so their results differ), appending with
`>>`, other options, pipelines, lists and commands sent to the background with
`&`. Such a command runs with a one-line tip naming the tool when it starts a
pipeline. Set `OMC_BASH_INTERCEPT=0` to run every command.
