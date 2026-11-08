//! Writing what language servers answer as short text a model reads: places as `file:line:column` with the line they point at, outlines of symbols, and diagnostics.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use crate::language::message;
use crate::{CodeAction, Diagnostic, Location, Sandbox, ServerStatus, Severity, Symbol};

/// The most places a lookup shows.
const MAX_LOCATIONS: usize = 100;

/// The most symbols a file's outline shows.
const MAX_DOCUMENT_SYMBOLS: usize = 300;

/// The most symbols a project search shows.
const MAX_WORKSPACE_SYMBOLS: usize = 100;

/// The most diagnostics shown.
const MAX_DIAGNOSTICS: usize = 100;

/// The most characters of a line quoted after a place.
const MAX_EXCERPT: usize = 160;

/// The most bytes of a hover shown.
const MAX_HOVER: usize = 4000;

/// The lines of files read so far, by path, so that each file is read once however many places are in it.
#[derive(Default)]
pub(super) struct Lines {
    /// The lines of each file read, or nothing when it could not be read.
    files: HashMap<PathBuf, Option<Vec<String>>>,
}

impl Lines {
    /// Line `line`, counted from one, of the file at `path`, trimmed and cut to [`MAX_EXCERPT`] characters; empty when the file or the line cannot be read.
    pub(super) fn excerpt(&mut self, path: &Path, line: u32) -> String {
        let lines = self.files.entry(path.to_path_buf()).or_insert_with(|| {
            std::fs::read_to_string(path)
                .ok()
                .map(|text| text.lines().map(str::to_owned).collect())
        });
        let Some(text) = lines.as_ref().and_then(|lines| {
            usize::try_from(line)
                .ok()
                .and_then(|line| line.checked_sub(1))
                .and_then(|index| lines.get(index))
        }) else {
            return String::new();
        };
        cut(text.trim(), MAX_EXCERPT)
    }
}

/// `text` cut to `most` characters, with `...` added when it was cut.
fn cut(text: &str, most: usize) -> String {
    match text.char_indices().nth(most) {
        Some((end, _)) => format!("{}...", &text[..end]),
        None => text.to_owned(),
    }
}

/// The places `found` as a block headed by how many `noun`s there are of `what`, grouped by file in the order first found, each place as `line:column` and the line it is on.
pub(super) fn locations(
    sandbox: &Sandbox,
    noun: &str,
    what: &str,
    found: &[Location],
    lines: &mut Lines,
) -> String {
    if found.is_empty() {
        return format!("No {noun} found for {what}.");
    }
    let mut unique: Vec<&Location> = Vec::with_capacity(found.len());
    for location in found {
        if !unique.iter().any(|seen| {
            seen.path == location.path
                && seen.line == location.line
                && seen.column == location.column
        }) {
            unique.push(location);
        }
    }
    let mut files: Vec<&Path> = Vec::new();
    for location in &unique {
        if !files.contains(&location.path.as_path()) {
            files.push(&location.path);
        }
    }
    let mut out = format!(
        "{} {noun}(s) of {what} in {} file(s):",
        unique.len(),
        files.len()
    );
    let mut shown = 0;
    for file in files {
        if shown >= MAX_LOCATIONS {
            break;
        }
        let _ = write!(out, "\n{}", sandbox.display(file));
        let mut here: Vec<&&Location> = unique
            .iter()
            .filter(|location| location.path == file)
            .collect();
        here.sort_by_key(|location| (location.line, location.column));
        for location in here {
            if shown >= MAX_LOCATIONS {
                break;
            }
            shown += 1;
            let _ = write!(
                out,
                "\n  {}:{}  {}",
                location.line,
                location.column,
                lines.excerpt(file, location.line)
            );
        }
    }
    if unique.len() > shown {
        let _ = write!(out, "\n[{} more not shown]", unique.len() - shown);
    }
    out
}

/// What a hover said about `what`, cut to [`MAX_HOVER`] bytes, or a line saying there was nothing.
pub(super) fn hover(what: &str, said: Option<&str>) -> String {
    let Some(said) = said.map(str::trim).filter(|said| !said.is_empty()) else {
        return format!("The language server has nothing to say about {what}.");
    };
    let mut text = said.to_owned();
    if text.len() > MAX_HOVER {
        let mut end = MAX_HOVER;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str("\n[cut]");
    }
    format!("{what}:\n{text}")
}

/// The symbols of the file shown as `shown`, as an outline: each on its line number, indented by how deeply it is nested, with its kind, name and detail.
pub(super) fn document_symbols(shown: &str, symbols: &[Symbol]) -> String {
    if symbols.is_empty() {
        return format!("No symbols found in {shown}.");
    }
    let mut out = format!("{} symbol(s) in {shown}:", symbols.len());
    for symbol in symbols.iter().take(MAX_DOCUMENT_SYMBOLS) {
        let _ = write!(
            out,
            "\n{:>5}  {}{} {}",
            symbol.location.line,
            "  ".repeat(symbol.depth),
            symbol.kind,
            symbol.name
        );
        if let Some(detail) = symbol
            .detail
            .as_deref()
            .map(|detail| detail.lines().next().unwrap_or_default().trim())
            .filter(|detail| !detail.is_empty() && *detail != symbol.name)
        {
            let _ = write!(out, "  {}", cut(detail, 100));
        }
    }
    if symbols.len() > MAX_DOCUMENT_SYMBOLS {
        let _ = write!(
            out,
            "\n[{} more not shown]",
            symbols.len() - MAX_DOCUMENT_SYMBOLS
        );
    }
    out
}

/// The symbols of the project matching `query`, each with its kind, name, place and what it is inside of.
pub(super) fn workspace_symbols(sandbox: &Sandbox, query: &str, symbols: &[Symbol]) -> String {
    if symbols.is_empty() {
        return format!("No symbols match \"{query}\".");
    }
    let mut out = format!("{} symbol(s) match \"{query}\":", symbols.len());
    for symbol in symbols.iter().take(MAX_WORKSPACE_SYMBOLS) {
        let _ = write!(
            out,
            "\n{} {}  {}:{}",
            symbol.kind,
            symbol.name,
            sandbox.display(&symbol.location.path),
            symbol.location.line
        );
        if let Some(container) = symbol.container.as_deref().filter(|c| !c.is_empty()) {
            let _ = write!(out, "  (in {container})");
        }
    }
    if symbols.len() > MAX_WORKSPACE_SYMBOLS {
        let _ = write!(
            out,
            "\n[{} more not shown; narrow the query]",
            symbols.len() - MAX_WORKSPACE_SYMBOLS
        );
    }
    out
}

/// The diagnostics `found` about `scope`, worst first, then by file and place, each as `file:line:column severity: message`, headed by how many there are of each severity.
pub(super) fn diagnostics(sandbox: &Sandbox, scope: &str, mut found: Vec<Diagnostic>) -> String {
    found.sort_by(|left, right| {
        left.severity
            .cmp(&right.severity)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| (left.line, left.column).cmp(&(right.line, right.column)))
    });
    found.dedup_by(|left, right| {
        left.path == right.path
            && left.line == right.line
            && left.column == right.column
            && left.message == right.message
    });
    if found.is_empty() {
        return format!("No diagnostics in {scope}.");
    }
    let count = |severity: Severity| found.iter().filter(|d| d.severity == severity).count();
    let mut out = format!(
        "Diagnostics in {scope}: {} error(s), {} warning(s), {} other",
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Information) + count(Severity::Hint)
    );
    for diagnostic in found.iter().take(MAX_DIAGNOSTICS) {
        let _ = write!(
            out,
            "\n{}:{}:{} {}: {}",
            sandbox.display(&diagnostic.path),
            diagnostic.line,
            diagnostic.column,
            diagnostic.severity.as_str(),
            message(diagnostic)
        );
    }
    if found.len() > MAX_DIAGNOSTICS {
        let _ = write!(out, "\n[{} more not shown]", found.len() - MAX_DIAGNOSTICS);
    }
    out
}

/// The code actions `offered` for `what`, numbered from one as they are applied, each with its kind and whether it is preferred or disabled.
pub(super) fn code_actions(what: &str, offered: &[CodeAction]) -> String {
    if offered.is_empty() {
        return format!("No code actions offered at {what}.");
    }
    let mut out = format!("{} code action(s) at {what}:", offered.len());
    for (index, action) in offered.iter().enumerate() {
        let _ = write!(out, "\n{:>3}. {}", index + 1, action.title);
        if let Some(kind) = action.kind.as_deref().filter(|kind| !kind.is_empty()) {
            let _ = write!(out, "  [{kind}]");
        }
        if action.preferred {
            out.push_str("  (preferred)");
        }
        if let Some(reason) = &action.disabled {
            let _ = write!(out, "  (disabled: {reason})");
        }
    }
    out.push_str("\nApply one with the same call and apply: <number>, best with title as listed.");
    out
}

/// The language servers `servers` under `heading`, each with its state, its languages and anything more said about it.
pub(super) fn servers(heading: &str, servers: &[ServerStatus]) -> String {
    if servers.is_empty() {
        return format!("{heading}: no language servers.");
    }
    let mut out = format!("{heading}:");
    for server in servers {
        let _ = write!(out, "\n{}: {}", server.name, server.state);
        if !server.languages.is_empty() {
            let _ = write!(out, " ({})", server.languages.join(", "));
        }
        if let Some(detail) = server.detail.as_deref().filter(|detail| !detail.is_empty()) {
            let _ = write!(out, "; {detail}");
        }
    }
    out
}

/// How many lines differ between `old` and `new`: those that differ where both have a line, and those one has beyond the other, once the lines they start and end with alike are left out.
pub(super) fn changed_line_count(old: &str, new: &str) -> usize {
    let (old_lines, new_lines): (Vec<&str>, Vec<&str>) =
        (old.lines().collect(), new.lines().collect());
    let (before, after) = alike_ends(&old_lines, &new_lines);
    (old_lines.len() - before - after).max(new_lines.len() - before - after)
}

/// How many lines `old` and `new` start with alike, and how many of the rest they end with alike.
fn alike_ends(old: &[&str], new: &[&str]) -> (usize, usize) {
    let before = old
        .iter()
        .zip(new)
        .take_while(|(left, right)| left == right)
        .count();
    let after = old[before..]
        .iter()
        .rev()
        .zip(new[before..].iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    (before, after)
}

/// The lines of `old` that differ in `new`, as removed and added lines under the number of the line, at most `most` of them; for a rename, which changes lines without adding or removing any, this is the whole change. When lines are added or removed, the stretch between the lines both start and end with alike is shown as removed and then added, at most `most` lines of each.
pub(super) fn changed_lines(old: &str, new: &str, most: usize) -> String {
    let old_lines: Vec<&str> = old.lines().collect();
    let new_lines: Vec<&str> = new.lines().collect();
    if old_lines.len() != new_lines.len() {
        let (before, after) = alike_ends(&old_lines, &new_lines);
        let removed = &old_lines[before..old_lines.len() - after];
        let added = &new_lines[before..new_lines.len() - after];
        let mut out = format!(
            "\n  @@ line {}: {} line(s) become {}",
            before + 1,
            removed.len(),
            added.len()
        );
        for (sign, lines) in [("-", removed), ("+", added)] {
            for line in lines.iter().take(most) {
                let _ = write!(out, "\n  {sign} {line}");
            }
            if lines.len() > most {
                let _ = write!(out, "\n  [{} more {sign} line(s)]", lines.len() - most);
            }
        }
        return out;
    }
    let changed: Vec<usize> = (0..old_lines.len())
        .filter(|index| old_lines[*index] != new_lines[*index])
        .collect();
    let mut out = String::new();
    for index in changed.iter().take(most) {
        let _ = write!(
            out,
            "\n  @@ line {}\n  - {}\n  + {}",
            index + 1,
            old_lines[*index],
            new_lines[*index]
        );
    }
    if changed.len() > most {
        let _ = write!(out, "\n  [{} more changed line(s)]", changed.len() - most);
    }
    out
}

#[cfg(test)]
/// Tests for writing answers.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// A place at `line` and `column` of `path`.
    fn at(path: &Path, line: u32, column: u32) -> Location {
        Location {
            path: path.to_path_buf(),
            line,
            column,
            end_line: line,
            end_column: column + 1,
        }
    }

    /// Places are grouped by file, sorted, quoted from their lines, and told once each.
    #[test]
    fn groups_places_by_file_with_their_lines() {
        let dir = scratch("lsp-render-locations");
        std::fs::write(
            dir.join("a.rs"),
            "fn helper() {}\nfn main() {\n    helper();\n}\n",
        )
        .unwrap();
        std::fs::write(dir.join("b.rs"), "use a::helper;\n").unwrap();
        let sandbox = Sandbox::new(&dir).unwrap();
        let (a, b) = (dir.join("a.rs"), dir.join("b.rs"));
        let text = locations(
            &sandbox,
            "reference",
            "`helper`",
            &[at(&a, 3, 5), at(&b, 1, 8), at(&a, 1, 4), at(&a, 3, 5)],
            &mut Lines::default(),
        );
        assert_eq!(
            text,
            "3 reference(s) of `helper` in 2 file(s):\na.rs\n  1:4  fn helper() {}\n  3:5  helper();\nb.rs\n  1:8  use a::helper;"
        );
        assert_eq!(
            locations(&sandbox, "definition", "`x`", &[], &mut Lines::default()),
            "No definition found for `x`."
        );
    }

    /// An outline indents nested symbols and leaves out a detail that only repeats the name.
    #[test]
    fn outlines_symbols() {
        let symbol = |name: &str, kind: &str, depth, line, detail: Option<&str>| Symbol {
            name: name.to_owned(),
            kind: kind.to_owned(),
            detail: detail.map(str::to_owned),
            container: None,
            depth,
            location: at(Path::new("/w/a.rs"), line, 1),
        };
        let text = document_symbols(
            "a.rs",
            &[
                symbol("Config", "struct", 0, 1, Some("Config")),
                symbol("path", "field", 1, 2, Some("PathBuf")),
            ],
        );
        assert_eq!(
            text,
            "2 symbol(s) in a.rs:\n    1  struct Config\n    2    field path  PathBuf"
        );
    }

    /// Only the lines a rename changed are shown, under their numbers.
    #[test]
    fn shows_the_lines_a_rename_changed() {
        let shown = changed_lines("a\nold()\nc\n", "a\nnew()\nc\n", 10);
        assert_eq!(shown, "\n  @@ line 2\n  - old()\n  + new()");
    }
}
