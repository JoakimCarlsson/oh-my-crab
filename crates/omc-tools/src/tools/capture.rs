//! One output stream of a command: kept whole while it is short, and once it grows too long, saved whole to a file while only its start and end are kept to show.
//!
//! A stream can also be condensed for the model as it arrives. Whenever condensing leaves anything out, the raw stream is saved whole to a file, however short it is, and the model is shown the condensed text with a note naming the file.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;

use super::bash::Condenser;
use crate::Spill;
use crate::spill::read_hint;

/// Most bytes of a stream shown to the model.
pub(crate) const MAX_STREAM_BYTES: usize = 30_000;

/// Bytes from the start of a stream shown when it is cut; the rest of what is shown is its end, where errors and summaries usually are.
pub(crate) const HEAD_BYTES: usize = 4_000;

/// Bytes from the end of a stream shown when it is cut.
const TAIL_BYTES: usize = MAX_STREAM_BYTES - HEAD_BYTES;

/// The part of a stream kept to show: all of it while it fits, its start and end once it does not.
#[derive(Default)]
struct Window {
    /// The whole stream while it fits, and its first [`HEAD_BYTES`] once it does not.
    head: Vec<u8>,
    /// The last [`TAIL_BYTES`] of the stream once it does not fit.
    tail: Vec<u8>,
    /// How many bytes the stream produced.
    total: usize,
}

impl Window {
    /// Takes in `bytes`, the next part of the stream, and returns everything the stream held so far when they are what made it too long to keep whole.
    fn push(&mut self, bytes: &[u8]) -> Option<Vec<u8>> {
        let fitted = self.fits();
        self.total += bytes.len();
        if !fitted {
            self.keep_tail(bytes);
            return None;
        }
        if self.fits() {
            self.head.extend_from_slice(bytes);
            return None;
        }
        let mut whole = std::mem::take(&mut self.head);
        whole.extend_from_slice(bytes);
        self.head = whole[..HEAD_BYTES].to_vec();
        self.keep_tail(&whole[HEAD_BYTES..]);
        Some(whole)
    }

    /// Whether the whole stream is kept.
    fn fits(&self) -> bool {
        self.total <= MAX_STREAM_BYTES
    }

    /// Adds `bytes` to the end kept to show, dropping what falls out of it.
    fn keep_tail(&mut self, bytes: &[u8]) {
        self.tail.extend_from_slice(bytes);
        let excess = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..excess);
    }

    /// The text kept, under a heading naming the stream `label`, followed by `note` in brackets: the whole stream, or its start and end around a mark of what was cut, with `note` telling how much was shown.
    fn render(&self, label: &str, note: impl FnOnce(Option<(usize, usize)>) -> String) -> String {
        let head = String::from_utf8_lossy(&self.head);
        if self.fits() {
            let head = head.trim_end();
            let gap = if head.is_empty() { "" } else { "\n" };
            return format!("--- {label} ---\n{head}{gap}[{}]", note(None));
        }
        format!(
            "--- {label} ---\n{head}\n[... {cut} bytes cut ...]\n{tail}\n[{note}]",
            cut = self.total - self.head.len() - self.tail.len(),
            tail = String::from_utf8_lossy(&self.tail).trim_end(),
            note = note(Some((self.head.len(), self.tail.len()))),
        )
    }
}

/// The condensed form of a stream, built as the stream arrives.
struct Condensed {
    /// What condenses the stream.
    condenser: Condenser,
    /// The condensed text kept to show.
    shown: Window,
}

/// One output stream of a command as it is read.
pub(crate) struct Capture {
    /// The name the stream goes by, such as `stdout`.
    label: &'static str,
    /// Where the stream is saved whole once it is too long to show, or once condensing leaves something out.
    spill: Spill,
    /// The raw stream kept to show.
    raw: Window,
    /// The file the raw stream is being written to once it is too long to keep whole.
    file: Option<File>,
    /// The file the raw stream is saved in, or why it could not be saved.
    saved: Option<Result<PathBuf, String>>,
    /// The condensed form of the stream, when it is condensed.
    condensed: Option<Condensed>,
}

impl Capture {
    /// An empty capture of the stream called `label`, saving into `spill` when it grows too long.
    pub(crate) fn new(label: &'static str, spill: Spill) -> Self {
        Self {
            label,
            spill,
            raw: Window::default(),
            file: None,
            saved: None,
            condensed: None,
        }
    }

    /// The capture condensing the stream with `condenser` for the model to read.
    pub(crate) fn condensed_by(mut self, condenser: Condenser) -> Self {
        self.condensed = Some(Condensed {
            condenser,
            shown: Window::default(),
        });
        self
    }

    /// Takes in `bytes`, the next part of the stream.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        match self.raw.push(bytes) {
            Some(whole) => self.start_saving(&whole),
            None => self.write(bytes),
        }
        if let Some(condensed) = &mut self.condensed {
            let text = condensed.condenser.push(bytes);
            condensed.shown.push(text.as_bytes());
        }
    }

    /// Starts saving the stream, once it is too long to keep whole, by writing `whole`, everything so far, to a new file.
    fn start_saving(&mut self, whole: &[u8]) {
        match self.spill.create(&self.file_label()) {
            Ok((path, file)) => {
                self.file = Some(file);
                self.saved = Some(Ok(path));
                self.write(whole);
            }
            Err(error) => self.saved = Some(Err(error.to_string())),
        }
    }

    /// The name saved files of the stream are made from.
    fn file_label(&self) -> String {
        format!("bash-{}", self.label)
    }

    /// Appends `bytes` to the file the stream is being saved in, giving up on the file when writing fails.
    fn write(&mut self, bytes: &[u8]) {
        if let Some(file) = &mut self.file
            && let Err(error) = file.write_all(bytes)
        {
            self.file = None;
            self.saved = Some(Err(error.to_string()));
        }
    }

    /// The stream as the model reads it, under a heading naming it; nothing when the stream was empty.
    ///
    /// The condensed text is shown when condensing left something out and the raw stream is saved whole where the model can read it; otherwise the raw stream is shown, whole, or its start and end with a note of how long it was and where the whole of it is. Call it once, after the last [`Capture::push`].
    pub(crate) fn render(&mut self) -> Option<String> {
        if self.raw.total == 0 {
            return None;
        }
        if let Some(text) = self.render_condensed() {
            return Some(text);
        }
        if self.raw.fits() {
            let head = String::from_utf8_lossy(&self.raw.head);
            return Some(format!("--- {} ---\n{}", self.label, head.trim_end()));
        }
        let place = self.place();
        let total = self.raw.total;
        let label = self.label;
        Some(self.raw.render(label, |shown| {
            let (head, tail) = shown.unwrap_or_default();
            format!("{label} truncated: {total} bytes in all, showing the first {head} and the last {tail}; {place}")
        }))
    }

    /// The condensed stream with a note of how much was left out and where the whole of it is, when condensing left anything out and nothing is lost by showing it.
    fn render_condensed(&mut self) -> Option<String> {
        let mut condensed = self.condensed.take()?;
        let rest = condensed.condenser.finish();
        condensed.shown.push(rest.as_bytes());
        if !condensed.condenser.removed() || !self.save_whole() {
            return None;
        }
        let label = self.label;
        let lines_in = condensed.condenser.lines_in();
        let lines_out = condensed.condenser.lines_out();
        let total = condensed.shown.total;
        let place = self.place();
        Some(condensed.shown.render(label, |shown| match shown {
            None => format!("{label} condensed: {lines_in} lines → {lines_out}; {place}"),
            Some((head, tail)) => format!(
                "{label} condensed: {lines_in} lines → {lines_out}, {total} bytes, showing the first {head} and the last {tail}; {place}"
            ),
        }))
    }

    /// Makes sure the raw stream is saved whole, saving it now when it was short enough to keep, and says whether showing less of it loses nothing: it is saved, or it was too long to keep anyway.
    fn save_whole(&mut self) -> bool {
        if self.saved.is_none() {
            let label = self.file_label();
            self.saved = Some(
                self.spill
                    .save(&label, &self.raw.head)
                    .map_err(|error| error.to_string()),
            );
        }
        matches!(self.saved, Some(Ok(_))) || !self.raw.fits()
    }

    /// Where the whole stream is, or why it could not be saved, for the model.
    fn place(&self) -> String {
        match &self.saved {
            Some(Ok(path)) => read_hint(path),
            Some(Err(error)) => format!("the whole stream could not be saved: {error}"),
            None => "the whole stream was not saved".to_owned(),
        }
    }
}

#[cfg(test)]
/// Tests for capturing a stream.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// A short stream is shown whole and nothing is saved.
    #[test]
    fn short_streams_are_shown_whole() {
        let dir = scratch("capture-short");
        let mut capture = Capture::new("stdout", Spill::new(dir.join("out")));
        capture.push(b"hello\n");
        assert_eq!(capture.render().unwrap(), "--- stdout ---\nhello");
        assert!(!dir.join("out").exists());
        assert!(Capture::new("stderr", Spill::disabled()).render().is_none());
    }

    /// A long stream keeps its start and end to show and is saved whole, however it arrives.
    #[test]
    fn long_streams_are_saved_whole() {
        let dir = scratch("capture-long");
        let mut capture = Capture::new("stdout", Spill::new(dir.join("out")));
        let lines: String = (0..20_000).map(|n| format!("line {n}\n")).collect();
        for chunk in lines.as_bytes().chunks(8192) {
            capture.push(chunk);
        }
        let text = capture.render().unwrap();
        assert!(text.starts_with("--- stdout ---\nline 0\nline 1\n"));
        assert!(text.contains("line 19999\n[stdout truncated: "));
        assert!(text.contains(&format!(
            "{} bytes in all, showing the first {HEAD_BYTES} and the last {TAIL_BYTES}",
            lines.len()
        )));
        let Some(Ok(path)) = &capture.saved else {
            panic!("the stream was not saved");
        };
        assert!(text.contains(&format!("the whole output is in {}", path.display())));
        assert_eq!(std::fs::read_to_string(path).unwrap(), lines);
    }

    /// A stream that cannot be saved is still cut, and says so.
    #[test]
    fn unsaved_streams_say_why() {
        let mut capture = Capture::new("stderr", Spill::disabled());
        capture.push(&vec![b'x'; MAX_STREAM_BYTES + 10]);
        let text = capture.render().unwrap();
        assert!(text.contains("[... 10 bytes cut ...]"));
        assert!(text.contains("the whole stream could not be saved"));
    }
}
