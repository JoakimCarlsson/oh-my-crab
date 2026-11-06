//! Line selectors: the `:50-200` style suffix of a `read` path.

use crate::ToolError;

/// One stretch of lines a selector asks for, before it is fitted to a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Span {
    /// From this line, 1-indexed, to the end.
    From(usize),
    /// From the first line to the second, both 1-indexed and inclusive.
    Between(usize, usize),
    /// The last this many lines.
    Last(usize),
}

/// A line range fitted to a text: the first line and, unless it runs to the
/// end, the last, both 1-indexed and inclusive.
pub(super) type Range = (usize, Option<usize>);

/// What the suffix of a `read` path asks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Selector {
    /// The stretches of lines asked for; none means the whole text.
    pub(super) spans: Vec<Span>,
    /// Whether the original HTML of a web page is wanted instead of its text.
    pub(super) raw: bool,
    /// Whether only the merge conflicts of a file are wanted.
    pub(super) conflicts: bool,
}

impl Selector {
    /// Parses a selector such as `50`, `50-`, `50-200`, `50+150`, `-60`, `raw`,
    /// or a comma-separated list of them, or `conflicts` alone.
    ///
    /// Returns `None` when `text` is not a selector, so that the caller can
    /// treat it as part of a file name instead.
    pub(super) fn parse(text: &str) -> Option<Self> {
        if text.trim() == "conflicts" {
            return Some(Self {
                conflicts: true,
                ..Self::default()
            });
        }
        let mut selector = Self::default();
        for part in text.split(',').map(str::trim) {
            if part == "raw" {
                selector.raw = true;
            } else {
                selector.spans.push(span(part)?);
            }
        }
        Some(selector)
    }

    /// Folds the legacy `offset` and `limit` arguments into the selector.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the selector already names
    /// lines or conflicts and `offset` or `limit` is given as well.
    pub(super) fn windowed(
        mut self,
        offset: Option<usize>,
        limit: Option<usize>,
    ) -> Result<Self, ToolError> {
        if offset.is_none() && limit.is_none() {
            return Ok(self);
        }
        if self.picks_lines() {
            return Err(ToolError::InvalidArguments(
                "give lines either as a path suffix such as file:50-200 or with offset and \
                 limit, not both"
                    .to_string(),
            ));
        }
        let first = offset.unwrap_or(1).max(1);
        self.spans.push(match limit {
            Some(count) => Span::Between(first, first + count.max(1) - 1),
            None => Span::From(first),
        });
        Ok(self)
    }

    /// Whether the selector names any lines, or the conflicts, rather than the
    /// whole text.
    pub(super) fn picks_lines(&self) -> bool {
        !self.spans.is_empty() || self.conflicts
    }

    /// Whether the ranges depend on the text's length, which then has to be
    /// counted first.
    pub(super) fn needs_total(&self) -> bool {
        self.spans.iter().any(|span| matches!(span, Span::Last(_)))
    }

    /// Fits the spans to a text of `total` lines, sorted and with overlapping or
    /// touching ranges merged; the whole text when the selector names no lines.
    ///
    /// `total` is only consulted for [`Span::Last`]; pass `None` when no span
    /// needs it.
    pub(super) fn ranges(&self, total: Option<usize>) -> Vec<Range> {
        if self.spans.is_empty() {
            return vec![(1, None)];
        }
        let total = total.unwrap_or(0);
        let mut ranges: Vec<Range> = self
            .spans
            .iter()
            .map(|span| match *span {
                Span::From(first) => (first, None),
                Span::Between(first, last) => (first, Some(last)),
                Span::Last(count) => (total.saturating_sub(count) + 1, Some(total)),
            })
            .filter(|(first, last)| last.is_none_or(|last| last >= *first))
            .collect();
        ranges.sort_by_key(|(first, _)| *first);
        let mut merged: Vec<Range> = Vec::with_capacity(ranges.len());
        for (first, last) in ranges {
            match merged.last_mut() {
                Some((_, end)) if end.is_none_or(|end| first <= end + 1) => {
                    *end = end.zip(last).map(|(a, b)| a.max(b));
                }
                _ => merged.push((first, last)),
            }
        }
        merged
    }
}

/// Parses one comma-separated part of a selector into a span.
fn span(part: &str) -> Option<Span> {
    if let Some(count) = part.strip_prefix('-') {
        return Some(Span::Last(number(count)?));
    }
    if let Some((first, count)) = part.split_once('+') {
        let first = number(first)?;
        return Some(Span::Between(first, first + number(count)? - 1));
    }
    if let Some((first, last)) = part.split_once('-') {
        let first = number(first)?;
        if last.is_empty() {
            return Some(Span::From(first));
        }
        let last = number(last)?;
        return (last >= first).then_some(Span::Between(first, last));
    }
    Some(Span::From(number(part)?))
}

/// Parses a positive decimal number made of ASCII digits only.
fn number(text: &str) -> Option<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok().filter(|value| *value > 0)
}

/// Splits `raw` into a path and the selector after its last `:`, when that
/// suffix parses as one and the path before it is not empty.
pub(super) fn split(raw: &str) -> Option<(&str, Selector)> {
    let (path, suffix) = raw.rsplit_once(':')?;
    if path.is_empty() {
        return None;
    }
    Some((path, Selector::parse(suffix)?))
}

/// Splits a URL into the address and its selector.
///
/// A `:` only starts a selector after the path has begun, so the port in
/// `http://host:8080` is never taken for one, and `conflicts`, which only
/// applies to files, stays part of the address.
pub(super) fn split_url(raw: &str) -> (&str, Selector) {
    let path_start = raw
        .find("://")
        .and_then(|scheme| raw[scheme + 3..].find('/').map(|slash| scheme + 3 + slash));
    match (path_start, raw.rfind(':')) {
        (Some(start), Some(colon)) if colon > start => match split(raw) {
            Some(split) if !split.1.conflicts => split,
            _ => (raw, Selector::default()),
        },
        _ => (raw, Selector::default()),
    }
}

/// Formats `ranges` back into selector syntax, for a note saying where to go on.
pub(super) fn render(ranges: &[Range]) -> String {
    ranges
        .iter()
        .map(|(first, last)| match last {
            Some(last) => format!("{first}-{last}"),
            None => first.to_string(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every span form parses, alone or in a list.
    #[test]
    fn parses_every_form() {
        let spans = |text: &str| Selector::parse(text).unwrap().spans;
        assert_eq!(spans("50"), vec![Span::From(50)]);
        assert_eq!(spans("50-"), vec![Span::From(50)]);
        assert_eq!(spans("50-200"), vec![Span::Between(50, 200)]);
        assert_eq!(spans("50+150"), vec![Span::Between(50, 199)]);
        assert_eq!(spans("-60"), vec![Span::Last(60)]);
        assert_eq!(
            spans("5-16, 960-973"),
            vec![Span::Between(5, 16), Span::Between(960, 973)]
        );
        let raw = Selector::parse("raw").unwrap();
        assert!(raw.raw && raw.spans.is_empty());
        let conflicts = Selector::parse("conflicts").unwrap();
        assert!(conflicts.conflicts && conflicts.picks_lines());
        assert_eq!(Selector::parse("conflicts,1-2"), None);
    }

    /// Text that is not a selector is refused rather than misread.
    #[test]
    fn refuses_non_selectors() {
        for text in [
            "", "abc", "0", "5-3", "+5", "5+0", "-0", "1,", "1-2-3", "x-5", "\\dir", "+1",
        ] {
            assert_eq!(Selector::parse(text), None, "{text:?}");
        }
    }

    /// A path splits at its last colon only when the suffix is a selector.
    #[test]
    fn splits_paths() {
        let (path, selector) = split("src/a.rs:5-16,960-973").unwrap();
        assert_eq!(path, "src/a.rs");
        assert_eq!(selector.spans.len(), 2);
        assert_eq!(split("dir/a:b.txt"), None);
        assert_eq!(split("C:\\notes.txt"), None);
        assert_eq!(split(":50"), None);
        let (path, _) = split("odd:name.txt:7").unwrap();
        assert_eq!(path, "odd:name.txt");
    }

    /// Ports are not selectors; suffixes after the path are.
    #[test]
    fn splits_urls() {
        let (url, selector) = split_url("http://localhost:8080");
        assert_eq!(
            (url, selector),
            ("http://localhost:8080", Selector::default())
        );
        let (url, selector) = split_url("http://localhost:8080/docs:raw");
        assert_eq!(url, "http://localhost:8080/docs");
        assert!(selector.raw);
        let (url, selector) = split_url("https://example.com/a:10-20");
        assert_eq!(url, "https://example.com/a");
        assert_eq!(selector.spans, vec![Span::Between(10, 20)]);
        let (url, _) = split_url("https://example.com/wiki/Help:Contents");
        assert_eq!(url, "https://example.com/wiki/Help:Contents");
    }

    /// Ranges are fitted to the length, sorted and merged.
    #[test]
    fn fits_ranges() {
        let ranges = |text: &str, total| Selector::parse(text).unwrap().ranges(total);
        assert_eq!(Selector::default().ranges(None), vec![(1, None)]);
        assert_eq!(ranges("-3", Some(10)), vec![(8, Some(10))]);
        assert_eq!(ranges("-30", Some(10)), vec![(1, Some(10))]);
        assert_eq!(ranges("-3", Some(0)), Vec::<Range>::new());
        assert_eq!(
            ranges("20-30,1-5,4-8,9-9", None),
            vec![(1, Some(9)), (20, Some(30))]
        );
        assert_eq!(ranges("10-12,11", None), vec![(10, None)]);
        assert_eq!(render(&ranges("1-5,9", None)), "1-5,9");
    }

    /// Offset and limit become a span, but not on top of a suffix.
    #[test]
    fn folds_offset_and_limit() {
        let window = Selector::default().windowed(Some(3), Some(2)).unwrap();
        assert_eq!(window.spans, vec![Span::Between(3, 4)]);
        let open = Selector::default().windowed(Some(3), None).unwrap();
        assert_eq!(open.spans, vec![Span::From(3)]);
        let both = Selector::parse("1-2").unwrap().windowed(Some(3), None);
        assert!(matches!(both, Err(ToolError::InvalidArguments(_))));
    }
}
