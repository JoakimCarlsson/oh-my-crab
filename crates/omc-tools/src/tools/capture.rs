//! One output stream of a command: kept whole while it is short, and once it grows too long, saved whole to a file while only its start and end are kept to show.

use std::fs::File;
use std::io::Write as _;
use std::path::PathBuf;

use crate::Spill;
use crate::spill::read_hint;

/// Most bytes of a stream shown to the model.
pub(crate) const MAX_STREAM_BYTES: usize = 30_000;

/// Bytes from the start of a stream shown when it is cut; the rest of what is shown is its end, where errors and summaries usually are.
pub(crate) const HEAD_BYTES: usize = 4_000;

/// Bytes from the end of a stream shown when it is cut.
const TAIL_BYTES: usize = MAX_STREAM_BYTES - HEAD_BYTES;

/// One output stream of a command as it is read.
pub(crate) struct Capture {
    /// The name the stream goes by, such as `stdout`.
    label: &'static str,
    /// Where the stream is saved whole once it is too long to show.
    spill: Spill,
    /// The whole stream while it fits, and its first [`HEAD_BYTES`] once it does not.
    head: Vec<u8>,
    /// The last [`TAIL_BYTES`] of the stream once it does not fit.
    tail: Vec<u8>,
    /// How many bytes the stream produced.
    total: usize,
    /// The file the stream is saved in once it does not fit, or why it could not be saved.
    saved: Option<Result<(PathBuf, File), String>>,
}

impl Capture {
    /// An empty capture of the stream called `label`, saving into `spill` when it grows too long.
    pub(crate) fn new(label: &'static str, spill: Spill) -> Self {
        Self {
            label,
            spill,
            head: Vec::new(),
            tail: Vec::new(),
            total: 0,
            saved: None,
        }
    }

    /// Takes in `bytes`, the next part of the stream.
    pub(crate) fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len();
        if self.saved.is_some() {
            self.write(bytes);
            self.keep_tail(bytes);
        } else if self.head.len() + bytes.len() <= MAX_STREAM_BYTES {
            self.head.extend_from_slice(bytes);
        } else {
            self.overflow(bytes);
        }
    }

    /// Starts saving the stream, once `bytes` make it too long to show: writes what was kept and `bytes` to a new file and keeps only the start and the end.
    fn overflow(&mut self, bytes: &[u8]) {
        let mut whole = std::mem::take(&mut self.head);
        whole.extend_from_slice(bytes);
        self.saved = Some(
            self.spill
                .create(&format!("bash-{}", self.label))
                .map_err(|error| error.to_string()),
        );
        self.write(&whole);
        self.head = whole[..HEAD_BYTES].to_vec();
        self.keep_tail(&whole[HEAD_BYTES..]);
    }

    /// Appends `bytes` to the file the stream is saved in, giving up on the file when writing fails.
    fn write(&mut self, bytes: &[u8]) {
        if let Some(Ok((_, file))) = &mut self.saved
            && let Err(error) = file.write_all(bytes)
        {
            self.saved = Some(Err(error.to_string()));
        }
    }

    /// Adds `bytes` to the end kept to show, dropping what falls out of it.
    fn keep_tail(&mut self, bytes: &[u8]) {
        self.tail.extend_from_slice(bytes);
        let excess = self.tail.len().saturating_sub(TAIL_BYTES);
        self.tail.drain(..excess);
    }

    /// The stream as the model reads it, under a heading naming it: whole, or its start and end with a note of how long it was and where the whole of it is; nothing when the stream was empty.
    pub(crate) fn render(&self) -> Option<String> {
        if self.total == 0 {
            return None;
        }
        let head = String::from_utf8_lossy(&self.head);
        let Some(saved) = &self.saved else {
            return Some(format!("--- {} ---\n{}", self.label, head.trim_end()));
        };
        let place = match saved {
            Ok((path, _)) => read_hint(path),
            Err(error) => format!("the whole stream could not be saved: {error}"),
        };
        Some(format!(
            "--- {label} ---\n{head}\n[... {cut} bytes cut ...]\n{tail}\n[{label} truncated: {total} bytes in all, showing the first {shown_head} and the last {shown_tail}; {place}]",
            label = self.label,
            cut = self.total - self.head.len() - self.tail.len(),
            tail = String::from_utf8_lossy(&self.tail).trim_end(),
            total = self.total,
            shown_head = self.head.len(),
            shown_tail = self.tail.len(),
        ))
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
        let Some(Ok((path, _))) = &capture.saved else {
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
