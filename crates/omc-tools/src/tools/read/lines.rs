//! Numbering the lines a selector picks out of a text.

use std::io::{self, BufRead};

use super::selector::{self, Selector};
use crate::ToolError;

/// Most lines one call returns.
pub(super) const MAX_LINES: usize = 2000;

/// Most bytes of text one call returns.
pub(super) const MAX_BYTES: usize = 50_000;

/// Longest line kept, in characters; longer lines are cut.
const MAX_LINE_CHARS: usize = 2000;

/// The line printed where the output skips lines between two ranges.
const GAP: &str = "…\n";

/// Numbers the lines `selector` picks from the text that `open` reads, which is
/// called `label` in notes.
///
/// `open` is called a second time when the selector counts from the end, since
/// the length has to be known first. Output stops at [`MAX_LINES`] lines or
/// [`MAX_BYTES`] bytes; a trailing note then says which selector continues.
///
/// # Errors
///
/// Returns [`ToolError::Io`] when the text cannot be read and
/// [`ToolError::Failed`] when it is not UTF-8.
pub(super) fn number<R: BufRead>(
    open: impl Fn() -> io::Result<R>,
    selector: &Selector,
    label: &str,
) -> Result<String, ToolError> {
    let total = if selector.needs_total() {
        Some(count(open()?)?)
    } else {
        None
    };
    let ranges = selector.ranges(total);
    let mut reader = open()?;
    let mut out = String::new();
    let mut shown = 0usize;
    let mut previous = 0usize;
    let mut line_number = 0usize;
    let mut current = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        line_number += 1;
        while ranges
            .get(current)
            .is_some_and(|(_, last)| last.is_some_and(|last| line_number > last))
        {
            current += 1;
        }
        let Some(&(first, last)) = ranges.get(current) else {
            break;
        };
        if line_number < first {
            continue;
        }
        let line = std::str::from_utf8(&buf).map_err(|_| not_text(label))?;
        if shown >= MAX_LINES || out.len() >= MAX_BYTES {
            let mut rest = vec![(line_number, last)];
            rest.extend_from_slice(&ranges[current + 1..]);
            out.push_str(&format!(
                "[more lines remain; continue with {label}:{}]\n",
                selector::render(&rest)
            ));
            return Ok(out);
        }
        if previous != 0 && line_number != previous + 1 {
            out.push_str(GAP);
        }
        out.push_str(&format!(
            "{line_number:>6}\t{}\n",
            clip(line.trim_end_matches(['\n', '\r']))
        ));
        shown += 1;
        previous = line_number;
    }
    if !out.is_empty() {
        return Ok(out);
    }
    Ok(match ranges.first() {
        _ if line_number == 0 => format!("[{label} is empty]"),
        Some((first, _)) => {
            format!("[line {first} is past the end of {label}, which has {line_number} lines]")
        }
        None => format!("[{label} has {line_number} lines; the selector picks none of them]"),
    })
}

/// Counts the lines of `reader`, a last line without a newline included.
///
/// # Errors
///
/// Returns the I/O error when reading fails.
fn count(mut reader: impl BufRead) -> io::Result<usize> {
    let mut lines = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            return Ok(lines);
        }
        lines += 1;
    }
}

/// Builds the error for a text that is not UTF-8, which is what most binary
/// files look like.
pub(super) fn not_text(label: &str) -> ToolError {
    ToolError::Failed(format!("{label} looks like a binary file"))
}

/// Cuts `line` to [`MAX_LINE_CHARS`] characters, marking the cut.
fn clip(line: &str) -> String {
    match line.char_indices().nth(MAX_LINE_CHARS) {
        Some((end, _)) => format!("{}… [line truncated]", &line[..end]),
        None => line.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Numbers the lines of `text` that `selector` picks.
    fn pick(text: &str, selector: &str) -> String {
        let selector = Selector::parse(selector).unwrap();
        number(|| Ok(io::Cursor::new(text.as_bytes())), &selector, "t").unwrap()
    }

    /// Ranges, counts and tails pick the right lines, with gaps marked.
    #[test]
    fn picks_selected_lines() {
        let text = "a\nb\nc\nd\ne\nf\n";
        assert_eq!(pick(text, "2-3"), "     2\tb\n     3\tc\n");
        assert_eq!(pick(text, "5"), "     5\te\n     6\tf\n");
        assert_eq!(pick(text, "2+2"), "     2\tb\n     3\tc\n");
        assert_eq!(pick(text, "-2"), "     5\te\n     6\tf\n");
        assert_eq!(
            pick(text, "1-1,5-5"),
            format!("     1\ta\n{GAP}     5\te\n")
        );
        assert_eq!(pick("a\nb", "-1"), "     2\tb\n");
    }

    /// Selecting past the end or reading nothing says so.
    #[test]
    fn reports_missing_lines() {
        assert!(pick("a\n", "9").contains("past the end of t, which has 1 lines"));
        assert_eq!(pick("", "1"), "[t is empty]");
    }

    /// A long read stops with a note that continues where it stopped.
    #[test]
    fn notes_where_to_continue() {
        let text = "x\n".repeat(MAX_LINES + 200);
        let out = pick(&text, "1-2050,2100-2105");
        assert!(out.ends_with("[more lines remain; continue with t:2001-2050,2100-2105]\n"));
        assert_eq!(out.matches('\t').count(), MAX_LINES);
        let open = pick(&text, "1");
        assert!(open.ends_with("[more lines remain; continue with t:2001]\n"));
    }

    /// Text that is not UTF-8 is refused.
    #[test]
    fn refuses_non_utf8() {
        let bytes: &[u8] = b"ok\n\xff\xfe\n";
        let result = number(|| Ok(io::Cursor::new(bytes)), &Selector::default(), "t");
        assert!(matches!(result, Err(ToolError::Failed(m)) if m.contains("binary")));
    }
}
