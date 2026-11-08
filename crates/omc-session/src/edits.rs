//! Making the edits a client's language services answer a rename, a code action or the move of a file with to the texts of the files on disk, without writing them, so that the `lsp` tool writes them the way it writes any rename.

use omc_tools::Renamed;
use omc_wire::{FileEdits, TextEdit};

/// The files the client's edits `files`, those of a rename, a code action or the move of a file, change, each with its whole new text made from its text on disk now and the number of edits made to it; files with no edits are left out.
///
/// # Errors
///
/// Returns why when a file cannot be read, or its edits overlap or end before they start.
pub(crate) fn renamed(files: Vec<FileEdits>) -> Result<Vec<Renamed>, String> {
    let mut changed: Vec<FileEdits> = Vec::new();
    for file in files {
        match changed.iter_mut().find(|held| held.path == file.path) {
            Some(held) => held.edits.extend(file.edits),
            None => changed.push(file),
        }
    }
    let mut renamed = Vec::with_capacity(changed.len());
    for file in changed {
        if file.edits.is_empty() {
            continue;
        }
        let text = std::fs::read_to_string(&file.path)
            .map_err(|error| format!("{} cannot be read: {error}", file.path.display()))?;
        let new = apply(&text, &file.edits)
            .ok_or_else(|| format!("the edits to {} overlap", file.path.display()))?;
        renamed.push(Renamed {
            path: file.path,
            text: new,
            edits: file.edits.len(),
        });
    }
    Ok(renamed)
}

/// `text` with `edits` made to it, each placed by where it falls in `text`; nothing when one ends before it starts or two overlap.
pub(crate) fn apply(text: &str, edits: &[TextEdit]) -> Option<String> {
    let mut placed = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = offset(text, edit.line, edit.column);
        let end = offset(text, edit.end_line, edit.end_column);
        if start > end {
            return None;
        }
        placed.push((start, end, edit.text.as_str()));
    }
    placed.sort_by_key(|(start, end, _)| (*start, *end));
    if placed.windows(2).any(|pair| pair[0].1 > pair[1].0) {
        return None;
    }
    let mut changed = text.to_owned();
    for (start, end, new) in placed.into_iter().rev() {
        changed.replace_range(start..end, new);
    }
    Some(changed)
}

/// The byte offset in `text` of character `column` of line `line`, both counted from one, a character being a Unicode scalar value; a column past the end of its line is the end of the line, before any carriage return, and a line past the end of the text is the end of the text.
fn offset(text: &str, line: u32, column: u32) -> usize {
    let mut start = 0;
    for _ in 1..line.max(1) {
        match text[start..].find('\n') {
            Some(newline) => start += newline + 1,
            None => return text.len(),
        }
    }
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |newline| start + newline);
    let content = text[start..end]
        .strip_suffix('\r')
        .unwrap_or(&text[start..end]);
    let before = usize::try_from(column.saturating_sub(1)).unwrap_or(usize::MAX);
    content
        .char_indices()
        .nth(before)
        .map_or(start + content.len(), |(index, _)| start + index)
}

#[cfg(test)]
/// Tests for making a client's rename edits.
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// An edit replacing from `(line, column)` to `(end_line, end_column)` with `text`.
    fn edit(line: u32, column: u32, end_line: u32, end_column: u32, text: &str) -> TextEdit {
        TextEdit {
            line,
            column,
            end_line,
            end_column,
            text: text.to_owned(),
        }
    }

    /// Edits are placed by character, a character outside the basic plane counting once, wherever they come in the list, and overlapping ones are refused.
    #[test]
    fn places_edits_by_character() {
        let text = "let 🦀 = old;\nold(🦀);\r\n";
        let edits = [edit(2, 1, 2, 4, "new"), edit(1, 9, 1, 12, "new")];
        assert_eq!(
            apply(text, &edits).as_deref(),
            Some("let 🦀 = new;\nnew(🦀);\r\n")
        );
        assert_eq!(
            apply("ab\r\n", &[edit(1, 3, 1, 99, "c")]).as_deref(),
            Some("abc\r\n")
        );
        assert_eq!(
            apply(text, &[edit(1, 1, 1, 5, ""), edit(1, 3, 1, 6, "")]),
            None
        );
        assert_eq!(apply(text, &[edit(2, 3, 1, 1, "")]), None);
    }

    /// The edits of a file named twice are made together to its text on disk, which is left as it was.
    #[test]
    fn makes_the_edits_to_the_text_on_disk_without_writing() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/client-rename");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("lib.rs");
        std::fs::write(&file, "fn old() {}\nfn main() { old(); }\n").unwrap();
        let files = vec![
            FileEdits {
                path: file.clone(),
                edits: vec![edit(1, 4, 1, 7, "new")],
            },
            FileEdits {
                path: file.clone(),
                edits: vec![edit(2, 13, 2, 16, "new")],
            },
            FileEdits {
                path: dir.join("untouched.rs"),
                edits: Vec::new(),
            },
        ];
        let renamed = renamed(files).unwrap();
        assert_eq!(renamed.len(), 1);
        assert_eq!(renamed[0].text, "fn new() {}\nfn main() { new(); }\n");
        assert_eq!(renamed[0].edits, 2);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "fn old() {}\nfn main() { old(); }\n"
        );
        assert!(
            super::renamed(vec![FileEdits {
                path: dir.join("missing.rs"),
                edits: vec![edit(1, 1, 1, 1, "x")],
            }])
            .is_err()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
