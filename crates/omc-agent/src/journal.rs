//! The history of one conversation, kept in an append-only file of JSON lines.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use omc_providers::Message;
use thiserror::Error;

/// Why a history file could not be opened.
#[derive(Debug, Error)]
pub enum JournalError {
    /// The file or its folder could not be read or written.
    #[error("{}: {source}", path.display())]
    Io {
        /// The file or folder.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// The file holds something that is not a history.
    #[error("{}: line {line}: {reason}", path.display())]
    Corrupt {
        /// The file.
        path: PathBuf,
        /// The line, counted from 1.
        line: usize,
        /// What is wrong with it.
        reason: String,
    },
}

/// One line of a history file.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Record {
    /// A message added to the history.
    Message(Message),
    /// The history is cut back to this many messages.
    Cut(usize),
}

/// The file a conversation's history is appended to.
pub struct Journal {
    /// The file the history is in.
    path: PathBuf,
    /// The open file and the number of cut-backs written so far.
    state: Mutex<State>,
}

/// What the journal changes as it is written.
struct State {
    /// The file, open for appending.
    file: File,
    /// How many cut-backs were written; a [`Scribe`] from before the last one is out of date.
    epoch: u64,
}

/// A way to append to a [`Journal`] that stops working once the history has been cut back past it.
///
/// A turn that is cancelled may still run for an instant after the cut-back was written; its late records must not land after it.
#[derive(Clone)]
pub struct Scribe {
    /// The journal written to.
    journal: Arc<Journal>,
    /// The epoch this scribe was made in.
    epoch: u64,
}

impl Journal {
    /// Opens the history at `path`, creating the file when it is not there, and returns it with the messages it holds.
    ///
    /// An incomplete last line, which a crash during a write leaves, is dropped from the file. When `keep` is given, only that many turns are kept; a turn is a user message and everything up to the next one. A history that ends in a prompt nothing answered, or in a tool call without its results, as a crash in the middle of a turn leaves, is cut back to before it. Whatever is cut off is recorded in the file as a cut-back.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] when the file cannot be read or opened, and [`JournalError::Corrupt`] when a line is not a record or cuts back past the start; the file is left as it was.
    pub fn open(
        path: &Path,
        keep: Option<usize>,
    ) -> Result<(Arc<Self>, Vec<Message>), JournalError> {
        let io_error = |source| JournalError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(folder) = path.parent() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(folder)
                .map_err(io_error)?;
        }
        let bytes = match std::fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(error) => return Err(io_error(error)),
        };
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1);
        let mut history = replay(path, &bytes[..complete])?;
        let saved = history.len();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)
            .map_err(io_error)?;
        if complete < bytes.len() {
            file.set_len(complete as u64).map_err(io_error)?;
        }
        if let Some(turns) = keep {
            history.truncate(end_of_turns(&history, turns));
        }
        while matches!(history.last(), Some(Message::User(_)))
            || matches!(history.last(), Some(Message::Assistant(reply)) if !reply.tool_calls.is_empty())
        {
            history.pop();
        }
        if history.len() < saved {
            write_record(&mut file, &Record::Cut(history.len())).map_err(io_error)?;
            file.sync_data().map_err(io_error)?;
        }
        let journal = Arc::new(Self {
            path: path.to_path_buf(),
            state: Mutex::new(State { file, epoch: 0 }),
        });
        Ok((journal, history))
    }

    /// The file the history is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A scribe for what is written from now until the history is next cut back.
    pub fn scribe(self: &Arc<Self>) -> Scribe {
        Scribe {
            journal: self.clone(),
            epoch: self.lock().epoch,
        }
    }

    /// Records that the history is cut back to `length` messages, syncs the file, and retires every scribe made before.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut(&self, length: usize) -> io::Result<()> {
        let mut state = self.lock();
        state.epoch += 1;
        write_record(&mut state.file, &Record::Cut(length))?;
        state.file.sync_data()
    }

    /// Takes the state, whatever a panicking writer left.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Scribe {
    /// Appends `message` to the file and flushes it; nothing is written when the history was cut back since this scribe was made.
    ///
    /// # Errors
    ///
    /// Returns the error of the write.
    pub fn append(&self, message: &Message) -> io::Result<()> {
        let mut state = self.journal.lock();
        if state.epoch != self.epoch {
            return Ok(());
        }
        write_record(&mut state.file, &Record::Message(message.clone()))
    }

    /// Syncs the file to disk, which is done when a turn ends.
    ///
    /// # Errors
    ///
    /// Returns the error of the sync.
    pub fn sync(&self) -> io::Result<()> {
        self.journal.lock().file.sync_data()
    }

    /// Records that the history is cut back to `length` messages and syncs; this scribe and the ones cloned from it stop writing.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut(&self, length: usize) -> io::Result<()> {
        {
            let state = self.journal.lock();
            if state.epoch != self.epoch {
                return Ok(());
            }
        }
        self.journal.cut(length)
    }
}

/// Writes `record` as one line with a single write, which is flushed as the file is not buffered.
fn write_record(file: &mut File, record: &Record) -> io::Result<()> {
    let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)
}

/// Builds the history the complete lines in `bytes` add up to.
///
/// # Errors
///
/// Returns [`JournalError::Corrupt`] for a line that is not a record, or a cut-back past the start or past the end.
fn replay(path: &Path, bytes: &[u8]) -> Result<Vec<Message>, JournalError> {
    let mut history = Vec::new();
    for (index, line) in bytes.split_inclusive(|&byte| byte == b'\n').enumerate() {
        let corrupt = |reason: String| JournalError::Corrupt {
            path: path.to_path_buf(),
            line: index + 1,
            reason,
        };
        match serde_json::from_slice(line).map_err(|error| corrupt(error.to_string()))? {
            Record::Message(message) => history.push(message),
            Record::Cut(length) if length <= history.len() => history.truncate(length),
            Record::Cut(length) => {
                return Err(corrupt(format!(
                    "cut back to {length} of {} messages",
                    history.len()
                )));
            }
        }
    }
    Ok(history)
}

/// The length of the first `turns` turns of `history`.
fn end_of_turns(history: &[Message], turns: usize) -> usize {
    history
        .iter()
        .enumerate()
        .filter(|(_, message)| matches!(message, Message::User(_)))
        .nth(turns)
        .map_or(history.len(), |(index, _)| index)
}

/// Tests for appending, replaying and cutting back a history.
#[cfg(test)]
mod tests {
    use omc_providers::{Reply, ToolCall, ToolResult};
    use serde_json::json;

    use super::*;

    /// A fresh path for a history file called `name`.
    fn scratch(name: &str) -> PathBuf {
        let folder =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-agent/journal");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        let _ = std::fs::remove_file(&path);
        path
    }

    /// An assistant reply that says `text`.
    fn said(text: &str) -> Message {
        Message::Assistant(Reply {
            text: text.into(),
            ..Reply::default()
        })
    }

    /// Three turns of a user and an assistant message each.
    fn three_turns() -> Vec<Message> {
        (1..=3)
            .flat_map(|n| [Message::User(format!("q{n}")), said(&format!("a{n}"))])
            .collect()
    }

    /// Appends `messages` through a fresh scribe.
    fn append_all(journal: &Arc<Journal>, messages: &[Message]) {
        let scribe = journal.scribe();
        for message in messages {
            scribe.append(message).unwrap();
        }
    }

    /// What was appended is what a later open replays, including signed thinking and encrypted reasoning.
    #[test]
    fn appended_messages_replay_unchanged() {
        let path = scratch("replay.jsonl");
        let (journal, history) = Journal::open(&path, None).unwrap();
        assert!(history.is_empty());
        let messages = vec![
            Message::User("go".into()),
            Message::Assistant(Reply {
                text: "ok".into(),
                thinking: "hm".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                }],
                opaque: vec![
                    json!({"type": "thinking", "thinking": "hm", "signature": "sig=="}),
                    json!({"type": "reasoning", "encrypted_content": "enc=="}),
                ],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: "contents".into(),
                is_error: false,
            }]),
            said("done"),
        ];
        append_all(&journal, &messages);
        drop(journal);
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert_eq!(loaded, messages);
    }

    /// A cut-back record makes the replay drop what came after the length, and later records go on from there.
    #[test]
    fn a_cut_back_drops_the_tail() {
        let path = scratch("cut.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        append_all(&journal, &three_turns());
        journal.cut(2).unwrap();
        append_all(&journal, &[Message::User("again".into()), said("ok")]);
        drop(journal);
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert_eq!(
            loaded,
            [
                three_turns()[0].clone(),
                three_turns()[1].clone(),
                Message::User("again".into()),
                said("ok")
            ]
        );
    }

    /// A scribe made before a cut-back writes nothing afterwards.
    #[test]
    fn a_retired_scribe_writes_nothing() {
        let path = scratch("retired.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        let scribe = journal.scribe();
        scribe.append(&Message::User("q".into())).unwrap();
        journal.cut(0).unwrap();
        scribe.append(&Message::User("late".into())).unwrap();
        drop(journal);
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert!(loaded.is_empty());
    }

    /// Keeping one turn after three leaves the first, and the file then replays to it without being asked.
    #[test]
    fn keeping_turns_cuts_the_file() {
        let path = scratch("keep.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        append_all(&journal, &three_turns());
        drop(journal);
        let (_, kept) = Journal::open(&path, Some(1)).unwrap();
        assert_eq!(kept, three_turns()[..2]);
        let (_, later) = Journal::open(&path, None).unwrap();
        assert_eq!(later, three_turns()[..2]);
        let (_, more) = Journal::open(&path, Some(9)).unwrap();
        assert_eq!(more, three_turns()[..2]);
    }

    /// A last line that was cut off is ignored, and the next record starts on a line of its own.
    #[test]
    fn a_truncated_last_line_is_ignored() {
        let path = scratch("truncated.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        append_all(&journal, &three_turns()[..2]);
        drop(journal);
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(br#"{"message":{"User":"cut o"#).unwrap();
        drop(file);
        let (journal, loaded) = Journal::open(&path, None).unwrap();
        assert_eq!(loaded, three_turns()[..2]);
        append_all(&journal, &[Message::User("next".into()), said("ok")]);
        drop(journal);
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert_eq!(loaded.len(), 4);
    }

    /// A history that stopped on a tool call, or on a prompt nothing answered, is cut back before it.
    #[test]
    fn a_dangling_tool_call_is_dropped() {
        let path = scratch("dangling.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        append_all(
            &journal,
            &[
                Message::User("go".into()),
                Message::Assistant(Reply {
                    tool_calls: vec![ToolCall {
                        id: "c".into(),
                        name: "bash".into(),
                        arguments: json!({}),
                    }],
                    ..Reply::default()
                }),
            ],
        );
        drop(journal);
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert!(loaded.is_empty());
    }

    /// A file that is not a history fails to open and is left as it was.
    #[test]
    fn an_unreadable_file_is_an_error_and_untouched() {
        let path = scratch("garbage.jsonl");
        let content = b"not json\n{\"cut\":0}\n";
        std::fs::write(&path, content).unwrap();
        let Err(JournalError::Corrupt { line, .. }) = Journal::open(&path, None) else {
            panic!("the file opened");
        };
        assert_eq!(line, 1);
        assert_eq!(std::fs::read(&path).unwrap(), content);
        std::fs::write(&path, b"{\"cut\":3}\n").unwrap();
        assert!(Journal::open(&path, None).is_err());
        std::fs::write(&path, [0xff, 0xfe, b'\n']).unwrap();
        assert!(Journal::open(&path, None).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), [0xff, 0xfe, b'\n']);
    }

    /// A path that cannot be read as a file is an error.
    #[test]
    fn a_directory_is_an_error() {
        let path = scratch("folder.jsonl");
        std::fs::create_dir_all(&path).unwrap();
        assert!(matches!(
            Journal::open(&path, None),
            Err(JournalError::Io { .. })
        ));
        std::fs::remove_dir(&path).unwrap();
    }
}
