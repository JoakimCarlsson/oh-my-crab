# Language services

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
  A client that also sets `navigation` answers the `lsp` tool's questions
  about code as well (see below); one that does not is never sent them.
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

## The `lsp` tool

The agent can also ask the language servers about code itself, with the `lsp`
tool. Each call names an `action`:

| Action            | Needs                                    | Answers                                                                 |
| ----------------- | ---------------------------------------- | ----------------------------------------------------------------------- |
| `definition`      | a place                                  | where the symbol is defined                                             |
| `type_definition` | a place                                  | where the symbol's type is defined                                      |
| `implementation`  | a place                                  | where the trait, interface or method is implemented                     |
| `references`      | a place                                  | every use of the symbol, its declaration included                       |
| `hover`           | a place                                  | the symbol's signature and documentation                                |
| `symbols`         | `path`, or `query`                       | the outline of a file, or the project's symbols whose names match        |
| `diagnostics`     | `path` (a file or a folder), or nothing  | the errors, warnings and hints of the file, the folder or the project   |
| `rename`          | a place and `new_name`                   | renames the symbol in every file that uses it                           |

A place is a file (`path`) and the symbol in it (`symbol`), written as it is
in the code, best with the `line` it is on, counted from one. The tool finds
the character the symbol starts at on that line (`occurrence` picks one when
it appears more than once), points a qualified name such as `Config::load` at
its last part, and looks a few lines either side when the line is a little
off, saying so in the result. Without a line it takes the symbol's first
whole-word occurrence in the file. `line` and `column` can be given instead of
`symbol`.

Results are short: places grouped by file, each as `line:column` and the line
quoted, at most 100 of them; outlines with each symbol's line, kind and
detail. Before asking, the server of the file is started if it is not running
and, the first time, given time to load the project, and every file it was
told about is brought up to date with what is on disk. A server that does not
offer an action (it did not say so when it started) is reported as not
answering it, and a file no installed server reads gets a message naming the
missing server; the model is told to fall back to `grep` or `ast_grep`.

`rename` asks the server for the edits, refuses them when they reach outside
the working directory or would create, move or delete files, and otherwise
writes every changed file or none. The written files then go through the same
steps as an `edit`: they are formatted, their diagnostics are added to the
result, and each file's new snapshot tag is given. Outside yolo mode a rename
asks first, under the scope `lsp rename`; `dry_run: true` shows the changed
lines without writing or asking. Every other action only looks: plan mode and
subagents in plan mode have a copy of the tool that refuses renames other than
dry runs.

Where the answers come from follows the language services above:

- With the harness's own servers, every action works for the languages whose
  servers are installed. `symbols` with a `query` and `diagnostics` for the
  whole project ask every server already running and those of the languages
  the project root names, starting them.
- With a client that runs the language servers and navigates code
  (`languages` and `navigation` in `Open`), every action is asked of the
  client as a `Language` request: `definition`, `type_definition`,
  `implementation`, `references`, `hover`, `document_symbols`,
  `workspace_symbols`, `workspace_diagnostics` and `rename`, each place
  counted in lines and characters from one (see
  [wire protocol](wire-protocol.md#language-1)). The client answers from its
  own servers, `unsupported` when a server does not offer the action and
  `no_server` when none reads the file; the tool gives the model its reason.
  A rename comes back as edits, never written by the client: `crab` makes
  them to the files on disk and then treats them as any rename, refusing
  edits outside the working directory, asking first outside yolo mode,
  writing every file or none, having the client format them and report their
  diagnostics, and giving each file's new snapshot tag. `crab` waits a minute
  for each answer.
- With a client that runs the language servers but did not set `navigation`,
  as a client written before it existed, the wire carries only formatting and
  the diagnostics of named files, so `lsp` answers `diagnostics` for a file
  through the client and refuses the other actions with a message saying so,
  without asking the client anything it would not understand.
- A program embedding the crates answers whatever its `LanguageService`
  implements; the navigation methods of that trait refuse by default.
