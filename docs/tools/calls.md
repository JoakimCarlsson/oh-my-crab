# How tool calls run

Arguments the model gets slightly wrong are put right against the tool's
schema before the tool runs, for built-in tools and tool-server tools alike: a
number or boolean sent as a string, a single value where a list is wanted, an
object or list sent as JSON text, and `null` for an optional argument. When the
arguments still do not fit, the call is turned away with every place that is
wrong and what was expected there.

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
