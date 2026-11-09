Search the project's code by meaning and by keyword at once. Use it first when you know what code does but not what it is called or where it lives ("where are retries scheduled", "parse the config file", "permission check before writing"); use `grep` for an exact string or regex, and `lsp` for references of a known symbol.
- `query`: what you are looking for, in plain words, identifiers or both.
- `path`: keep to files this glob matches (`*.rs`, `crates/omc-tools/**`) or to this folder or file (`src/server`).
- `limit`: 1 to 30 results, 8 by default.
Each result is a chunk of a file, usually one definition with its comments, as `path:first-last` and the lines that best match, numbered. The index honours `.gitignore`, is built on the first call and brought up to date on every call, so it sees your edits. When the embedding model is unavailable the search is by keyword alone and the header says so. Read the file around a result before relying on it.
