# Searching code by meaning

`code_search` finds code by what it does as well as by its words. A query in
plain words, such as `where are retries scheduled` or `check permissions before
writing a file`, finds the functions that do it even when they share no word
with the query; identifiers in the query find their definitions and uses as
`grep` would. It is for when the agent knows what the code does but not what
it is called or where it lives; `grep` stays the tool for an exact string or
regular expression, and `lsp` for the references of a known symbol.

| Argument | What it does                                                                                       |
| -------- | -------------------------------------------------------------------------------------------------- |
| `query`  | What to look for, in plain words, identifiers or both. Required.                                   |
| `path`   | Keeps to files a glob matches (`*.rs`, `crates/omc-tools/**`), or to a folder or file named as it is (`src/server`). A glob without `/` matches at any depth. |
| `limit`  | How many results, 1 to 30; 8 by default.                                                           |

Each result is a chunk of a file, as `path:first-last`, followed by at most 12
of its lines, numbered like `read` numbers them: the run of lines that holds
the most words of the query. A script gets the results as data:
`{ results: { path, start, end, text }[], keyword_only }`.

`code_search` only reads the project, so it never asks, plan mode allows it,
and subagents and scripts get it. It is on the tool list, not hidden: it is
meant as the first step of finding one's way around a project. It can be
hidden with the rest through `OMC_DEFERRED_TOOLS`; see
[hidden tools](README.md#hidden-tools).

## How the index is built

The first call indexes the working folder; every later call brings the index
up to date first, so a search sees the files the agent just wrote.

- **Which files.** The files `git` would see: `.gitignore` and the other
  ignore files are honoured, hidden files are included, `.git` is not.
  Files over 1 MiB, files that are not UTF-8 text, lockfiles such as
  `Cargo.lock` and `package-lock.json`, `*.min.js` and files that commonly
  hold secrets (see [below](#permissions-and-secrets)) are left out, and the
  index stops at 50,000 files, saying so in the result.
- **Chunks.** Code in Rust, Go, TypeScript, TSX, JavaScript, Python, C, C++,
  Java, Lua, CSS, HTML and JSON is cut along its syntax tree, with the same
  tree-sitter grammars as `ast_grep`: neighbouring items are packed into
  chunks of at most 40 lines, the comments before an item staying with it,
  and an item longer than that is cut along the items inside it, such as the
  methods of an `impl` or a class. Other files are cut into windows of 40
  lines that overlap by 10. A chunk keeps at most 4,000 bytes. Items nested
  more than 32 levels deep, and every item once 20,000 syntax nodes of a file
  have been looked at, are cut into windows instead, so deeply nested or
  generated code costs no more than plain text.
- **Words.** Each chunk's path, text and the pieces of its `camelCase` and
  `PascalCase` identifiers go into an SQLite FTS5 table, ranked by BM25 with
  the path counting double. A query word of three letters or more also
  matches as the start of a longer word.
- **Meaning.** Each chunk, with its path, is embedded by
  [`minishlab/potion-code-16M`](https://huggingface.co/minishlab/potion-code-16M),
  a static embedding model trained for code: it averages a learned vector per
  token, so it needs no GPU and embeds thousands of chunks a second on one
  core. The vectors are kept in a `vec0` table of
  [sqlite-vec](https://github.com/asg017/sqlite-vec) and compared by cosine.
- **Ranking.** The 50 best chunks by words and the 50 nearest by meaning
  (more when `limit` is high) are fused by reciprocal rank, each list counting
  `1 / (60 + place)`, so a chunk high in both comes first.
- **Keeping up.** A file whose size and time of change are as recorded is not
  read again; one that changed is read, and cut and embedded again only when
  its bytes changed. Files that are gone leave the index.

The model, 64 MB, is downloaded once from a pinned revision on Hugging Face
by the first `code_search` call that needs it, into `<cache>/index/models/`.
While it cannot be downloaded or loaded, the search goes by words alone and
the result starts with `[keyword search only: <reason>]`; it is tried again
after ten minutes. Chunks indexed without the model are embedded once it is
there.

Each project has its own database, `<cache>/index/projects/<name>-<hash>.sqlite3`,
named after the folder and a hash of its path, so a linked worktree gets an
index of its own. Several conversations can share one: SQLite's locks keep
their refreshes apart. Deleting the folder only costs the next search the time
to build it again.

## Permissions and secrets

`code_search` answers to [permission rules](../extending/permissions.md)
written for `Grep` and `Read` as well as to its own name: a deny rule without
a path, such as `Grep` or `Read`, stops it. Rules with a path cannot stop a
search, since it does not know beforehand which files it will find; instead
every result whose file a `Read` or `Grep` deny rule touches is left out, as
is, outside yolo mode, every one an ask rule touches. Nothing in the result
says that any were left out, so a search cannot tell whether a hidden file
matched. So with `Read(.env)` denied,
`code_search` never shows `.env`, just as `read` never reads it. The same holds
when a script calls it.

Files that commonly hold secrets are never indexed at all, whatever the rules
say, and are dropped from an index built before: `.env` and `.env.*`,
`.envrc`, `.netrc`, `.npmrc`, `.pypirc`, `.pgpass`, `.htpasswd`,
`.git-credentials`, `id_rsa`, `id_dsa`, `id_ecdsa` and `id_ed25519` with
anything after a `.` or `_`, `credentials`, `credentials.json`,
`secrets.json`, `secrets.yaml`, `secrets.yml`, `service-account.json`, and
files ending in `.pem`, `.key`, `.crt`, `.cer`, `.der`, `.p12`, `.pfx`,
`.jks`, `.keystore`, `.kdbx`, `.gpg`, `.asc`, `.ovpn`, `.tfstate` or
`.tfvars`, in any case. `read` and `grep` can still open them, under the
permission rules.

## Configuration

| Variable         | What it does                                                                                        | Default         |
| ---------------- | --------------------------------------------------------------------------------------------------- | --------------- |
| `OMC_CODE_INDEX` | Set to `0` to turn the index off: no `code_search`, and `memory_search` goes by words alone.         | on              |
| `OMC_INDEX_DIR`  | The folder the indexes and the model are kept in. Point it at a folder holding the model's files under `models/potion-code-16M` to work offline. | `<cache>/index` |

See [configuration](../configuration.md) and [memory](../memory.md) for how
`memory_search` uses the same model.
