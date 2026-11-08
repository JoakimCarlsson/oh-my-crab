//! Turning the place a model names, a line and the symbol on it, into the character a language server is asked about.

/// How many lines either side of the one named a symbol is looked for when it is not on that line, since a model's line number is often off by a little.
const NEARBY: usize = 5;

/// Where a symbol was found: its line and the character its name starts at, both counted from one, and a note when it was not on the line named.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Found {
    /// The line, counted from one.
    pub(super) line: u32,
    /// The character of the line, counted from one.
    pub(super) column: u32,
    /// What to tell the model about where the symbol was found, when it was not where it said.
    pub(super) note: Option<String>,
}

/// Finds the `occurrence`th (from one) place `symbol` is written on line `line` (from one) of `text`, or, when it is not on that line at all, the nearest line within [`NEARBY`] lines that has it.
///
/// A whole-word match wins over one inside a longer name. A qualified symbol such as `Type::method` or `value.field` is placed at its last part, which is the symbol meant.
///
/// # Errors
///
/// Returns what to tell the model when the line is past the end of the text, the symbol is empty, has fewer occurrences on the line than asked, or is on none of the lines looked at.
pub(super) fn find(
    text: &str,
    line: u32,
    symbol: &str,
    occurrence: Option<u32>,
) -> Result<Found, String> {
    let lines: Vec<&str> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect();
    let count = lines.len() - usize::from(text.ends_with('\n'));
    let index = usize::try_from(line).unwrap_or(usize::MAX);
    if index == 0 || index > count.max(1) {
        return Err(format!(
            "line {line} is past the end of the file, which has {count} line(s)"
        ));
    }
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return Err("symbol is empty; give the name as it is written on the line".to_owned());
    }
    let shift = last_part(symbol);
    let wanted = occurrence.unwrap_or(1).max(1);
    let here = starts(lines[index - 1], symbol);
    if !here.is_empty() {
        let Some(start) = here.get(usize::try_from(wanted - 1).unwrap_or(usize::MAX)) else {
            return Err(format!(
                "`{symbol}` occurs {} time(s) on line {line}, not {wanted}",
                here.len()
            ));
        };
        return Ok(Found {
            line,
            column: column_of(*start + shift),
            note: None,
        });
    }
    for distance in 1..=NEARBY {
        for candidate in [index.checked_sub(distance), Some(index + distance)]
            .into_iter()
            .flatten()
            .filter(|candidate| (1..=count).contains(candidate))
        {
            if let Some(start) = starts(lines[candidate - 1], symbol).first() {
                let found = u32::try_from(candidate).unwrap_or(u32::MAX);
                return Ok(Found {
                    line: found,
                    column: column_of(*start + shift),
                    note: Some(format!(
                        "`{symbol}` is not on line {line}; used its first occurrence on line {found}"
                    )),
                });
            }
        }
    }
    Err(format!(
        "`{symbol}` is not on line {line}, nor within {NEARBY} lines of it. Line {line} is: {}",
        lines[index - 1].trim()
    ))
}

/// Finds the first line of `text` on which `symbol` is written as a whole word, for a model that named a symbol but no line, with a note saying which line was used.
///
/// # Errors
///
/// Returns what to tell the model when the symbol is empty or not written as a whole word anywhere in the text.
pub(super) fn find_anywhere(text: &str, symbol: &str) -> Result<Found, String> {
    let symbol = symbol.trim();
    if symbol.is_empty() {
        return Err("symbol is empty; give the name as it is written in the file".to_owned());
    }
    let shift = last_part(symbol);
    text.lines()
        .enumerate()
        .find_map(|(index, line)| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let first = *whole_words(line, symbol).first()?;
            let found = u32::try_from(index + 1).unwrap_or(u32::MAX);
            Some(Found {
                line: found,
                column: column_of(line[..first].chars().count() + shift),
                note: Some(format!(
                    "no line given; used the first occurrence of `{symbol}`, on line {found}"
                )),
            })
        })
        .ok_or_else(|| format!("`{symbol}` is not written anywhere in the file as a whole word"))
}

/// How many characters the part of `symbol` a language server is pointed at has: its last part when it is qualified, as [`find`] places it, otherwise all of it.
pub(super) fn named_length(symbol: &str) -> u32 {
    let symbol = symbol.trim();
    let length = symbol.chars().count() - last_part(symbol);
    u32::try_from(length).unwrap_or(u32::MAX)
}

/// How many characters into `symbol` its last part starts: after the last `::`, `.`, `->` or `#` when it is qualified, otherwise nought.
fn last_part(symbol: &str) -> usize {
    let cut = ["::", ".", "->", "#"]
        .iter()
        .filter_map(|separator| {
            symbol
                .rfind(separator)
                .map(|at| at + separator.len())
                .filter(|end| *end < symbol.len())
        })
        .max()
        .unwrap_or(0);
    symbol[..cut].chars().count()
}

/// The characters, counted from nought, at which `symbol` starts on `line`: its whole-word occurrences when it has any, otherwise every occurrence.
fn starts(line: &str, symbol: &str) -> Vec<usize> {
    let whole = whole_words(line, symbol);
    let chosen = if whole.is_empty() {
        line.match_indices(symbol).map(|(at, _)| at).collect()
    } else {
        whole
    };
    chosen
        .into_iter()
        .map(|at| line[..at].chars().count())
        .collect()
}

/// The byte offsets at which `symbol` is written on `line` as a whole word, not as part of a longer name.
fn whole_words(line: &str, symbol: &str) -> Vec<usize> {
    line.match_indices(symbol)
        .map(|(at, _)| at)
        .filter(|at| {
            let before = line[..*at].chars().next_back();
            let after = line[at + symbol.len()..].chars().next();
            let starts_word = symbol.chars().next().is_some_and(is_word);
            let ends_word = symbol.chars().next_back().is_some_and(is_word);
            !(starts_word && before.is_some_and(is_word))
                && !(ends_word && after.is_some_and(is_word))
        })
        .collect()
}

/// Whether `c` can be part of an identifier in most languages.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The character counted from one at `index` characters counted from nought.
fn column_of(index: usize) -> u32 {
    u32::try_from(index + 1).unwrap_or(u32::MAX)
}

#[cfg(test)]
/// Tests for placing symbols.
mod tests {
    use super::*;

    /// A symbol is placed at its whole-word occurrence, the one asked for, counting characters rather than bytes.
    #[test]
    fn places_the_symbol_on_its_line() {
        let text = "fn main() {\n    let é = helper(helper_count);\n    helper();\n}\n";
        assert_eq!(
            find(text, 2, "helper", None).unwrap(),
            Found {
                line: 2,
                column: 13,
                note: None
            }
        );
        assert_eq!(find(text, 2, "helper_count", None).unwrap().column, 20);
        assert_eq!(find(text, 3, "helper", Some(1)).unwrap().column, 5);
        let error = find(text, 3, "helper", Some(2)).unwrap_err();
        assert!(error.contains("occurs 1 time(s)"), "{error}");
    }

    /// A qualified symbol is placed at its last part.
    #[test]
    fn places_a_qualified_symbol_at_its_last_part() {
        let text = "let x = Config::load(path);\nself.items.len()\n";
        assert_eq!(find(text, 1, "Config::load", None).unwrap().column, 17);
        assert_eq!(find(text, 2, "self.items", None).unwrap().column, 6);
    }

    /// A symbol a line or two off is found on the nearest line that has it, with a note; one nowhere near is refused, quoting the line.
    #[test]
    fn looks_on_nearby_lines() {
        let text = "a\nb\nfn target() {}\nc\n";
        let found = find(text, 1, "target", None).unwrap();
        assert_eq!((found.line, found.column), (3, 4));
        assert!(found.note.unwrap().contains("line 3"));
        let error = find(text, 2, "missing", None).unwrap_err();
        assert!(error.contains("Line 2 is: b"), "{error}");
        assert!(
            find(text, 9, "a", None)
                .unwrap_err()
                .contains("past the end")
        );
    }

    /// With no line, the first whole-word occurrence in the file is used, with a note.
    #[test]
    fn finds_a_symbol_anywhere_without_a_line() {
        let text = "use crate::helpers;\nfn helper() {}\n";
        let found = find_anywhere(text, "helper").unwrap();
        assert_eq!((found.line, found.column), (2, 4));
        assert!(found.note.unwrap().contains("line 2"));
        assert!(find_anywhere(text, "missing").is_err());
    }

    /// A name inside a longer one is used only when there is no whole-word occurrence.
    #[test]
    fn falls_back_to_a_part_of_a_longer_name() {
        assert_eq!(
            find("call_helper_now()\n", 1, "helper", None)
                .unwrap()
                .column,
            6
        );
    }
}
