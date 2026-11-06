//! Outlines: what a bare `read` of a large code file returns in place of its
//! text, with the bodies of functions folded away and every shown line keeping
//! its real number.

use std::path::Path;

use ast_grep_core::Node;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc};
use ast_grep_language::SupportLang;

use super::super::ast::detect;
use super::super::snapshot::outline_tag;
use super::lines::{MAX_BYTES, MAX_LINES, clip};
use super::selector;

/// The fewest lines a file must have to be outlined; shorter files are read whole.
pub(crate) const MIN_LINES: usize = 200;

/// The fewest lines a function body must hide to be folded, since its
/// placeholder takes a line of its own.
pub(crate) const MIN_FOLD_LINES: usize = 6;

/// The outline is only given when it hides at least one line in this many of
/// the file; otherwise folding saves too little and the file is read whole.
const MIN_HIDDEN_SHARE: usize = 4;

/// The largest file outlined, in bytes; larger files are read whole, a window at a time.
const MAX_SOURCE_BYTES: u64 = 4 * 1024 * 1024;

/// A run of lines an outline hides, 1-indexed and inclusive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Fold {
    /// The first hidden line.
    pub(crate) first: usize,
    /// The last hidden line.
    pub(crate) last: usize,
}

impl Fold {
    /// How many lines the fold hides.
    fn len(self) -> usize {
        self.last + 1 - self.first
    }
}

/// Reads the code file at `path`, shown as `label`, as an outline: its tag,
/// then its lines with the bodies of functions folded, then a footer naming
/// the folded ranges and how to read them.
///
/// Returns `None` when the file should be read whole instead: its language has
/// no outline, it is too short, too large or not UTF-8, it does not parse
/// cleanly, or folding would hide too little of it.
///
/// # Errors
///
/// Returns the I/O error when the file's metadata or contents cannot be read.
pub(super) fn read(path: &Path, label: &str) -> std::io::Result<Option<String>> {
    if detect(path).is_none_or(|lang| !outlined(lang))
        || std::fs::metadata(path)?.len() > MAX_SOURCE_BYTES
    {
        return Ok(None);
    }
    let bytes = std::fs::read(path)?;
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return Ok(None);
    };
    Ok(folds(path, text).map(|folds| {
        format!(
            "[tag {}]\n{}",
            outline_tag(&bytes),
            render(text, &folds, label)
        )
    }))
}

/// The lines an outline of `text`, the contents of the file at `path`, hides,
/// in order; `None` when the file is not outlined but read whole.
///
/// This depends only on the file's name and contents, so `edit` can work out
/// again which lines an outline it issued a tag for left out.
pub(crate) fn folds(path: &Path, text: &str) -> Option<Vec<Fold>> {
    let lang = detect(path).filter(|lang| outlined(*lang))?;
    let total = text.split_inclusive('\n').count();
    if total < MIN_LINES || text.len() as u64 > MAX_SOURCE_BYTES {
        return None;
    }
    let parsed = lang.ast_grep(text);
    let root = parsed.root();
    if root.dfs().any(|node| node.is_error() || node.is_missing()) {
        return None;
    }
    let mut found = Vec::new();
    let mut pending = vec![root];
    while let Some(node) = pending.pop() {
        match foldable(lang, &node) {
            Some(fold) => found.push(fold),
            None => {
                let children: Vec<_> = node.children().collect();
                pending.extend(children.into_iter().rev());
            }
        }
    }
    found.sort_by_key(|fold| fold.first);
    let hidden: usize = found.iter().map(|fold| fold.len()).sum();
    (hidden * MIN_HIDDEN_SHARE >= total).then_some(found)
}

/// Whether files in `lang` are outlined.
fn outlined(lang: SupportLang) -> bool {
    matches!(
        lang,
        SupportLang::C
            | SupportLang::Cpp
            | SupportLang::Go
            | SupportLang::Java
            | SupportLang::JavaScript
            | SupportLang::Lua
            | SupportLang::Python
            | SupportLang::Rust
            | SupportLang::Tsx
            | SupportLang::TypeScript
    )
}

/// The node kinds of `lang` that are functions or methods with a body.
fn function_kinds(lang: SupportLang) -> &'static [&'static str] {
    match lang {
        SupportLang::Rust => &["function_item"],
        SupportLang::Go => &["function_declaration", "method_declaration", "func_literal"],
        SupportLang::JavaScript | SupportLang::TypeScript | SupportLang::Tsx => &[
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
            "function_expression",
            "function",
            "generator_function",
            "arrow_function",
        ],
        SupportLang::Python => &["function_definition"],
        SupportLang::C | SupportLang::Cpp => &["function_definition"],
        SupportLang::Java => &["method_declaration", "constructor_declaration"],
        SupportLang::Lua => &["function_declaration", "function_definition"],
        _ => &[],
    }
}

/// Whether a node of `kind` in `lang` is a function without a name, the kind
/// passed to a call as a callback.
fn anonymous(lang: SupportLang, kind: &str) -> bool {
    match lang {
        SupportLang::JavaScript | SupportLang::TypeScript | SupportLang::Tsx => matches!(
            kind,
            "arrow_function" | "function_expression" | "function" | "generator_function"
        ),
        SupportLang::Go => kind == "func_literal",
        SupportLang::Lua => kind == "function_definition",
        _ => false,
    }
}

/// The node kinds that hold the statements of a function body.
const BODY_KINDS: [&str; 4] = [
    "block",
    "statement_block",
    "compound_statement",
    "constructor_body",
];

/// The fold of the body of `node` when it is a function whose body hides at
/// least [`MIN_FOLD_LINES`] lines, and is not a callback whose body is mostly
/// further calls with callbacks, such as a test suite, which is outlined
/// inside instead.
fn foldable(lang: SupportLang, node: &Node<'_, StrDoc<SupportLang>>) -> Option<Fold> {
    let kind = node.kind();
    if !function_kinds(lang).contains(&kind.as_ref()) {
        return None;
    }
    if anonymous(lang, &kind) && passes_callbacks(lang, node) {
        return None;
    }
    let body = node.field("body")?;
    if !BODY_KINDS.contains(&body.kind().as_ref()) {
        return None;
    }
    let (first, last) = hidden(lang, node, &body)?;
    let fold = Fold {
        first: first + 1,
        last: last + 1,
    };
    (last >= first && fold.len() >= MIN_FOLD_LINES).then_some(fold)
}

/// The first and last 0-indexed lines of the body `body` of the function
/// `function` that an outline hides: the lines between the braces of a braced
/// body, or the lines of the statements of an indented or keyword-closed body,
/// after a Python docstring and never on the line of the signature or of a
/// closing keyword. `None` when no whole line is left to hide.
fn hidden(
    lang: SupportLang,
    function: &Node<'_, StrDoc<SupportLang>>,
    body: &Node<'_, StrDoc<SupportLang>>,
) -> Option<(usize, usize)> {
    let open = body.start_pos().line();
    let close = last_line(body);
    if body.text().starts_with('{') {
        return (close > open + 1).then(|| (open + 1, close - 1));
    }
    let header = body
        .prev()
        .map_or(function.start_pos().line(), |prev| last_line(&prev));
    let mut first = open.max(header + 1);
    if lang == SupportLang::Python
        && let Some(docstring) = docstring(body)
    {
        first = first.max(last_line(&docstring) + 1);
    }
    let mut last = close;
    if last_line(function) == close && function.range().end > body.range().end {
        last = close.checked_sub(1)?;
    }
    (last >= first).then_some((first, last))
}

/// The 0-indexed line `node` ends on, not counting a line it only reaches the start of.
fn last_line(node: &Node<'_, StrDoc<SupportLang>>) -> usize {
    let end = node.end_pos();
    let (line, column) = end.byte_point();
    if column == 0 && line > node.start_pos().line() {
        line - 1
    } else {
        line
    }
}

/// The docstring that opens the Python block `body`, when it has one.
fn docstring<'t>(body: &Node<'t, StrDoc<SupportLang>>) -> Option<Node<'t, StrDoc<SupportLang>>> {
    let first = body.named_children().next()?;
    let only = first.named_children().next()?;
    (first.kind() == "expression_statement" && only.kind() == "string").then_some(first)
}

/// Whether the body of the function `function`, written in `lang`, holds a
/// statement that calls something with a function of its own as an argument,
/// as `describe` and `t.Run` blocks do.
fn passes_callbacks(lang: SupportLang, function: &Node<'_, StrDoc<SupportLang>>) -> bool {
    let Some(body) = function.field("body") else {
        return false;
    };
    statements(&body).iter().any(|statement| {
        let call = if statement.kind() == "expression_statement" {
            statement.named_children().next()
        } else {
            Some(statement.clone())
        };
        call.filter(|call| matches!(call.kind().as_ref(), "call_expression" | "function_call"))
            .and_then(|call| call.field("arguments"))
            .is_some_and(|arguments| {
                arguments
                    .named_children()
                    .any(|argument| anonymous(lang, &argument.kind()))
            })
    })
}

/// The statements of the body `body`, looking through the statement list a
/// Go block wraps them in.
fn statements<'t>(body: &Node<'t, StrDoc<SupportLang>>) -> Vec<Node<'t, StrDoc<SupportLang>>> {
    body.named_children()
        .flat_map(|child| {
            if child.kind() == "statement_list" {
                child.named_children().collect()
            } else {
                vec![child]
            }
        })
        .collect()
}

/// Renders the outline of `text`, shown as `label`, that hides `folds`: a
/// header, the numbered lines with a placeholder for each fold, and a footer
/// naming the folded ranges and how to read them.
///
/// The output stops at [`MAX_LINES`] lines or [`MAX_BYTES`] bytes like any
/// read, with a note saying where to continue; the footer then only names the
/// folds before that point.
fn render(text: &str, folds: &[Fold], label: &str) -> String {
    let lines: Vec<&str> = text
        .split_inclusive('\n')
        .map(|line| line.trim_end_matches(['\n', '\r']))
        .collect();
    let hidden: usize = folds.iter().map(|fold| fold.len()).sum();
    let mut out = format!(
        "[outline of {label}, {} lines: {} function bodies folded, {hidden} lines hidden]\n",
        lines.len(),
        folds.len()
    );
    let start = out.len();
    let mut shown = 0usize;
    let mut listed = Vec::new();
    let mut folds = folds.iter().peekable();
    let mut line_number = 1usize;
    while line_number <= lines.len() {
        if shown >= MAX_LINES || out.len() - start >= MAX_BYTES {
            out.push_str(&format!(
                "[more lines remain; continue with {label}:{line_number}]\n"
            ));
            break;
        }
        if let Some(fold) = folds.next_if(|fold| fold.first == line_number) {
            let line = lines[fold.first - 1];
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push_str(&format!(
                "      \t{indent}… {} lines folded ({}-{})\n",
                fold.len(),
                fold.first,
                fold.last
            ));
            listed.push((fold.first, Some(fold.last)));
            line_number = fold.last + 1;
        } else {
            out.push_str(&format!(
                "{line_number:>6}\t{}\n",
                clip(lines[line_number - 1])
            ));
            line_number += 1;
        }
        shown += 1;
    }
    let Some(first) = listed.first() else {
        return out;
    };
    let ranges = selector::render(&listed);
    let example = selector::render(std::slice::from_ref(first));
    out.push_str(&format!(
        "[folded: {ranges}. Read a folded range, such as {label}:{example}, before relying on \
         or editing what it holds: an edit with this outline's tag that touches a folded line \
         is refused. {label}:1- reads the whole text.]\n"
    ));
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A Rust file of `functions` functions with bodies of `body` lines each,
    /// preceded by a doc comment, after a `use` line.
    pub(crate) fn rust_source(functions: usize, body: usize) -> String {
        let mut code = String::from("use std::fmt;\n\n");
        for index in 0..functions {
            code.push_str(&format!(
                "/// Function {index}.\npub fn f{index}(x: u32) -> u32 {{\n"
            ));
            for step in 0..body {
                code.push_str(&format!("    let v{step} = x + {step};\n"));
            }
            code.push_str("    x\n}\n\n");
        }
        code
    }

    /// The folds of `code` read as a file called `name`.
    fn fold(name: &str, code: &str) -> Option<Vec<Fold>> {
        folds(Path::new(name), code)
    }

    /// Function bodies of Rust are folded between their braces, keeping signatures, docs and imports.
    #[test]
    fn folds_rust_function_bodies() {
        let code = rust_source(20, 10);
        let found = fold("a.rs", &code).unwrap();
        assert_eq!(found.len(), 20);
        assert_eq!(found[0], Fold { first: 5, last: 15 });
        let out = render(&code, &found, "a.rs");
        assert!(out.starts_with("[outline of a.rs, 302 lines: 20 function bodies folded"));
        assert!(out.contains("     1\tuse std::fmt;\n"), "{out}");
        assert!(out.contains("     3\t/// Function 0.\n"), "{out}");
        assert!(
            out.contains("     4\tpub fn f0(x: u32) -> u32 {\n      \t    … 11 lines folded (5-15)\n    16\t}\n"),
            "{out}"
        );
        assert!(out.contains("[folded: 5-15,20-30,"), "{out}");
        assert!(out.contains("such as a.rs:5-15, before"), "{out}");
        assert!(!out.contains("let v3"), "{out}");
    }

    /// Short files, short bodies, unknown languages and broken code are not outlined.
    #[test]
    fn leaves_some_files_whole() {
        assert_eq!(fold("a.rs", &rust_source(3, 10)), None, "too short");
        assert_eq!(fold("a.rs", &rust_source(80, 2)), None, "bodies too short");
        assert_eq!(fold("a.txt", &rust_source(20, 10)), None, "not code");
        let broken = format!("{}fn broken( {{\n", rust_source(20, 10));
        assert_eq!(fold("a.rs", &broken), None, "does not parse");
    }

    /// Methods inside impl blocks are folded while the impl header and closing brace stay.
    #[test]
    fn keeps_impl_and_trait_structure() {
        let mut code = String::from("pub struct S;\n\nimpl S {\n");
        for index in 0..20 {
            code.push_str(&format!("    pub fn m{index}(&self) {{\n"));
            code.push_str(&"        work();\n".repeat(9));
            code.push_str("    }\n");
        }
        code.push_str("}\n");
        let found = fold("s.rs", &code).unwrap();
        assert_eq!(found.len(), 20);
        let out = render(&code, &found, "s.rs");
        assert!(
            out.contains("     3\timpl S {\n     4\t    pub fn m0(&self) {\n"),
            "{out}"
        );
        assert!(out.contains("        … 9 lines folded (5-13)\n"), "{out}");
    }

    /// Python bodies are folded after the signature and the docstring.
    #[test]
    fn folds_python_after_docstrings() {
        let mut code = String::from("import os\n\n\nclass Thing:\n");
        for index in 0..20 {
            code.push_str(&format!(
                "    def m{index}(self, x):\n        \"\"\"Does {index}.\"\"\"\n"
            ));
            code.push_str(&"        x = os.getcwd()\n".repeat(8));
            code.push_str("        return x\n\n");
        }
        let found = fold("t.py", &code).unwrap();
        assert_eq!(found[0], Fold { first: 7, last: 15 });
        let out = render(&code, &found, "t.py");
        assert!(out.contains("     4\tclass Thing:\n     5\t    def m0(self, x):\n     6\t        \"\"\"Does 0.\"\"\"\n      \t        … 9 lines folded (7-15)\n"), "{out}");
    }

    /// Go, TypeScript, C and Lua functions are folded too.
    #[test]
    fn folds_other_languages() {
        let body = |line: &str| line.repeat(8);
        let mut go = String::from("package main\n\n");
        let mut ts = String::from("import { x } from \"y\";\n\n");
        let mut c = String::from("#include <stdio.h>\n\n");
        let mut lua = String::from("local M = {}\n\n");
        for index in 0..20 {
            go.push_str(&format!(
                "func F{index}() int {{\n{}\treturn 1\n}}\n\n",
                body("\tprintln(1)\n")
            ));
            ts.push_str(&format!(
                "export function f{index}(): number {{\n{}  return 1;\n}}\n\n",
                body("  x();\n")
            ));
            c.push_str(&format!(
                "int f{index}(void)\n{{\n{}    return 1;\n}}\n\n",
                body("    puts(\"x\");\n")
            ));
            lua.push_str(&format!(
                "function M.f{index}()\n{}  return 1\nend\n\n",
                body("  print(1)\n")
            ));
        }
        for (name, code, first) in [
            ("a.go", &go, 4),
            ("a.ts", &ts, 4),
            ("a.c", &c, 5),
            ("a.lua", &lua, 4),
        ] {
            let found = fold(name, code).unwrap_or_else(|| panic!("{name} not outlined"));
            assert_eq!(found.len(), 20, "{name}");
            assert_eq!(
                found[0],
                Fold {
                    first,
                    last: first + 8
                },
                "{name}"
            );
        }
    }

    /// A callback full of calls with callbacks, such as a test suite, is outlined inside rather than folded whole.
    #[test]
    fn opens_suites_of_callbacks() {
        let mut code = String::from("describe(\"suite\", () => {\n");
        for index in 0..20 {
            code.push_str(&format!("  it(\"case {index}\", () => {{\n"));
            code.push_str(&"    expect(1).toBe(1);\n".repeat(8));
            code.push_str("  });\n");
        }
        code.push_str("});\n");
        let found = fold("a.test.js", &code).unwrap();
        assert_eq!(found.len(), 20);
        let out = render(&code, &found, "a.test.js");
        assert!(out.contains("     2\t  it(\"case 0\", () => {\n"), "{out}");
    }

    /// A large outline stops like a long read, saying where to continue.
    #[test]
    fn caps_long_outlines() {
        let code = rust_source(1200, 6);
        let found = fold("big.rs", &code).unwrap();
        let out = render(&code, &found, "big.rs");
        assert!(
            out.contains("[more lines remain; continue with big.rs:"),
            "{out:.200}"
        );
        assert!(out.matches('\n').count() <= MAX_LINES + 3);
    }
}
