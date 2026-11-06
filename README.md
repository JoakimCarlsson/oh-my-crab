# oh-my-crab

`crab` is a coding-agent harness with no interface of its own. It runs the
agent loop, the tools and the model providers for one conversation, and talks
to the program that started it with JSON lines on standard input and output.

It is inspired by [oh-my-pi](https://github.com/can1357/oh-my-pi) and is
primarily used by [pandemonium](https://github.com/JoakimCarlsson/pandemonium),
which starts it for each agent session and draws the conversation itself. Any
other front end, such as a terminal UI, can use the same protocol.

## Install

```sh
make install
```

This installs `crab` under `~/.local/bin`. Set `PREFIX` to install elsewhere.

## Usage

```text
crab serve                      hold a conversation over standard input and output
crab tools                      print the tools the agent can have, as a JSON list
crab providers                  print the providers, as a JSON list of id, name and sign-in
crab account <p> [folder]       print who provider <p> is signed in as, or nothing
crab history <id> [folder]      print a saved conversation: title, settings and turns
crab rename <id> <folder> <t>   name a saved conversation nobody has open
crab import <id> <folder>        save a conversation a client kept itself, read as JSON
```

A conversation opened with an id is saved by `crab`: its messages, its turns,
its title, the settings chosen for it and the list of what can be set, the commands it offers, the context window, its notebook, the files each tool call changed, and the turns that were stopped or failed, which the model never sees but a client still shows. It comes back as it was
when the same id is opened again, so a client does not need to keep a copy. The
agent names an unnamed conversation itself with `set_title`; a name the client
sends with `Rename` always wins.

A prompt sent while a turn is running is not dropped. It is queued, and the
client is told with a `Queued` event in the `Waiting` state, carrying a ticket
that every later event about it repeats. At the next safe point of the running
turn, after its current batch of tool calls and any compaction, and before the
next model request, the waiting prompts are put into the conversation in the
order they came, so the model reads them mid-turn; each is then reported as
`Steered` and saved as a turn of its own under the id the client gave it. When
the model's final answer ends the turn before a prompt could be put in, that
prompt starts the next turn by itself and is reported as `Started`; any others
wait for that turn. Prompts with files are handled like any other prompt. When
the turn is cancelled, fails or needs signing in again, the prompts still
waiting are never sent: each is reported as `Dropped` so the client can offer
it again, and prompts already put into a cancelled or failed turn are saved
with that turn as stopped or failed. A `/compact` sent while a turn is running
is refused with a `Failed` event; the other commands work as they do between
turns.

The commands and events are defined in `crates/omc-wire`. `crates/omc-client`
is a small blocking client for programs that start `crab` and talk to it.

## Tool calls

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
readable when the conversation is opened again, and folders nobody wrote to for
seven days are removed when a conversation opens.

`bash` condenses the output of noisy commands before the model reads it: git
(status hints, blank lines in `log`, push, pull, fetch and clone progress),
cargo, npm, pnpm, yarn and bun, make and ninja, gcc and clang, go, pytest,
docker and podman, and apt and dnf. Build steps, downloads and passing tests are
counted on one line ("… 143 Compiling lines"), progress and bookkeeping lines
are dropped, colour codes and progress bars redrawn in place are cleaned up, and
a line repeated straight after itself is counted. Lines that report an error or
a warning are always kept. The command is recognised from the last command of
each pipeline, past `cd x &&`, variable assignments, `sudo`, `env`, `time` and
the like, so `cargo test | grep FAILED` is left alone. Whenever condensing
leaves anything out, the raw output is saved whole, however short, and the
result ends with a note such as `[stderr condensed: 412 lines → 38; the whole
output is in <path>; …]`. The rules are TOML files, one per family of tools,
in `crates/omc-tools/src/tools/bash/rules`. Set `OMC_BASH_CONDENSE=0` to show
all output as it is.

`bash` also steers the model to the dedicated tools. A command line that is
nothing but a plain `cat`, `head`, `tail` or `less` of a file, a `grep` or `rg`
search, a `find -name` or `ls -R`, a `sed -i` on one file, or an `echo`,
`printf` or `cat <<EOF` into a file is not run; the result names the tool to
use (`read`, `grep`, `glob`, `edit` or `write`) and the arguments that do the
same. Only forms whose meaning is certain are turned away: every word literal,
every path inside the working directory, every option one the dedicated tool
has. Pipelines, redirections and anything else that needs the shell run as
usual, with a one-line tip when they start with such a command. Set
`OMC_BASH_INTERCEPT=0` to run every command.

## Long conversations

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
demand; it takes no files.

A compaction deletes nothing. It is recorded at the point it happened, the
summary goes to the model in place of the older messages, and the whole
conversation stays saved and is still shown when it is opened again, with the
compaction as a finished `compact` step holding the summary. When the todo
list the model last wrote before a compaction is hidden by it and still has
open items, they are listed again after the summary. A `/compact` the
reader sent comes back as a turn of its own, and one that failed comes back
with its reason.

Nothing left out or summarized is lost to the model either. Once a
conversation has been pruned or compacted, the model is offered a `recall` tool
that reads the whole history again: the full output of one tool call, the
lines that contain some text, or a range of messages by number.

## When things go wrong

A request that fails in a way that passes on its own (a rate limit, an
overloaded or failing server, a dropped connection or a stream that ends before
the reply is complete) is sent again, up to 8 times. The wait doubles each
time from about 2 seconds, with some jitter, up to 60 seconds; when the
provider says how long to wait with `retry-after` or `retry-after-ms`, that wait
is kept to, and a provider that asks for more than 60 seconds is not waited
for. Each wait shows as a running `retry` step saying which attempt it is and
why, and stopping the turn ends it. Sign-in problems, spent quotas, billing,
missing credits and requests the provider will not take fail the turn at once.

A reply that is empty, or that the output limit cut off before any tool call,
is not kept: the model is sent a short notice, which the client never shows as
a turn and the history does not keep, and asked again, up to three times a
turn. A reply cut off part of the way through its text is carried on and kept
as one reply. The tool calls of a reply the output limit cut off are never run:
each fails with the reason, and the model is told to take a smaller step. A
call whose arguments are not a valid JSON object fails with what is wrong with
them.

When the model makes the same calls with the same arguments five times in a
row, or makes three edits in a row to one file that leave it as it was, a
notice telling it to change course is added to the results of that step, and
after six such edits it is told to stop and explain to the user.

The todo list the model keeps with `todo` is watched as well. When it still
has items pending or in progress after 12 successful `edit`, `write`,
`ast_edit` or `bash` calls since it last changed, a reminder listing them is
added to the results of that step, at most twice before the list changes
again. When a turn that changed files is about to end with open items, the
model is sent a notice, once a turn, to finish them or update the list, and its
next reply is kept as one with the first. Neither happens when the model's
reply ends by asking the user a question.

A reply without tool calls whose text holds a call written out as text is put
right. A block marked as a call, such as `<tool_call>{"name": "read",
"arguments": {...}}</tool_call>`, becomes that call when the tool exists, and so
does a fenced JSON block that is a call of a known tool or the arguments of an
`edit`, when the reply says little else. A fenced call amid a longer answer is
never run; the model is sent a notice to make the call if it meant to, and its
next reply is kept as one with the first. Either counts against the three
recoveries of a turn. Any other code block, JSON included, is left alone.

## Reading code

`read` of a code file without a line selector returns an outline instead of
the text when the file has at least 200 lines and is written in Rust, Go,
TypeScript, JavaScript, Python, C, C++, Java or Lua. The file is parsed with
the same tree-sitter grammars as `ast_grep`. Imports, signatures, type, trait,
impl and class headers, constants and comments are shown with their real line
numbers, and every function or method body that would hide at least six lines
is folded to one unnumbered line such as `… 30 lines folded (120-149)`. A
Python docstring stays visible, and a callback that only calls other functions
with callbacks, such as a `describe` block, is opened so that each test inside
it is folded on its own. A footer lists the folded ranges as a selector, for
example `read src/lib.rs:120-149,200-260`, and `src/lib.rs:1-` reads the whole
text. A file is read whole as before when it is shorter, does not parse
cleanly, is larger than 4 MiB, or when folding would hide less than a quarter of
its lines. A selector always returns the text. A program that builds the
tools itself can turn outlines off with `Read::with_outline(sandbox, false)`.

An outline carries a tag of its own, different from the tag of the text.
`edit` accepts that tag, works out the outline's folds again from the file and
refuses any operation that touches a folded line, naming the range to read
first. Reading a range returns the plain tag, which allows edits anywhere, as
does the tag `edit` returns. Each kind of read is kept apart when old reads are
left out: an outline is only made out of date by another bare read of the same
path, never by a read of a range, and a read of a range never by an outline.

### Merge conflicts

When the lines `read` returns include a merge conflict marker (`<<<<<<<`,
`|||||||`, `=======` or `>>>>>>>` at the start of a line), a note after them
lists every conflict in the file: its number, counting from 1 in file order,
its lines, and each side with its label and lines, the common ancestor's side
included for the diff3 and zdiff3 styles. A file with conflicts is never read
as an outline. `read src/lib.rs:conflicts` returns only the conflict blocks,
numbered, under the file's tag.

`edit` resolves a conflict by number without the model typing the block out:
`{"op": "resolve", "conflict": 2, "take": "theirs"}` replaces the whole block,
markers included, with one side, `ours`, `theirs`, `base` or `both` (ours then
theirs), or with `text` instead of `take`, where an empty text removes the
block. The number is checked against the tag like any other operation, so a
file changed since the read is refused rather than resolved in the wrong
place.

### Generated files

`edit`, `write` (when it would overwrite a file) and `ast_edit` refuse files
that a tool or package manager writes. A file counts as one when a comment in
its first 30 lines carries a generator's banner, such as `Code generated …
DO NOT EDIT`, `@generated`, `This file was automatically generated`,
`<auto-generated>` or the banners of protoc, gRPC, sqlc, buf, Swagger, OpenAPI
Generator, FlatBuffers and Thrift, or when its name is a lockfile
(`Cargo.lock`, `package-lock.json`, `yarn.lock`, `pnpm-lock.yaml`, `go.sum` and
others) or one generators use (`*.pb.go`, `*_pb2.py`, `*.g.dart`,
`*.designer.cs` and others). The refusal names the evidence and tells the
model to change the source or the generator instead. When the user explicitly
asks for the file itself to change, the model sets `generated: true`: in the
file's section for `edit`, and as an argument of `write` and `ast_edit`. An
`ast_edit` over a directory leaves generated files unchanged and lists them.

### Mistyped paths

When `read` is given a path that does not exist, it looks for files in the
working directory, honoring `.gitignore`, whose path ends with the one given,
or matches it when case is ignored. If there is exactly one, it is read and the
result begins by saying which file was read instead; if there are several, the
error lists up to five. `edit` never follows such a path: its error names the
candidates. `write` refuses a new file whose directory does not exist when
existing files end with its path, and creates it anyway with `create: true`.

## Reading images, databases, archives and JSON

An image larger than 3.75 MB, or longer than 2000 pixels on a side, is not
refused but shrunk before it reaches the model: it is decoded, turned upright
by its EXIF orientation, scaled so its long side is at most 2000 pixels and
re-encoded, as PNG when it has transparent pixels and that stays small enough,
as JPEG at quality 80 otherwise. Lower JPEG qualities and then smaller sizes
are tried until it fits. The header names the original and shown sizes and
the factor to multiply coordinates by to map them back to the original.
Images that already fit are passed on unchanged; files over 64 MiB are
refused.

The text after a database, archive or JSON file's name and a `:` names what
to read inside it, and the usual line selectors page the result:

| Path                           | Result                                                    |
| ------------------------------ | --------------------------------------------------------- |
| `app.db`                       | tables and views, with row counts and `CREATE` statements |
| `app.db:users`                 | the schema and the first 50 rows, numbered from 1         |
| `app.db:users:51-100`, `:-20`  | other rows; `offset` and `limit` work too                 |
| `app.db:SELECT ...`            | a read-only query, paged the same way                     |
| `src.zip`                      | members with their sizes                                  |
| `src.zip:lib/a.rs:10-40`       | a member's text, numbered, without a tag                  |
| `src.zip:lib`                  | the members under a directory                             |
| `data.json:.items[0].name`     | the jq filter's output, pretty-printed and numbered       |
| `data.json:.items[]:100-200`   | lines of that output                                      |

SQLite files are found by their header, whatever their name. The database is
opened read-only with `query_only` set, only `SELECT`, `WITH` and
`PRAGMA table_info` statements run, a statement that would write is refused,
and a query is stopped after 10 seconds. Zip files are found by their header,
tar files by their header or `.tar` name, and gzipped tar files by a gzip
header and a `.tar.gz` or `.tgz` name. A member is unpacked only up to
32 MiB and a gzip stream only up to 1 GiB, so a small archive cannot unpack
into unbounded memory. Binary members are reported with their size; image
and PDF members are shown as images and PDF files are. JSON filters run
in-process with the full jq language of `jaq`; a JSON file followed by a line
selector such as `data.json:20-40` is read as text, as before. A read inside
a database, archive or JSON file that takes more than 20 seconds is given up
on and fails, so a filter that never ends cannot hold the turn; it keeps its
thread busy until `crab` exits.

## Prompt cache

Providers cache the start of a request, tools then system prompt then
messages, only while it stays byte for byte the same. `crab` therefore makes
the system prompt and the tool list once per conversation, when its first
turn starts, and sends them unchanged after that; they are made again only
when the model changes or the checkpoint tools are turned on or off. What
changes later reaches the agent as a short notice at the start of the next
prompt instead:

- today's date and the git branch, on the first prompt, again when either
  changes, and again after a compaction or a fresh context window has dropped
  the earlier notice;
- plan mode turning on, with its rules and the tools it allows, and turning
  off;
- skills, subagent kinds and instruction files that were added, changed or
  removed on disk since the system prompt was written.

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
a network mount say, gives no outline at all. The outline is made once, when
the conversation's first turn starts, and kept unchanged for the rest of it,
also when the system prompt is made again for another model; the prompt says
it is a snapshot from the start of the conversation. Subagents get the same
outline in their own system prompts.

Plan mode no longer takes tools off the list. The tools that change things
stay listed, and a call to one while plan mode is on is refused with a
message naming the tools that are allowed; it is never run and never asks for
approval. Outside plan mode `exit_plan_mode` is refused the same way.

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
  after it while idle, when the model or a cache setting changes, and for
  models that think within a fixed token budget, whose requests cannot be
  replayed without output. What warming cost is saved with the conversation
  under `cache_warming`.

Prices per model and cache lifetimes are in `crates/omc-providers/src/models.rs`.
The requests that summarize a conversation for a compaction write nothing to
the cache, since they are never sent again.

## Language services

After `edit` or `write` changes a file, `crab` has it formatted as its language
says and adds the errors and warnings found in the files it changed to the
tool's result, at most 20, waiting up to three seconds for them. The tag in the
result is the tag of the formatted file, so the next edit can use it.

The formatting and the diagnostics come from whoever can provide them, and only
one set of language servers runs:

- A client that runs language servers of its own sets `languages` when it opens
  the conversation. `crab` then sends it each request as a `Language` event
  with a ticket, and it answers with a `Language` command under that ticket. A
  request it does not answer in time gets no formatting and no diagnostics.
- Otherwise `crab` starts the usual server of each language itself, when its
  program is installed: `rust-analyzer`, `gopls`, `typescript-language-server`,
  `pyright-langserver` or `pylsp`, `clangd`, `lua-language-server` and `zls`.
  Loading a project can run its build scripts, so a server starts the first
  time a file of its language is written, which the user allowed. In `yolo`
  mode the servers of the languages the project root names (`Cargo.toml`,
  `go.mod`, `package.json` and so on) start when the conversation opens
  instead, so the first write already gets its diagnostics; a server started
  on the first write may still be loading and miss them. A server that stops
  is started again, up to three times. These come from `crates/omc-lsp`, which
  the default `lsp` feature of `omc-session` builds in; a build without it has
  no servers of its own.
- A program that embeds `omc-session` can pass its own
  `omc_tools::LanguageService` in `Options` to `run_with`, and one that builds
  the tools itself can hand `Registry::builtin_in` its own `Languages`.

With none of these, `edit` and `write` behave as they would without language
services.

## Providers

`claude`, `codex`, `copilot`, `openrouter` and `ollama`. See
[docs/providers](docs/providers/supported.md) for sign-in and how to add one.

A client lists them with `crab providers`, or `omc_session::providers()` when it
links the harness, so a provider added to crab shows up in every client without
a change there.

## Development

```sh
make fmt
make lint
cargo test --workspace
```

## License

MIT, see [LICENSE](LICENSE).
