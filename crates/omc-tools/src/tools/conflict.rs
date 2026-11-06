//! Merge conflicts left in a file: finding the marker blocks, numbering them,
//! describing them in `read` results and working out the lines a resolution
//! puts in a block's place.
//!
//! A block opens with a `<<<<<<<` line, may hold a `|||||||` line before the
//! common ancestor's version (the diff3 and zdiff3 styles), is split by a line
//! of exactly `=======` and closes with a `>>>>>>>` line. Markers count only at
//! the start of a line, alone or followed by a space and a label. Conflicts are
//! numbered from 1 in file order, so a number names the same block for as long
//! as the snapshot tag of the file stays the same.

use std::io::Read as _;
use std::path::Path;

use serde::Deserialize;

/// Opens a block; what follows is our side.
const OURS: &str = "<<<<<<<";

/// Starts the common ancestor's side in the diff3 styles.
const BASE: &str = "|||||||";

/// Ends our side (or the ancestor's) and starts theirs.
const SEPARATOR: &str = "=======";

/// Closes a block.
const THEIRS: &str = ">>>>>>>";

/// Largest file scanned for conflicts; larger files are read without them.
const MAX_SCAN_BYTES: u64 = 10 * 1024 * 1024;

/// Most bytes of blocks one conflicts listing shows.
const MAX_LISTING_BYTES: usize = 50_000;

/// One conflict block, by 1-indexed line numbers of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Conflict {
    /// Its number, counting from 1 in file order.
    pub(crate) id: usize,
    /// The `<<<<<<<` line.
    pub(crate) start: usize,
    /// The `|||||||` line, when the block has the ancestor's side.
    pub(crate) base: Option<usize>,
    /// The `=======` line.
    pub(crate) separator: usize,
    /// The `>>>>>>>` line.
    pub(crate) end: usize,
    /// The label after `<<<<<<<`, often `HEAD`.
    pub(crate) ours_label: String,
    /// The label after `|||||||`.
    pub(crate) base_label: String,
    /// The label after `>>>>>>>`, often the branch merged in.
    pub(crate) theirs_label: String,
}

/// Which side of a conflict a resolution keeps.
#[derive(Debug, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Take {
    /// Our side, between `<<<<<<<` and `|||||||` or `=======`.
    Ours,
    /// Their side, between `=======` and `>>>>>>>`.
    Theirs,
    /// The common ancestor's side, between `|||||||` and `=======`.
    Base,
    /// Our side followed by theirs.
    Both,
}

impl Take {
    /// The word the model uses for this side.
    pub(crate) fn word(self) -> &'static str {
        match self {
            Self::Ours => "ours",
            Self::Theirs => "theirs",
            Self::Base => "base",
            Self::Both => "both",
        }
    }
}

impl Conflict {
    /// The 1-indexed first and last line of our side; the first is past the
    /// last when the side is empty.
    fn ours(&self) -> (usize, usize) {
        (self.start + 1, self.base.unwrap_or(self.separator) - 1)
    }

    /// The first and last line of the ancestor's side, when the block has one.
    fn base_side(&self) -> Option<(usize, usize)> {
        self.base.map(|base| (base + 1, self.separator - 1))
    }

    /// The first and last line of their side.
    fn theirs(&self) -> (usize, usize) {
        (self.separator + 1, self.end - 1)
    }

    /// The lines of `lines`, the whole file split into lines, that resolving
    /// the block with `take` keeps.
    ///
    /// # Errors
    ///
    /// Returns a message when `take` is [`Take::Base`] and the block has no
    /// ancestor's side.
    pub(crate) fn resolve<'a>(
        &self,
        lines: &[&'a str],
        take: Take,
    ) -> Result<Vec<&'a str>, String> {
        let pick = |(first, last): (usize, usize)| -> Vec<&'a str> {
            if first > last {
                return Vec::new();
            }
            lines[first - 1..last].to_vec()
        };
        Ok(match take {
            Take::Ours => pick(self.ours()),
            Take::Theirs => pick(self.theirs()),
            Take::Both => [pick(self.ours()), pick(self.theirs())].concat(),
            Take::Base => pick(self.base_side().ok_or_else(|| {
                format!(
                    "conflict {} has no base side; only conflicts written in the diff3 or \
                     zdiff3 style carry one",
                    self.id
                )
            })?),
        })
    }

    /// One line describing the block: its number, lines and sides.
    fn summary(&self) -> String {
        let mut sides = vec![side("ours", &self.ours_label, self.ours())];
        if let Some(range) = self.base_side() {
            sides.push(side("base", &self.base_label, range));
        }
        sides.push(side("theirs", &self.theirs_label, self.theirs()));
        format!(
            "#{} lines {}-{}: {}",
            self.id,
            self.start,
            self.end,
            sides.join(", ")
        )
    }
}

/// Describes one side of a block: its name, label and lines.
fn side(name: &str, label: &str, (first, last): (usize, usize)) -> String {
    let label = if label.is_empty() {
        String::new()
    } else {
        format!(" {label:?}")
    };
    let lines = if first > last {
        "empty".to_owned()
    } else if first == last {
        format!("line {first}")
    } else {
        format!("lines {first}-{last}")
    };
    format!("{name}{label} ({lines})")
}

/// Where the scan of a block has got to.
enum Phase {
    /// Outside any block.
    Idle,
    /// In our side of a block opened on the given line with the given label.
    Ours(usize, String),
    /// In the ancestor's side: the block's start, our label, the `|||||||` line and its label.
    Base(usize, String, usize, String),
    /// In their side: the block's start, our label, the `|||||||` line and its label if any, and the `=======` line.
    Theirs(usize, String, Option<(usize, String)>, usize),
}

/// The label after `marker` when `line` is that marker alone or followed by a
/// space and a label.
fn label<'a>(line: &'a str, marker: &str) -> Option<&'a str> {
    let rest = line.strip_prefix(marker)?;
    if rest.is_empty() {
        return Some("");
    }
    rest.strip_prefix(' ').map(str::trim)
}

/// The complete conflict blocks of `text`, numbered in order.
///
/// A block that is never closed, or whose markers come out of order, is
/// dropped; a new `<<<<<<<` starts over.
pub(crate) fn scan(text: &str) -> Vec<Conflict> {
    let mut found = Vec::new();
    let mut phase = Phase::Idle;
    for (index, raw) in text.split('\n').enumerate() {
        let number = index + 1;
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if let Some(name) = label(line, OURS) {
            phase = Phase::Ours(number, name.to_owned());
            continue;
        }
        phase = match phase {
            Phase::Idle => Phase::Idle,
            Phase::Ours(start, ours) => {
                if let Some(name) = label(line, BASE) {
                    Phase::Base(start, ours, number, name.to_owned())
                } else if line == SEPARATOR {
                    Phase::Theirs(start, ours, None, number)
                } else if label(line, THEIRS).is_some() {
                    Phase::Idle
                } else {
                    Phase::Ours(start, ours)
                }
            }
            Phase::Base(start, ours, base, base_label) => {
                if line == SEPARATOR {
                    Phase::Theirs(start, ours, Some((base, base_label)), number)
                } else if label(line, BASE).is_some() || label(line, THEIRS).is_some() {
                    Phase::Idle
                } else {
                    Phase::Base(start, ours, base, base_label)
                }
            }
            Phase::Theirs(start, ours, base, separator) => {
                if let Some(name) = label(line, THEIRS) {
                    let (base, base_label) =
                        base.map_or((None, String::new()), |(line, label)| (Some(line), label));
                    found.push(Conflict {
                        id: found.len() + 1,
                        start,
                        base,
                        separator,
                        end: number,
                        ours_label: ours,
                        base_label,
                        theirs_label: name.to_owned(),
                    });
                    Phase::Idle
                } else if line == SEPARATOR || label(line, BASE).is_some() {
                    Phase::Idle
                } else {
                    Phase::Theirs(start, ours, base, separator)
                }
            }
        };
    }
    found
}

/// The conflict blocks of the file at `path` and its text, or nothing when it
/// is larger than [`MAX_SCAN_BYTES`] or cannot be read as text.
pub(crate) fn scan_file(path: &Path) -> (Vec<Conflict>, String) {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_SCAN_BYTES + 1).read_to_end(&mut bytes));
    if read.is_err() || bytes.len() as u64 > MAX_SCAN_BYTES {
        return (Vec::new(), String::new());
    }
    match String::from_utf8(bytes) {
        Ok(text) if text.contains(OURS) => (scan(&text), text),
        _ => (Vec::new(), String::new()),
    }
}

/// Whether the numbered lines `shown`, as `read` prints them, include a
/// conflict marker.
pub(crate) fn shows_markers(shown: &str) -> bool {
    shown.lines().any(|line| {
        line.split_once('\t').is_some_and(|(_, text)| {
            [OURS, BASE, THEIRS]
                .iter()
                .any(|marker| label(text, marker).is_some())
                || text == SEPARATOR
        })
    })
}

/// How to resolve a conflict of the file `label`, for the end of a note.
fn how_to_resolve(label: &str) -> String {
    format!(
        "Resolve one with an edit operation {{\"op\": \"resolve\", \"conflict\": N, \"take\": \
         \"ours\"|\"theirs\"|\"base\"|\"both\"}}, or with \"text\" instead of \"take\" to put \
         your own lines in its place; it replaces the whole block, markers included. \
         {label}:conflicts lists only the conflicts."
    )
}

/// The note `read` adds after lines of the file `label` that show conflict
/// markers, listing every conflict in the file.
pub(crate) fn note(label: &str, conflicts: &[Conflict]) -> String {
    let mut out = format!("\n[{} in {label}:\n", count(conflicts.len()));
    for conflict in conflicts {
        out.push_str(&conflict.summary());
        out.push('\n');
    }
    out.push_str(&how_to_resolve(label));
    out.push_str("]\n");
    out
}

/// The conflicts of the file `label`, holding `text`, each described and
/// followed by its numbered lines, under the header `header`.
pub(crate) fn listing(header: &str, label: &str, text: &str, conflicts: &[Conflict]) -> String {
    if conflicts.is_empty() {
        return format!("{header}[no merge conflicts in {label}]\n");
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = format!("{header}[{} in {label}]\n", count(conflicts.len()));
    for conflict in conflicts {
        if out.len() >= MAX_LISTING_BYTES {
            out.push_str(&format!(
                "\n[conflicts #{} on are not shown; read {label}:{}- to see them]\n",
                conflict.id, conflict.start
            ));
            break;
        }
        out.push('\n');
        out.push_str(&conflict.summary());
        out.push('\n');
        for number in conflict.start..=conflict.end {
            let line = lines[number - 1]
                .strip_suffix('\r')
                .unwrap_or(lines[number - 1]);
            out.push_str(&format!("{number:>6}\t{line}\n"));
        }
    }
    out.push('\n');
    out.push('[');
    out.push_str(&how_to_resolve(label));
    out.push_str("]\n");
    out
}

/// "1 merge conflict" or "N merge conflicts".
fn count(n: usize) -> String {
    if n == 1 {
        "1 merge conflict".to_owned()
    } else {
        format!("{n} merge conflicts")
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A file with a plain conflict on lines 2-6 and a diff3 conflict on lines 8-14.
    pub(crate) const TWO: &str = "top\n<<<<<<< HEAD\nours 1\n=======\ntheirs 1\ntheirs 2\n>>>>>>> feature\nmiddle\n<<<<<<< HEAD\nours 2\n||||||| base\nancestor\n=======\n>>>>>>> feature\nend\n";

    /// Both blocks are found with their lines and labels.
    #[test]
    fn finds_blocks() {
        let found = scan(TWO);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0],
            Conflict {
                id: 1,
                start: 2,
                base: None,
                separator: 4,
                end: 7,
                ours_label: "HEAD".into(),
                base_label: String::new(),
                theirs_label: "feature".into(),
            }
        );
        assert_eq!(
            (
                found[1].start,
                found[1].base,
                found[1].separator,
                found[1].end
            ),
            (9, Some(11), 13, 14)
        );
        assert_eq!(
            found[1].summary(),
            "#2 lines 9-14: ours \"HEAD\" (line 10), base \"base\" (line 12), theirs \"feature\" (empty)"
        );
    }

    /// Lookalike lines, unclosed blocks and markers out of order are not conflicts.
    #[test]
    fn ignores_malformed_blocks() {
        assert!(scan("<<<<<<<< x\n=======\n>>>>>>> y\n").is_empty());
        assert!(scan("<<<<<<< a\n======= \n>>>>>>> b\n").is_empty());
        assert!(scan("<<<<<<< a\nx\n=======\ny\n").is_empty());
        assert!(scan("<<<<<<< a\n>>>>>>> b\n").is_empty());
        let restarted = scan("<<<<<<< a\nx\n<<<<<<< b\ny\n=======\nz\n>>>>>>> c\n");
        assert_eq!(restarted.len(), 1);
        assert_eq!(restarted[0].start, 3);
        assert_eq!(
            scan("<<<<<<< a\r\nx\r\n=======\r\ny\r\n>>>>>>> b\r\n").len(),
            1
        );
    }

    /// Each side resolves to its own lines; base needs a base side.
    #[test]
    fn resolves_sides() {
        let found = scan(TWO);
        let lines: Vec<&str> = TWO.split('\n').collect();
        assert_eq!(
            found[0].resolve(&lines, Take::Ours).unwrap(),
            vec!["ours 1"]
        );
        assert_eq!(
            found[0].resolve(&lines, Take::Theirs).unwrap(),
            vec!["theirs 1", "theirs 2"]
        );
        assert_eq!(
            found[0].resolve(&lines, Take::Both).unwrap(),
            vec!["ours 1", "theirs 1", "theirs 2"]
        );
        assert!(
            found[0]
                .resolve(&lines, Take::Base)
                .unwrap_err()
                .contains("no base side")
        );
        assert_eq!(
            found[1].resolve(&lines, Take::Base).unwrap(),
            vec!["ancestor"]
        );
        assert!(found[1].resolve(&lines, Take::Theirs).unwrap().is_empty());
    }

    /// Markers are spotted in numbered read output only as whole lines.
    #[test]
    fn spots_markers_in_numbered_lines() {
        assert!(shows_markers("     2\t<<<<<<< HEAD\n"));
        assert!(shows_markers("     4\t=======\n"));
        assert!(!shows_markers("     4\t========\n     5\tx <<<<<<< y\n"));
    }
}
