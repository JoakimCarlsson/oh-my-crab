//! Cutting a file into the pieces the index keeps.
//!
//! Code in a language with a built-in grammar is cut along its syntax tree:
//! the top-level items are packed together, in order, into chunks of at most
//! [`MAX_LINES`] lines, and an item longer than that is cut along its own
//! items in the same way, down to items that hold no others, which are cut
//! into windows of lines. So a chunk is usually one definition with the
//! comments before it, or a run of small ones. Other files are cut into
//! overlapping windows of lines.

use std::path::Path;

use ast_grep_core::tree_sitter::{LanguageExt, StrDoc};
use ast_grep_core::{Language, Node};
use ast_grep_language::SupportLang;

/// The most lines a chunk packs together.
pub const MAX_LINES: usize = 40;

/// How many lines a window moves on from the one before, so that windows of plain text overlap by `MAX_LINES - WINDOW_STEP` lines.
const WINDOW_STEP: usize = 30;

/// The most bytes of a chunk's text that are kept, so a minified line does not make one huge chunk.
pub const MAX_CHUNK_BYTES: usize = 4000;

/// The languages whose grammars cut code along its syntax tree.
const LANGUAGES: &[SupportLang] = &[
    SupportLang::C,
    SupportLang::Cpp,
    SupportLang::Css,
    SupportLang::Go,
    SupportLang::Html,
    SupportLang::Java,
    SupportLang::JavaScript,
    SupportLang::Json,
    SupportLang::Lua,
    SupportLang::Python,
    SupportLang::Rust,
    SupportLang::Tsx,
    SupportLang::TypeScript,
];

/// One piece of a file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// The first line it covers, counting from 1.
    pub start: usize,
    /// The last line it covers, counting from 1.
    pub end: usize,
    /// Its text, cut to [`MAX_CHUNK_BYTES`].
    pub text: String,
}

/// A span of lines, counting from 0, both ends included.
type Span = (usize, usize);

/// The chunks of the file at `path` whose text is `text`, in order, leaving out those that are only white space.
pub fn chunks(path: &Path, text: &str) -> Vec<Chunk> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.is_empty() {
        return Vec::new();
    }
    let spans = match grammar(path) {
        Some(lang) => syntax_spans(lang, text, lines.len()),
        None => windows(0, lines.len() - 1, WINDOW_STEP),
    };
    spans
        .into_iter()
        .filter_map(|(start, end)| chunk(&lines, start, end))
        .collect()
}

/// The built-in grammar for the file at `path`, judged by its extension.
fn grammar(path: &Path) -> Option<SupportLang> {
    SupportLang::from_path(path).filter(|lang| LANGUAGES.contains(lang))
}

/// The chunk covering the lines `start` to `end` of `lines`, or nothing when they hold only white space.
fn chunk(lines: &[&str], start: usize, end: usize) -> Option<Chunk> {
    let end = end.min(lines.len().saturating_sub(1));
    let covered = lines.get(start..=end)?;
    if covered.iter().all(|line| line.trim().is_empty()) {
        return None;
    }
    let mut text = covered.join("\n");
    if text.len() > MAX_CHUNK_BYTES {
        let mut cut = MAX_CHUNK_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    Some(Chunk {
        start: start + 1,
        end: end + 1,
        text,
    })
}

/// The spans of `text`, parsed as `lang` and `count` lines long, cut along its syntax tree; windows of lines when it has no items.
fn syntax_spans(lang: SupportLang, text: &str, count: usize) -> Vec<Span> {
    let parsed = lang.ast_grep(text);
    let root = parsed.root();
    let items = named_children(&root);
    if items.is_empty() {
        return windows(0, count - 1, WINDOW_STEP);
    }
    pack(&items)
}

/// The named children of `node`, in order.
fn named_children<'t>(node: &Node<'t, StrDoc<SupportLang>>) -> Vec<Node<'t, StrDoc<SupportLang>>> {
    node.children().filter(Node::is_named).collect()
}

/// The lines `node` covers.
fn span_of(node: &Node<'_, StrDoc<SupportLang>>) -> Span {
    (node.start_pos().line(), node.end_pos().line())
}

/// Whether `node` is a comment, which belongs with the item after it.
fn is_comment(node: &Node<'_, StrDoc<SupportLang>>) -> bool {
    node.kind().contains("comment")
}

/// `items`, siblings in order, each with the lines it covers together with the comments right before it; comments with no item after them make one last unit of their own.
fn units<'t>(
    items: &[Node<'t, StrDoc<SupportLang>>],
) -> Vec<(Span, Node<'t, StrDoc<SupportLang>>)> {
    let mut units = Vec::new();
    let mut comments: Option<usize> = None;
    for item in items {
        let (start, end) = span_of(item);
        if is_comment(item) {
            comments.get_or_insert(start);
            continue;
        }
        units.push(((comments.take().unwrap_or(start), end), item.clone()));
    }
    if let (Some(start), Some(last)) = (comments, items.last()) {
        units.push(((start, span_of(last).1), last.clone()));
    }
    units
}

/// The spans that `items`, siblings in order, are packed into: neighbours share a span while it stays within [`MAX_LINES`], comments going with the item after them, and an item longer than that is cut along its own items, its first and last lines going to the first and last of their spans, or into windows when it has none.
fn pack(items: &[Node<'_, StrDoc<SupportLang>>]) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut current: Option<Span> = None;
    for ((start, end), item) in units(items) {
        if end - start + 1 > MAX_LINES {
            spans.extend(current.take());
            spans.extend(split(&item, start, end));
            continue;
        }
        current = match current {
            Some((first, _)) if end - first < MAX_LINES => Some((first, end)),
            Some(full) => {
                spans.push(full);
                Some((start, end))
            }
            None => Some((start, end)),
        };
    }
    spans.extend(current);
    spans
}

/// The spans of `item`, which covers the lines `start` to `end` and is too long for one: those of its own items, stretched to its first and last lines, or windows when it has none.
fn split(item: &Node<'_, StrDoc<SupportLang>>, start: usize, end: usize) -> Vec<Span> {
    let inner = named_children(item);
    let mut spans = if inner.is_empty() {
        Vec::new()
    } else {
        pack(&inner)
    };
    if spans.is_empty() {
        return windows(start, end, MAX_LINES);
    }
    if let Some(first) = spans.first_mut() {
        first.0 = first.0.min(start);
    }
    if let Some(last) = spans.last_mut() {
        last.1 = last.1.max(end);
    }
    spans
}

/// Windows of at most [`MAX_LINES`] lines over the lines `start` to `end`, each starting `step` lines after the one before.
fn windows(start: usize, end: usize, step: usize) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut at = start;
    loop {
        let last = (at + MAX_LINES - 1).min(end);
        spans.push((at, last));
        if last >= end {
            return spans;
        }
        at += step;
    }
}

#[cfg(test)]
/// Tests for cutting files into chunks.
mod tests {
    use super::*;

    /// Rust source with `functions` functions of `body` lines each, separated by blank lines.
    fn rust(functions: usize, body: usize) -> String {
        (0..functions)
            .map(|n| {
                let lines: Vec<String> =
                    (0..body).map(|i| format!("    let x{i} = {i};")).collect();
                format!(
                    "/// Function {n}.\nfn f{n}() {{\n{}\n}}\n",
                    lines.join("\n")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Small definitions share a chunk, and a chunk never starts inside one.
    #[test]
    fn packs_small_definitions_together() {
        let source = rust(3, 2);
        let found = chunks(Path::new("a.rs"), &source);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].start, 1);
        assert!(found[0].text.contains("fn f0") && found[0].text.contains("fn f2"));
    }

    /// Definitions that do not fit together get a chunk each, with their doc comment.
    #[test]
    fn keeps_definitions_whole() {
        let source = rust(3, 25);
        let found = chunks(Path::new("a.rs"), &source);
        assert_eq!(found.len(), 3);
        for (n, chunk) in found.iter().enumerate() {
            assert!(chunk.text.contains(&format!("fn f{n}()")), "{chunk:?}");
            assert!(chunk.text.trim_end().ends_with('}'));
        }
        assert!(found[1].text.starts_with("/// Function 1."));
    }

    /// A definition longer than a chunk is cut into pieces that cover it whole.
    #[test]
    fn cuts_long_definitions() {
        let source = rust(1, 100);
        let found = chunks(Path::new("a.rs"), &source);
        assert!(found.len() > 1);
        assert_eq!(found[0].start, 1);
        assert_eq!(found.last().unwrap().end, source.lines().count());
        assert!(found.iter().all(|c| c.end - c.start < MAX_LINES + 2));
    }

    /// A file without a grammar is cut into overlapping windows, and blank files give nothing.
    #[test]
    fn windows_plain_text() {
        let text: String = (1..=70).map(|n| format!("line {n}\n")).collect();
        let found = chunks(Path::new("notes.txt"), &text);
        assert_eq!(
            found.iter().map(|c| (c.start, c.end)).collect::<Vec<_>>(),
            [(1, 40), (31, 70)]
        );
        assert!(chunks(Path::new("blank.txt"), "\n\n  \n").is_empty());
        assert!(chunks(Path::new("empty.rs"), "").is_empty());
    }

    /// A long single line is cut to the byte limit on a character boundary.
    #[test]
    fn caps_chunk_size() {
        let text = "é".repeat(MAX_CHUNK_BYTES);
        let found = chunks(Path::new("min.txt"), &text);
        assert_eq!(found.len(), 1);
        assert!(found[0].text.len() <= MAX_CHUNK_BYTES);
    }
}
