//! Reading what servers answer to questions about code: places, hovers, symbols and the edits of a rename, put in the harness's terms.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use omc_tools::{Location, Renamed, Symbol};
use serde_json::Value;

use crate::client::apply_edits;
use crate::text::{column, path_of};

/// The texts of files read so far, by path, so that places in the same file are converted with one read.
#[derive(Default)]
pub(crate) struct Texts {
    /// The text of each file read, or nothing when it could not be read.
    files: HashMap<PathBuf, Option<String>>,
}

impl Texts {
    /// Texts that start with `text` as the text of the file at `path`, which has been read already.
    pub(crate) fn with(path: &Path, text: &str) -> Self {
        let mut texts = Self::default();
        texts
            .files
            .insert(path.to_path_buf(), Some(text.to_owned()));
        texts
    }

    /// The text of the file at `path`, read from disk the first time; nothing when it cannot be read.
    pub(crate) fn of(&mut self, path: &Path) -> Option<&str> {
        self.files
            .entry(path.to_path_buf())
            .or_insert_with(|| std::fs::read_to_string(path).ok())
            .as_deref()
    }

    /// The character, counted from one, that the position `place` of the protocol, a line and a count of UTF-16 code units both from nought, falls on in the file at `path`, with its line counted from one.
    fn place(&mut self, path: &Path, place: &Value) -> Option<(u32, u32)> {
        let line = u32::try_from(place["line"].as_u64()?).ok()?;
        let character = u32::try_from(place["character"].as_u64()?).ok()?;
        let column = match self.of(path) {
            Some(text) => column(text, line, character),
            None => character + 1,
        };
        Some((line + 1, column))
    }

    /// The stretch `range` of the protocol in the file at `path`.
    fn location(&mut self, path: &Path, range: &Value) -> Option<Location> {
        let (line, column) = self.place(path, &range["start"])?;
        let (end_line, end_column) = self.place(path, &range["end"]).unwrap_or((line, column));
        Some(Location {
            path: path.to_path_buf(),
            line,
            column,
            end_line,
            end_column,
        })
    }
}

/// The places in a server's answer to a definition, implementation or references request: nothing, one location, or a list of locations or location links.
pub(crate) fn locations(answer: &Value, texts: &mut Texts) -> Vec<Location> {
    let items: Vec<&Value> = match answer {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![answer],
        _ => Vec::new(),
    };
    items
        .into_iter()
        .filter_map(|item| {
            let (uri, range) = match item.get("targetUri") {
                Some(uri) => (
                    uri,
                    item.get("targetSelectionRange")
                        .unwrap_or(&item["targetRange"]),
                ),
                None => (&item["uri"], &item["range"]),
            };
            let path = path_of(uri.as_str()?)?;
            texts.location(&path, range)
        })
        .collect()
}

/// The text of a server's answer to a hover request, its parts joined by blank lines, code in fenced blocks; nothing when it says nothing.
pub(crate) fn hover(answer: &Value) -> Option<String> {
    /// The text of one part of a hover's contents.
    fn part(content: &Value) -> Option<String> {
        match content {
            Value::String(text) => Some(text.clone()),
            Value::Object(object) => {
                let value = object.get("value")?.as_str()?;
                match object.get("language").and_then(Value::as_str) {
                    Some(language) => Some(format!("```{language}\n{value}\n```")),
                    None => Some(value.to_owned()),
                }
            }
            _ => None,
        }
    }
    let contents = answer.get("contents")?;
    let parts: Vec<String> = match contents {
        Value::Array(items) => items.iter().filter_map(part).collect(),
        other => part(other).into_iter().collect(),
    };
    let text = parts
        .into_iter()
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    (!text.is_empty()).then_some(text)
}

/// The name of the symbol kind numbered `kind` in the protocol, in lower case.
fn kind_name(kind: Option<u64>) -> &'static str {
    const KINDS: [&str; 26] = [
        "file",
        "module",
        "namespace",
        "package",
        "class",
        "method",
        "property",
        "field",
        "constructor",
        "enum",
        "interface",
        "function",
        "variable",
        "constant",
        "string",
        "number",
        "boolean",
        "array",
        "object",
        "key",
        "null",
        "enum member",
        "struct",
        "event",
        "operator",
        "type parameter",
    ];
    kind.and_then(|kind| usize::try_from(kind).ok())
        .and_then(|kind| kind.checked_sub(1))
        .and_then(|index| KINDS.get(index))
        .copied()
        .unwrap_or("symbol")
}

/// The symbols in a server's answer to a document symbol request about the file at `path`: a tree of document symbols, flattened with each symbol followed by those inside it, or a flat list of symbol information.
pub(crate) fn document_symbols(answer: &Value, path: &Path, texts: &mut Texts) -> Vec<Symbol> {
    let mut found = Vec::new();
    if let Some(items) = answer.as_array() {
        for item in items {
            if item.get("selectionRange").is_some() || item.get("children").is_some() {
                nested(item, path, 0, None, texts, &mut found);
            } else if let Some(symbol) = information(item, texts) {
                found.push(symbol);
            }
        }
    }
    found
}

/// Adds the document symbol `item` of the file at `path`, nested `depth` deep inside `container`, and the symbols inside it, to `found`.
fn nested(
    item: &Value,
    path: &Path,
    depth: usize,
    container: Option<&str>,
    texts: &mut Texts,
    found: &mut Vec<Symbol>,
) {
    let Some(name) = item["name"].as_str() else {
        return;
    };
    let range = item.get("selectionRange").unwrap_or(&item["range"]);
    if let Some(location) = texts.location(path, range) {
        found.push(Symbol {
            name: name.to_owned(),
            kind: kind_name(item["kind"].as_u64()).to_owned(),
            detail: item["detail"].as_str().map(str::to_owned),
            container: container.map(str::to_owned),
            depth,
            location,
        });
    }
    for child in item["children"].as_array().into_iter().flatten() {
        nested(child, path, depth + 1, Some(name), texts, found);
    }
}

/// The symbol a server describes as `item`, symbol information or a workspace symbol, whose location may name only its file.
fn information(item: &Value, texts: &mut Texts) -> Option<Symbol> {
    let location = &item["location"];
    let path = path_of(location["uri"].as_str()?)?;
    let location = match location.get("range") {
        Some(range) => texts.location(&path, range)?,
        None => Location {
            path,
            line: 1,
            column: 1,
            end_line: 1,
            end_column: 1,
        },
    };
    Some(Symbol {
        name: item["name"].as_str()?.to_owned(),
        kind: kind_name(item["kind"].as_u64()).to_owned(),
        detail: None,
        container: item["containerName"].as_str().map(str::to_owned),
        depth: 0,
        location,
    })
}

/// The symbols in a server's answer to a workspace symbol request.
pub(crate) fn workspace_symbols(answer: &Value, texts: &mut Texts) -> Vec<Symbol> {
    answer
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| information(item, texts))
        .collect()
}

/// The files a server's answer to a rename request changes, each with its whole new text, made from its text on disk now.
///
/// # Errors
///
/// Returns why when the answer would create, move or delete files, names a file that is not on disk, or holds edits that overlap.
pub(crate) fn renamed(answer: &Value) -> Result<Vec<Renamed>, String> {
    let mut by_file: Vec<(String, Vec<Value>)> = Vec::new();
    let mut add =
        |uri: &str, edits: &[Value]| match by_file.iter_mut().find(|(seen, _)| seen == uri) {
            Some((_, held)) => held.extend(edits.iter().cloned()),
            None => by_file.push((uri.to_owned(), edits.to_vec())),
        };
    if let Some(changes) = answer["documentChanges"].as_array() {
        for change in changes {
            if change.get("kind").is_some() {
                return Err(format!(
                    "the rename would also {} a file, which is not done here; rename the file with bash, then the symbol",
                    change["kind"].as_str().unwrap_or("change")
                ));
            }
            let Some(uri) = change["textDocument"]["uri"].as_str() else {
                continue;
            };
            add(uri, change["edits"].as_array().map_or(&[], Vec::as_slice));
        }
    } else if let Some(changes) = answer["changes"].as_object() {
        for (uri, edits) in changes {
            add(uri, edits.as_array().map_or(&[], Vec::as_slice));
        }
    }
    let mut files = Vec::with_capacity(by_file.len());
    for (uri, edits) in by_file {
        if edits.is_empty() {
            continue;
        }
        let path =
            path_of(&uri).ok_or_else(|| format!("the rename names {uri}, which is not a file"))?;
        let text = std::fs::read_to_string(&path)
            .map_err(|error| format!("{} cannot be read: {error}", path.display()))?;
        let new = apply_edits(&text, &edits)
            .ok_or_else(|| format!("the rename's edits to {} overlap", path.display()))?;
        files.push(Renamed {
            path,
            text: new,
            edits: edits.len(),
        });
    }
    Ok(files)
}

#[cfg(test)]
/// Tests for reading answers about code.
mod tests {
    use serde_json::json;

    use super::*;
    use crate::text::file_uri;

    /// A scratch folder named `name` under the workspace's git-ignored `build/`.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-lsp")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// Locations and location links are read alike, placed by character in their files.
    #[test]
    fn reads_locations_and_links() {
        let path = PathBuf::from("/w/a.rs");
        let mut texts = Texts::with(&path, "let é = 1;\nfn f() {}\n");
        let uri = file_uri(&path);
        let range =
            json!({"start": {"line": 0, "character": 4}, "end": {"line": 0, "character": 5}});
        let answer = json!([
            {"uri": uri, "range": range},
            {"targetUri": uri, "targetRange": range, "targetSelectionRange": {"start": {"line": 1, "character": 3}, "end": {"line": 1, "character": 4}}},
        ]);
        let found = locations(&answer, &mut texts);
        assert_eq!(
            (found[0].line, found[0].column, found[0].end_column),
            (1, 5, 6)
        );
        assert_eq!((found[1].line, found[1].column), (2, 4));
        assert!(locations(&Value::Null, &mut texts).is_empty());
    }

    /// Hover contents of every shape become text, code fenced.
    #[test]
    fn reads_hovers() {
        assert_eq!(
            hover(&json!({"contents": {"kind": "markdown", "value": "```rust\nfn f()\n```"}}))
                .as_deref(),
            Some("```rust\nfn f()\n```")
        );
        assert_eq!(
            hover(&json!({"contents": [{"language": "go", "value": "func F()"}, "Does things."]}))
                .as_deref(),
            Some("```go\nfunc F()\n```\n\nDoes things.")
        );
        assert_eq!(hover(&json!({"contents": ""})), None);
        assert_eq!(hover(&Value::Null), None);
    }

    /// A tree of document symbols is flattened with depths and containers.
    #[test]
    fn flattens_document_symbols() {
        let path = PathBuf::from("/w/a.rs");
        let mut texts = Texts::with(&path, "struct S {\n    x: u32,\n}\n");
        let range = |line, character| json!({"start": {"line": line, "character": character}, "end": {"line": line, "character": character + 1}});
        let answer = json!([{
            "name": "S", "kind": 23, "range": range(0, 0), "selectionRange": range(0, 7),
            "children": [{"name": "x", "kind": 8, "detail": "u32", "range": range(1, 4), "selectionRange": range(1, 4)}]
        }]);
        let found = document_symbols(&answer, &path, &mut texts);
        assert_eq!(found.len(), 2);
        assert_eq!(
            (found[0].kind.as_str(), found[0].location.column),
            ("struct", 8)
        );
        assert_eq!(
            (found[1].depth, found[1].container.as_deref()),
            (1, Some("S"))
        );
        assert_eq!(found[1].detail.as_deref(), Some("u32"));
    }

    /// A workspace edit's text edits are made to the files on disk; one that moves files is refused.
    #[test]
    fn makes_the_edits_of_a_rename() {
        let dir = scratch("navigate-rename");
        let (a, b) = (dir.join("a.rs"), dir.join("b.rs"));
        std::fs::write(&a, "fn old() {}\n").unwrap();
        std::fs::write(&b, "use crate::old;\nold();\n").unwrap();
        let edit = |line, start, end| json!({"range": {"start": {"line": line, "character": start}, "end": {"line": line, "character": end}}, "newText": "new"});
        let answer = json!({"documentChanges": [
            {"textDocument": {"uri": file_uri(&a), "version": 1}, "edits": [edit(0, 3, 6)]},
            {"textDocument": {"uri": file_uri(&b), "version": null}, "edits": [edit(0, 11, 14), edit(1, 0, 3)]},
        ]});
        let files = renamed(&answer).unwrap();
        assert_eq!(files[0].text, "fn new() {}\n");
        assert_eq!(
            (files[1].text.as_str(), files[1].edits),
            ("use crate::new;\nnew();\n", 2)
        );
        let changes = json!({"changes": {file_uri(&a): [edit(0, 3, 6)]}});
        assert_eq!(renamed(&changes).unwrap()[0].text, "fn new() {}\n");
        let moving = json!({"documentChanges": [{"kind": "rename", "oldUri": file_uri(&a), "newUri": file_uri(&b)}]});
        assert!(renamed(&moving).unwrap_err().contains("rename a file"));
    }
}
