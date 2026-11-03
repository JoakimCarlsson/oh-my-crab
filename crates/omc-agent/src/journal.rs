//! The history of one conversation, kept in an append-only file of JSON lines.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use omc_providers::Message;
use omc_tools::FileChange;
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

/// What a client said about a turn when it prompted: the id it knows the turn by and what the reader wrote.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Mark {
    /// The id the client gave the turn.
    pub id: String,
    /// What the reader wrote, as they wrote it.
    pub text: String,
    /// The names of the files the reader attached.
    #[serde(default)]
    pub files: Vec<String>,
}

/// What a saved conversation holds besides its messages: its turns' marks, its name, the choices made about it and its notebook.
#[derive(Clone, Debug, Default)]
pub struct Saved {
    /// The messages.
    pub history: Vec<Message>,
    /// The marks of the turns, with the position of the message each starts at.
    pub marks: Vec<Marked>,
    /// The turns that were stopped or failed, oldest first.
    pub lost: Vec<Lost>,
    /// The conversation's name, when it has one.
    pub title: Option<String>,
    /// The latest value of each setting that was chosen, oldest choice first.
    pub settings: Vec<(String, String)>,
    /// The notebook.
    pub notes: String,
    /// The files each tool call changed, by the id of the call.
    pub changes: std::collections::HashMap<String, Vec<FileChange>>,
    /// What the harness last found out and saved under each key, such as the settings it offers; the latest value of each, oldest first.
    pub facts: Vec<(String, serde_json::Value)>,
}

/// A turn that was stopped or failed, which is no part of the history the model is given but is part of the conversation the client shows.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Lost {
    /// The length the history had when the turn was lost, which is where the turn would have started.
    pub at: usize,
    /// What the client said about the turn when it prompted.
    pub mark: Mark,
    /// Why the turn ended: [`STOPPED`] for a turn the reader stopped, and the failure for one that failed.
    pub reason: String,
}

/// The reason given for a turn the reader stopped.
pub const STOPPED: &str = "stopped";

/// The reason given for a turn that was still running when the conversation stopped.
pub const INTERRUPTED: &str = "interrupted";

impl Saved {
    /// The value saved under `key`, read as a `T`, or what `T` is by default when nothing readable is saved there.
    pub fn fact<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        self.facts
            .iter()
            .find(|(held, _)| held == key)
            .and_then(|(_, value)| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default()
    }

    /// The context window saved with the conversation, when one was.
    pub fn window(&self) -> Option<u64> {
        self.facts
            .iter()
            .find(|(held, _)| held == "window")
            .and_then(|(_, value)| value.as_u64())
    }
}

/// The mark of a turn and the position in the history of the message that starts the turn.
pub type Marked = (usize, Mark);

/// One line of a history file, borrowing the message it writes.
#[derive(serde::Serialize)]
#[serde(rename_all = "lowercase")]
enum Written<'a> {
    /// The next message starts the turn this describes.
    Turn(&'a Mark),
    /// A message added to the history.
    Message(&'a Message),
    /// The history is cut back to this many messages.
    Cut(usize),
    /// The notebook now holds this text.
    Notes(&'a str),
    /// The conversation is named this.
    Title(&'a str),
    /// The setting named first was given the value named second.
    Setting(&'a str, &'a str),
    /// A turn was stopped or failed.
    Lost(&'a Lost),
    /// The lost turn with this id, and every lost turn after it, is gone.
    Forget(&'a str),
    /// The tool call with this id changed these files.
    Changes(&'a str, &'a [FileChange]),
    /// The value saved under this key is now this.
    Fact(&'a str, &'a serde_json::Value),
}

/// One line of a history file, as it is read.
#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
enum Record {
    /// The next message starts the turn this describes.
    Turn(Mark),
    /// A message added to the history.
    Message(Message),
    /// The history is cut back to this many messages.
    Cut(usize),
    /// The notebook now holds this text.
    Notes(String),
    /// The conversation is named this.
    Title(String),
    /// The setting named first was given the value named second.
    Setting(String, String),
    /// A turn was stopped or failed.
    Lost(Lost),
    /// The lost turn with this id, and every lost turn after it, is gone.
    Forget(String),
    /// The tool call with this id changed these files.
    Changes(String, Vec<FileChange>),
    /// The value saved under this key is now this.
    Fact(String, serde_json::Value),
}

/// The file a conversation's history is appended to.
pub struct Journal {
    /// The file the history is in.
    path: PathBuf,
    /// The notebook as the file held it when it was opened.
    notes: String,
    /// The open file and the number of cut-backs written so far.
    state: Mutex<State>,
}

/// What the journal changes as it is written.
struct State {
    /// The file, open for appending.
    file: File,
    /// How many cut-backs were asked for; a [`Scribe`] from before the last one is out of date.
    epoch: u64,
    /// A cut-back that could not be written yet, which is tried again before anything else is written so that a failed turn never comes back.
    pending: Option<usize>,
    /// The marks of the turns in the history, in order.
    marks: Vec<Marked>,
    /// The turns that were stopped or failed, oldest first.
    lost: Vec<Lost>,
    /// The conversation's name.
    title: Option<String>,
    /// The latest value of each chosen setting, oldest choice first.
    settings: Vec<(String, String)>,
    /// The files each tool call changed, by the id of the call.
    changes: std::collections::HashMap<String, Vec<FileChange>>,
    /// The latest value saved under each key, oldest first.
    facts: Vec<(String, serde_json::Value)>,
}

impl State {
    /// Writes the cut-back that is waiting, if there is one.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync; the cut-back then stays waiting.
    fn settle(&mut self) -> io::Result<()> {
        if let Some(length) = self.pending {
            write_record(&mut self.file, &Written::Cut(length))?;
            self.file.sync_data()?;
            self.pending = None;
        }
        Ok(())
    }
}

/// A way to append to a [`Journal`] that stops working once the history has been cut back past it, or once the journal is dropped, so that a turn that is still winding down does not keep the file locked.
///
/// A turn that is cancelled may still run for an instant after the cut-back was written; its late records must not land after it.
#[derive(Clone)]
pub struct Scribe {
    /// The journal written to.
    journal: Weak<Journal>,
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
    /// Returns [`JournalError::Io`] when the file cannot be read or opened, is not a regular file, or is open in another conversation, and [`JournalError::Corrupt`] when a line is not a record or cuts back past the start; the file is left as it was.
    pub fn open(
        path: &Path,
        keep: Option<usize>,
    ) -> Result<(Arc<Self>, Vec<Message>), JournalError> {
        Self::open_before(path, keep, None)
    }

    /// Opens the history like [`Journal::open`], and when `before` names the id of a turn, cuts the history back to before that turn as well, whichever of that and `keep` leaves less.
    ///
    /// # Errors
    ///
    /// Returns what [`Journal::open`] does, and [`JournalError::Io`] when no turn has the id `before`.
    pub fn open_before(
        path: &Path,
        keep: Option<usize>,
        before: Option<&str>,
    ) -> Result<(Arc<Self>, Vec<Message>), JournalError> {
        let io_error = |source| JournalError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(folder) = path.parent() {
            create_folder(folder).map_err(io_error)?;
        }
        let mut file = open_private(path).map_err(io_error)?;
        if !file.metadata().map_err(io_error)?.is_file() {
            return Err(io_error(io::Error::other("not a regular file")));
        }
        file.try_lock().map_err(|error| {
            io_error(match error {
                std::fs::TryLockError::WouldBlock => {
                    io::Error::other("the conversation is open elsewhere")
                }
                std::fs::TryLockError::Error(error) => error,
            })
        })?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io_error)?;
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1);
        let Saved {
            mut history,
            mut marks,
            mut lost,
            title,
            settings,
            notes,
            changes,
            facts,
        } = replay(path, &bytes[..complete])?;
        let saved = history.len();
        if complete < bytes.len() {
            file.set_len(complete as u64).map_err(io_error)?;
        }
        file.seek(SeekFrom::End(0)).map_err(io_error)?;
        if let Some(turns) = keep {
            history.truncate(end_of_turns(&history, turns));
            lost.retain(|held| held.at <= history.len());
        }
        let mut forgotten: Option<String> = None;
        if let Some(id) = before {
            if let Some((at, _)) = marks.iter().find(|(_, mark)| mark.id == id) {
                history.truncate(history.len().min(*at));
                lost.retain(|held| held.at <= history.len());
            } else if let Some(index) = lost.iter().position(|held| held.mark.id == id) {
                history.truncate(history.len().min(lost[index].at));
                lost.truncate(index);
                forgotten = Some(id.to_owned());
            } else {
                return Err(io_error(io::Error::other(format!(
                    "no turn has the id {id}"
                ))));
            }
        }
        let intended = history.len();
        while matches!(history.last(), Some(Message::User(_)))
            || matches!(history.last(), Some(Message::Assistant(reply)) if !reply.tool_calls.is_empty())
        {
            history.pop();
        }
        let interrupted: Vec<Lost> = marks
            .iter()
            .filter(|(at, _)| *at >= history.len() && *at < intended)
            .map(|(_, mark)| Lost {
                at: history.len(),
                mark: mark.clone(),
                reason: INTERRUPTED.to_owned(),
            })
            .collect();
        if let Some(id) = &forgotten {
            write_record(&mut file, &Written::Forget(id)).map_err(io_error)?;
        }
        if history.len() < saved {
            write_record(&mut file, &Written::Cut(history.len())).map_err(io_error)?;
            for turn in &interrupted {
                write_record(&mut file, &Written::Lost(turn)).map_err(io_error)?;
            }
            file.sync_data().map_err(io_error)?;
        }
        lost.extend(interrupted);
        marks.retain(|(at, _)| *at < history.len());
        let journal = Arc::new(Self {
            path: path.to_path_buf(),
            notes,
            state: Mutex::new(State {
                file,
                epoch: 0,
                pending: None,
                marks,
                lost,
                title,
                settings,
                changes,
                facts,
            }),
        });
        Ok((journal, history))
    }

    /// Reads the history in the file at `path` without opening it for writing or locking it, so that a conversation that is open elsewhere can be read; an incomplete last line is left out.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] when the file cannot be read, and [`JournalError::Corrupt`] when a line is not a record.
    pub fn read(path: &Path) -> Result<Saved, JournalError> {
        let bytes = std::fs::read(path).map_err(|source| JournalError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1);
        replay(path, &bytes[..complete])
    }

    /// Names the conversation saved at `path` `title`, which works on a conversation nobody has open; a turn that was left unanswered is cut back, as it is whenever the history is opened.
    ///
    /// # Errors
    ///
    /// Returns what [`Journal::open`] does, and [`JournalError::Io`] when the name cannot be written.
    pub fn rename(path: &Path, title: &str) -> Result<(), JournalError> {
        let (journal, _) = Self::open(path, None)?;
        journal
            .save_title(title)
            .map_err(|source| JournalError::Io {
                path: path.to_path_buf(),
                source,
            })
    }

    /// The conversation's name, when it has one.
    pub fn title(&self) -> Option<String> {
        self.lock().title.clone()
    }

    /// The latest value of each setting that was chosen, oldest choice first.
    pub fn settings(&self) -> Vec<(String, String)> {
        self.lock().settings.clone()
    }

    /// Records that the conversation is named `title` and syncs.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of a cut-back that was waiting to be written.
    pub fn save_title(&self, title: &str) -> io::Result<()> {
        let mut state = self.lock();
        state.settle()?;
        write_record(&mut state.file, &Written::Title(title))?;
        state.title = Some(title.to_owned());
        state.file.sync_data()
    }

    /// Records that the setting `id` was given the value `value` and syncs.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of a cut-back that was waiting to be written.
    pub fn save_setting(&self, id: &str, value: &str) -> io::Result<()> {
        let mut state = self.lock();
        state.settle()?;
        write_record(&mut state.file, &Written::Setting(id, value))?;
        state.settings.retain(|(held, _)| held != id);
        state.settings.push((id.to_owned(), value.to_owned()));
        state.file.sync_data()
    }

    /// The files each tool call changed, by the id of the call.
    pub fn changes(&self) -> std::collections::HashMap<String, Vec<FileChange>> {
        self.lock().changes.clone()
    }

    /// The value last saved under `key`, when there is one.
    pub fn fact(&self, key: &str) -> Option<serde_json::Value> {
        self.lock()
            .facts
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.clone())
    }

    /// Saves `value` under `key`, replacing what was saved there, unless it is what is saved already.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of a cut-back that was waiting to be written.
    pub fn save_fact(&self, key: &str, value: &serde_json::Value) -> io::Result<()> {
        let mut state = self.lock();
        if state
            .facts
            .iter()
            .any(|(held, old)| held == key && old == value)
        {
            return Ok(());
        }
        state.settle()?;
        write_record(&mut state.file, &Written::Fact(key, value))?;
        state.facts.retain(|(held, _)| held != key);
        state.facts.push((key.to_owned(), value.clone()));
        state.file.sync_data()
    }

    /// Records that the tool call `call` changed `files`.
    ///
    /// # Errors
    ///
    /// Returns the error of the write, or of a cut-back that was waiting to be written.
    pub fn save_changes(&self, call: &str, files: &[FileChange]) -> io::Result<()> {
        let mut state = self.lock();
        state.settle()?;
        write_record(&mut state.file, &Written::Changes(call, files))?;
        state.changes.insert(call.to_owned(), files.to_vec());
        Ok(())
    }

    /// The turns that were stopped or failed, oldest first.
    pub fn lost(&self) -> Vec<Lost> {
        self.lock().lost.clone()
    }

    /// The marks of the turns in the history now, in order.
    pub fn marks(&self) -> Vec<Marked> {
        self.lock().marks.clone()
    }

    /// Records that the turn that starts with the next message, which will be message number `at`, is the one `mark` describes.
    ///
    /// # Errors
    ///
    /// Returns the error of the write, or of a cut-back that was waiting to be written.
    pub fn mark(&self, at: usize, mark: &Mark) -> io::Result<()> {
        let mut state = self.lock();
        state.settle()?;
        write_record(&mut state.file, &Written::Turn(mark))?;
        state.marks.push((at, mark.clone()));
        Ok(())
    }

    /// The notebook the history was opened with, empty when none was saved.
    pub fn notes(&self) -> &str {
        &self.notes
    }

    /// Records that the notebook now holds `text` and syncs.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of a cut-back that was waiting to be written.
    pub fn save_notes(&self, text: &str) -> io::Result<()> {
        let mut state = self.lock();
        state.settle()?;
        write_record(&mut state.file, &Written::Notes(text))?;
        state.file.sync_data()
    }

    /// The file the history is kept in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// A scribe for what is written from now until the history is next cut back.
    pub fn scribe(self: &Arc<Self>) -> Scribe {
        Scribe {
            journal: Arc::downgrade(self),
            epoch: self.lock().epoch,
        }
    }

    /// Records that the history is cut back to `length` messages, syncs the file, and retires every scribe made before.
    ///
    /// When the write fails the cut-back is kept and written before the next record of any scribe, which fails until it can be.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut(&self, length: usize) -> io::Result<()> {
        let mut state = self.lock();
        state.epoch += 1;
        state.pending = Some(length);
        state.marks.retain(|(at, _)| *at < length);
        state.settle()
    }

    /// Records that the history is cut back to `length` messages like [`Journal::cut`], and that the turns it cut off were lost for `reason`, so that a client can still show them.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut_losing(&self, length: usize, reason: &str) -> io::Result<()> {
        let removed: Vec<Lost> = {
            let state = self.lock();
            state
                .marks
                .iter()
                .filter(|(at, _)| *at >= length)
                .map(|(_, mark)| Lost {
                    at: length,
                    mark: mark.clone(),
                    reason: reason.to_owned(),
                })
                .collect()
        };
        self.cut(length)?;
        let mut state = self.lock();
        for turn in removed {
            write_record(&mut state.file, &Written::Lost(&turn))?;
            state.lost.push(turn);
        }
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
    /// Returns the error of the write, or of a cut-back that was waiting to be written.
    pub fn append(&self, message: &Message) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        let mut state = journal.lock();
        if state.epoch != self.epoch {
            return Ok(());
        }
        state.settle()?;
        write_record(&mut state.file, &Written::Message(message))
    }

    /// Records that the notebook now holds `text` and syncs; nothing is written when the history was cut back since this scribe was made.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of a cut-back that was waiting to be written.
    pub fn notes(&self, text: &str) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        let mut state = journal.lock();
        if state.epoch != self.epoch {
            return Ok(());
        }
        state.settle()?;
        write_record(&mut state.file, &Written::Notes(text))?;
        state.file.sync_data()
    }

    /// Starts the history over with `first` as its only message: the old messages are cut off and `first` is appended, in one step that keeps this scribe writing.
    ///
    /// # Errors
    ///
    /// Returns the error of the write, or of a cut-back that was waiting to be written.
    pub fn restart(&self, first: &Message) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        let mut state = journal.lock();
        if state.epoch != self.epoch {
            return Ok(());
        }
        state.settle()?;
        write_record(&mut state.file, &Written::Cut(0))?;
        state.marks.clear();
        write_record(&mut state.file, &Written::Message(first))
    }

    /// Replaces the messages from index `keep` on with `messages`: the history is cut back to `keep` messages and `messages` are appended, in one step that keeps this scribe writing.
    ///
    /// # Errors
    ///
    /// Returns the error of the write, or of a cut-back that was waiting to be written.
    pub fn replace(&self, keep: usize, messages: &[Message]) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        let mut state = journal.lock();
        if state.epoch != self.epoch {
            return Ok(());
        }
        state.settle()?;
        write_record(&mut state.file, &Written::Cut(keep))?;
        state.marks.retain(|(at, _)| *at < keep);
        for message in messages {
            write_record(&mut state.file, &Written::Message(message))?;
        }
        Ok(())
    }

    /// Syncs the file to disk, which is done when a turn ends.
    ///
    /// # Errors
    ///
    /// Returns the error of the sync.
    pub fn sync(&self) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        let mut state = journal.lock();
        state.settle()?;
        state.file.sync_data()
    }

    /// Records that the history is cut back to `length` messages and syncs; this scribe and the ones cloned from it stop writing.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut(&self, length: usize) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        if journal.lock().epoch != self.epoch {
            return Ok(());
        }
        journal.cut(length)
    }

    /// Records that the tool call `call` changed `files`, unless the history was cut back past this scribe already.
    ///
    /// # Errors
    ///
    /// Returns the error of the write.
    pub fn changes(&self, call: &str, files: &[FileChange]) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        if journal.lock().epoch != self.epoch {
            return Ok(());
        }
        journal.save_changes(call, files)
    }

    /// Cuts the history back like [`Journal::cut_losing`], unless it was cut back past this scribe already.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync.
    pub fn cut_losing(&self, length: usize, reason: &str) -> io::Result<()> {
        let Some(journal) = self.journal.upgrade() else {
            return Ok(());
        };
        if journal.lock().epoch != self.epoch {
            return Ok(());
        }
        journal.cut_losing(length, reason)
    }
}

/// Creates `folder` and its parents, readable by the owner only on systems that have file modes.
///
/// # Errors
///
/// Returns the error of the creation.
fn create_folder(folder: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(folder)
}

/// Opens `path` for reading and writing, creating it readable by the owner only on systems that have file modes.
///
/// The file is not opened in append mode, because shortening it needs the right to write anywhere in it on Windows; the journal seeks to the end itself, and holds the only lock on the file.
///
/// # Errors
///
/// Returns the error of the open.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Writes `record` as one line with a single write, which is flushed as the file is not buffered.
fn write_record(file: &mut File, record: &Written<'_>) -> io::Result<()> {
    let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    file.write_all(&line)
}

/// Builds the saved conversation the complete lines in `bytes` add up to.
///
/// # Errors
///
/// Returns [`JournalError::Corrupt`] for a line that is not a record, or a cut-back past the start or past the end.
fn replay(path: &Path, bytes: &[u8]) -> Result<Saved, JournalError> {
    let mut history = Vec::new();
    let mut notes = String::new();
    let mut title = None;
    let mut settings: Vec<(String, String)> = Vec::new();
    let mut marks: Vec<Marked> = Vec::new();
    let mut lost: Vec<Lost> = Vec::new();
    let mut changes: std::collections::HashMap<String, Vec<FileChange>> =
        std::collections::HashMap::new();
    let mut facts: Vec<(String, serde_json::Value)> = Vec::new();
    let mut waiting: Option<Mark> = None;
    for (index, line) in bytes.split_inclusive(|&byte| byte == b'\n').enumerate() {
        let corrupt = |reason: String| JournalError::Corrupt {
            path: path.to_path_buf(),
            line: index + 1,
            reason,
        };
        match serde_json::from_slice(line).map_err(|error| corrupt(error.to_string()))? {
            Record::Turn(mark) => waiting = Some(mark),
            Record::Message(message) => {
                if matches!(message, Message::User(_))
                    && let Some(mark) = waiting.take()
                {
                    marks.push((history.len(), mark));
                }
                history.push(message);
            }
            Record::Notes(text) => notes = text,
            Record::Title(text) => title = Some(text),
            Record::Setting(id, value) => {
                settings.retain(|(held, _)| *held != id);
                settings.push((id, value));
            }
            Record::Lost(turn) => lost.push(turn),
            Record::Changes(call, files) => {
                changes.insert(call, files);
            }
            Record::Fact(key, value) => {
                facts.retain(|(held, _)| *held != key);
                facts.push((key, value));
            }
            Record::Forget(id) => {
                if let Some(index) = lost.iter().position(|held| held.mark.id == id) {
                    lost.truncate(index);
                }
            }
            Record::Cut(length) if length <= history.len() => {
                history.truncate(length);
                marks.retain(|(at, _)| *at < length);
                lost.retain(|held| held.at <= length);
                waiting = None;
            }
            Record::Cut(length) => {
                return Err(corrupt(format!(
                    "cut back to {length} of {} messages",
                    history.len()
                )));
            }
        }
    }
    Ok(Saved {
        history,
        marks,
        lost,
        title,
        settings,
        notes,
        changes,
        facts,
    })
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
        drop(scribe);
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

    /// A history open in one conversation cannot be opened in another until the first lets go of it.
    #[test]
    fn a_history_opens_once_at_a_time() {
        let path = scratch("locked.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        assert!(matches!(
            Journal::open(&path, None),
            Err(JournalError::Io { .. })
        ));
        drop(journal);
        assert!(Journal::open(&path, None).is_ok());
    }

    /// A record written before a field was added to the message types still loads.
    #[test]
    fn records_without_newer_fields_load() {
        let path = scratch("older.jsonl");
        std::fs::write(
            &path,
            "{\"message\":{\"User\":\"q\"}}\n{\"message\":{\"Assistant\":{\"text\":\"a\"}}}\n",
        )
        .unwrap();
        let (_, loaded) = Journal::open(&path, None).unwrap();
        assert_eq!(loaded, [Message::User("q".into()), said("a")]);
    }

    /// The last notebook written comes back when the file is opened again, and cuts do not touch it.
    #[test]
    fn notes_survive_a_reopen() {
        let path = scratch("notes.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        assert_eq!(journal.notes(), "");
        let scribe = journal.scribe();
        scribe.append(&Message::User("hi".into())).unwrap();
        scribe.append(&said("hello")).unwrap();
        scribe.notes("first").unwrap();
        scribe.notes("second").unwrap();
        journal.cut(0).unwrap();
        drop(scribe);
        drop(journal);
        let (journal, history) = Journal::open(&path, None).unwrap();
        assert!(history.is_empty());
        assert_eq!(journal.notes(), "second");
        journal.save_notes("third").unwrap();
        drop(journal);
        let (journal, _) = Journal::open(&path, None).unwrap();
        assert_eq!(journal.notes(), "third");
    }

    /// A mark of a turn is saved, comes back with its turn, goes with it when the history is cut back, and a history can be opened cut back to before a turn by its id.
    #[test]
    fn marks_follow_their_turns() {
        let path = scratch("marks.jsonl");
        let mark = |id: &str| Mark {
            id: id.into(),
            text: format!("prompt {id}"),
            files: vec!["a.txt".into()],
        };
        let (journal, _) = Journal::open(&path, None).unwrap();
        let scribe = journal.scribe();
        for (at, id) in [(0, "1"), (2, "2"), (4, "3")] {
            journal.mark(at, &mark(id)).unwrap();
            scribe.append(&Message::User(format!("u{id}"))).unwrap();
            scribe
                .append(&Message::Assistant(Reply::default()))
                .unwrap();
        }
        assert_eq!(journal.marks().len(), 3);
        journal.cut(4).unwrap();
        assert_eq!(journal.marks().len(), 2);
        drop(scribe);
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!((saved.history.len(), saved.marks.len()), (4, 2));
    }

    /// Opening before a turn cuts that turn and what follows it from the history and from the file, and an id nobody has fails.
    #[test]
    fn opening_before_a_turn_cuts_it() {
        let path = scratch("before.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        let scribe = journal.scribe();
        for (at, id) in [(0, "a"), (2, "b")] {
            journal
                .mark(
                    at,
                    &Mark {
                        id: id.into(),
                        text: id.into(),
                        files: Vec::new(),
                    },
                )
                .unwrap();
            scribe.append(&Message::User(id.into())).unwrap();
            scribe
                .append(&Message::Assistant(Reply::default()))
                .unwrap();
        }
        drop(scribe);
        drop(journal);
        assert!(Journal::open_before(&path, None, Some("zzz")).is_err());
        let (journal, history) = Journal::open_before(&path, None, Some("b")).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(journal.marks().len(), 1);
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!((saved.history.len(), saved.marks.len()), (2, 1));
    }

    /// A name and the latest value of each setting are saved, come back when the history is opened or read, and a nameless history can be named without opening a conversation.
    #[test]
    fn title_and_settings_are_saved() {
        let path = scratch("facts.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        journal.save_setting("model", "a").unwrap();
        journal.save_setting("effort", "high").unwrap();
        journal.save_setting("model", "b").unwrap();
        journal.save_title("First").unwrap();
        journal.save_title("Second").unwrap();
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.title.as_deref(), Some("Second"));
        assert_eq!(
            saved.settings,
            vec![
                ("effort".to_owned(), "high".to_owned()),
                ("model".to_owned(), "b".to_owned())
            ]
        );
        Journal::rename(&path, "Third").unwrap();
        let (journal, _) = Journal::open(&path, None).unwrap();
        assert_eq!(journal.title().as_deref(), Some("Third"));
        assert_eq!(journal.settings().len(), 2);
    }

    /// A turn cut off for a reason is saved as lost with its prompt, stays out of the history, comes back when the file is read, and a history can be opened before it by its id.
    #[test]
    fn lost_turns_are_saved_and_can_be_rewound_to() {
        let path = scratch("lost.jsonl");
        let mark = |id: &str| Mark {
            id: id.into(),
            text: format!("prompt {id}"),
            files: Vec::new(),
        };
        let (journal, _) = Journal::open(&path, None).unwrap();
        let scribe = journal.scribe();
        journal.mark(0, &mark("1")).unwrap();
        scribe.append(&Message::User("u1".into())).unwrap();
        scribe.append(&said("a1")).unwrap();
        journal.mark(2, &mark("2")).unwrap();
        scribe.append(&Message::User("u2".into())).unwrap();
        journal.cut_losing(2, STOPPED).unwrap();
        let scribe = journal.scribe();
        journal.mark(2, &mark("3")).unwrap();
        scribe.append(&Message::User("u3".into())).unwrap();
        scribe.append(&said("a3")).unwrap();
        drop(scribe);
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.history.len(), 4);
        assert_eq!(saved.lost.len(), 1);
        assert_eq!(
            (
                saved.lost[0].mark.id.as_str(),
                saved.lost[0].at,
                saved.lost[0].reason.as_str()
            ),
            ("2", 2, STOPPED)
        );
        let (journal, history) = Journal::open_before(&path, None, Some("3")).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(journal.lost().len(), 1);
        drop(journal);
        let (journal, history) = Journal::open_before(&path, None, Some("2")).unwrap();
        assert_eq!(history.len(), 2);
        assert!(journal.lost().is_empty());
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert!(saved.lost.is_empty());
    }

    /// A turn a crash left unanswered is kept as interrupted when the history is opened again.
    #[test]
    fn an_unanswered_turn_is_kept_as_interrupted() {
        let path = scratch("interrupted.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        let scribe = journal.scribe();
        journal
            .mark(
                0,
                &Mark {
                    id: "9".into(),
                    text: "asked".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        scribe.append(&Message::User("asked".into())).unwrap();
        drop(scribe);
        drop(journal);
        let (journal, history) = Journal::open(&path, None).unwrap();
        assert!(history.is_empty());
        assert_eq!(journal.lost()[0].reason, INTERRUPTED);
        drop(journal);
        assert_eq!(Journal::read(&path).unwrap().lost.len(), 1);
    }

    /// The files a tool call changed and the facts the harness saved come back when the file is read, a fact keeps only its latest value, and saving the same value again writes nothing.
    #[test]
    fn changes_and_facts_are_saved() {
        let path = scratch("changes.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        journal
            .save_changes(
                "c1",
                &[FileChange {
                    path: "a.rs".into(),
                    old: "x\n".into(),
                    new: "y\n".into(),
                }],
            )
            .unwrap();
        journal.save_fact("window", &json!(1000)).unwrap();
        journal.save_fact("window", &json!(2000)).unwrap();
        let size = std::fs::metadata(&path).unwrap().len();
        journal.save_fact("window", &json!(2000)).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.changes["c1"][0].new, "y\n");
        assert_eq!(saved.window(), Some(2000));
        assert_eq!(saved.facts.len(), 1);
    }
}
