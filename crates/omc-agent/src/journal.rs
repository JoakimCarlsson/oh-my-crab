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
    /// The id of the branch the messages, marks and lost turns are of.
    pub branch: String,
    /// Every branch of the conversation, in the order they were made, the first one first; the one named by `branch` is among them.
    pub branches: Vec<Branch>,
}

/// The id of the branch a conversation starts on, which is also the only branch of a history saved before branches were recorded.
pub const FIRST_BRANCH: &str = "1";

/// One line of a conversation: the turns from its start to its tip. Going back to before a turn starts a new branch from that point, and the turns left behind stay on the branch they were on.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Branch {
    /// The branch's id, given by the journal: [`FIRST_BRANCH`] for the first, and counting up from there.
    pub id: String,
    /// The branch it went back from, or nothing for the first.
    pub parent: Option<String>,
    /// The id of the turn of `parent` it went back to before, or nothing for the first.
    pub before: Option<String>,
    /// The marks of its turns, with the position of the message each starts at.
    pub marks: Vec<Marked>,
    /// Its turns that were stopped or failed, oldest first.
    pub lost: Vec<Lost>,
}

/// A branch and the messages on it, as a history file is replayed.
#[derive(Clone, Debug, Default)]
struct Line {
    /// The branch, with its marks and lost turns.
    branch: Branch,
    /// The messages on it.
    history: Vec<Message>,
    /// Whether a turn was started or a message added on it since it was made, which a branch made by going back and left as it was has not.
    grown: bool,
}

impl Line {
    /// The first branch, empty.
    fn first() -> Self {
        Self {
            branch: Branch {
                id: FIRST_BRANCH.to_owned(),
                ..Branch::default()
            },
            ..Self::default()
        }
    }

    /// A new branch called `forked.id` that goes back from this one to before the turn `forked.before`, holding everything this one holds until it is cut back.
    fn branched(&self, forked: &Forked) -> Self {
        Self {
            branch: Branch {
                id: forked.id.clone(),
                parent: Some(self.branch.id.clone()),
                before: Some(forked.before.clone()),
                marks: self.branch.marks.clone(),
                lost: self.branch.lost.clone(),
            },
            history: self.history.clone(),
            grown: false,
        }
    }

    /// Whether going back on this branch should move its starting point instead of making another branch: it was made by going back and no turn was started on it since.
    fn bare(&self) -> bool {
        self.branch.parent.is_some() && !self.grown
    }

    /// Where the turn `id` starts on this branch: the length of the history before it, and the index of the lost turn when it is one that was lost; nothing when no turn of the branch has the id.
    fn find(&self, id: &str) -> Option<(usize, Option<usize>)> {
        if let Some((at, _)) = self.branch.marks.iter().find(|(_, mark)| mark.id == id) {
            return Some((*at, None));
        }
        self.branch
            .lost
            .iter()
            .position(|held| held.mark.id == id)
            .map(|index| (self.branch.lost[index].at, Some(index)))
    }
}

/// That a new branch starts: written before the cut-back that takes it back to before a turn.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct Forked {
    /// The id of the new branch, or of the branch the conversation is on when that branch only moves its starting point.
    id: String,
    /// The id of the turn the branch goes back to before.
    before: String,
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

/// What a reply the reader stopped ends with in the saved history, so that the model knows on the next turn that it was cut off there.
pub const STOPPED_NOTE: &str = "[The user stopped this reply here.]";

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
    /// A new branch starts from the branch the conversation is on, and the records after this are of it.
    Branch(&'a Forked),
    /// The records after this are of the branch with this id.
    Switch(&'a str),
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
    /// A new branch starts from the branch the conversation is on, and the records after this are of it.
    Branch(Forked),
    /// The records after this are of the branch with this id.
    Switch(String),
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
    /// The lock file beside it, held locked while the journal is open.
    _held: File,
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
    /// Every branch, in the order they were made; the marks and lost turns of the one the conversation is on are kept in `marks` and `lost` instead.
    branches: Vec<Branch>,
    /// The index in `branches` of the branch the conversation is on.
    current: usize,
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

/// How long opening a conversation keeps trying for its lock before saying it is open elsewhere.
const LOCK_PATIENCE: std::time::Duration = std::time::Duration::from_millis(500);

/// The file beside the conversation file `path` that is locked while the conversation is open: the conversation file itself is not locked, since Windows lets nobody read a locked file, and a conversation open elsewhere must stay readable.
fn lock_path(path: &Path) -> PathBuf {
    let mut named = path.as_os_str().to_owned();
    named.push(".lock");
    PathBuf::from(named)
}

/// Takes the lock on the lock file `file` of a conversation, trying again for up to [`LOCK_PATIENCE`].
///
/// A process another thread is starting holds a copy of every open file until it runs its program, and the lock with it, so a lock let go a moment ago can look held for that moment; a conversation open in another process stays held throughout.
///
/// # Errors
///
/// Returns an error saying the conversation is open elsewhere when the lock stays held, and the error of the file system when locking fails otherwise.
fn lock(file: &File) -> io::Result<()> {
    let deadline = std::time::Instant::now() + LOCK_PATIENCE;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(io::Error::other("the conversation is open elsewhere"));
            }
        }
    }
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

    /// Opens the history like [`Journal::open`], and when `before` names the id of a turn, goes back to before that turn as well, whichever of that and `keep` leaves less.
    ///
    /// Going back starts a new branch from that point, and the turns left behind stay in the file on the branch they were on; see [`Journal::open_branch`].
    ///
    /// # Errors
    ///
    /// Returns what [`Journal::open`] does, and [`JournalError::Io`] when no turn has the id `before`.
    pub fn open_before(
        path: &Path,
        keep: Option<usize>,
        before: Option<&str>,
    ) -> Result<(Arc<Self>, Vec<Message>), JournalError> {
        Self::open_branch(path, keep, None, before)
    }

    /// Opens the history like [`Journal::open`] on the branch `branch` when that is given, at its tip, and then, when `before` names the id of a turn of that branch, goes back to before it.
    ///
    /// Going back starts a new branch that holds the turns up to that point, and the conversation goes on there; the turns left behind stay on the branch they were on, which [`Journal::open_branch`] can open again. A branch that was made by going back and has had no turn since is moved back instead of being branched again, so that going back several times in a row leaves one branch. `keep` applies to the branch that is opened, after any going back.
    ///
    /// # Errors
    ///
    /// Returns what [`Journal::open`] does, and [`JournalError::Io`] when no branch has the id `branch` or no turn of it has the id `before`; the file is left as it was then.
    pub fn open_branch(
        path: &Path,
        keep: Option<usize>,
        branch: Option<&str>,
        before: Option<&str>,
    ) -> Result<(Arc<Self>, Vec<Message>), JournalError> {
        let io_error = |source| JournalError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(folder) = path.parent() {
            create_folder(folder).map_err(io_error)?;
        }
        let held = open_private(&lock_path(path)).map_err(io_error)?;
        lock(&held).map_err(io_error)?;
        let mut file = open_private(path).map_err(io_error)?;
        if !file.metadata().map_err(io_error)?.is_file() {
            return Err(io_error(io::Error::other("not a regular file")));
        }
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map_err(io_error)?;
        let complete = bytes
            .iter()
            .rposition(|&byte| byte == b'\n')
            .map_or(0, |end| end + 1);
        let Replayed {
            mut lines,
            mut current,
            title,
            settings,
            notes,
            changes,
            facts,
        } = replay(path, &bytes[..complete])?;
        let switched = match branch {
            Some(id) if id != lines[current].branch.id => {
                current = lines
                    .iter()
                    .position(|line| line.branch.id == id)
                    .ok_or_else(|| {
                        io_error(io::Error::other(format!("no branch has the id {id}")))
                    })?;
                Some(id)
            }
            _ => None,
        };
        let mut forked: Option<Forked> = None;
        let mut target: Option<(usize, Option<usize>)> = None;
        if let Some(id) = before {
            let Some(found) = lines[current].find(id) else {
                return Err(io_error(io::Error::other(format!(
                    "no turn has the id {id}"
                ))));
            };
            target = Some(found);
            if lines[current].bare() {
                lines[current].branch.before = Some(id.to_owned());
                forked = Some(Forked {
                    id: lines[current].branch.id.clone(),
                    before: id.to_owned(),
                });
            } else {
                let made = Forked {
                    id: (lines.len() + 1).to_string(),
                    before: id.to_owned(),
                };
                let line = lines[current].branched(&made);
                lines.push(line);
                current = lines.len() - 1;
                forked = Some(made);
            }
        }
        if complete < bytes.len() {
            file.set_len(complete as u64).map_err(io_error)?;
        }
        file.seek(SeekFrom::End(0)).map_err(io_error)?;
        let Line {
            branch: opened,
            mut history,
            ..
        } = std::mem::take(&mut lines[current]);
        let Branch {
            mut marks,
            mut lost,
            ..
        } = opened.clone();
        let saved = history.len();
        if let Some(turns) = keep {
            history.truncate(end_of_turns(&history, turns));
            lost.retain(|held| held.at <= history.len());
        }
        let mut forgotten: Option<&str> = None;
        if let Some((at, lost_index)) = target {
            history.truncate(history.len().min(at));
            match lost_index {
                Some(index) => {
                    lost.truncate(index);
                    forgotten = before;
                }
                None => lost.retain(|held| held.at <= history.len()),
            }
        }
        let intended = history.len();
        while matches!(history.last(), Some(Message::User(_) | Message::Images(_)))
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
        let mut wrote = false;
        if let Some(id) = switched {
            write_record(&mut file, &Written::Switch(id)).map_err(io_error)?;
            wrote = true;
        }
        if let Some(made) = &forked {
            write_record(&mut file, &Written::Branch(made)).map_err(io_error)?;
            wrote = true;
        }
        if let Some(id) = forgotten {
            write_record(&mut file, &Written::Forget(id)).map_err(io_error)?;
            wrote = true;
        }
        if history.len() < saved {
            write_record(&mut file, &Written::Cut(history.len())).map_err(io_error)?;
            for turn in &interrupted {
                write_record(&mut file, &Written::Lost(turn)).map_err(io_error)?;
            }
            wrote = true;
        }
        if wrote {
            file.sync_data().map_err(io_error)?;
        }
        lost.extend(interrupted);
        marks.retain(|(at, _)| *at < history.len());
        let mut branches: Vec<Branch> = lines.into_iter().map(|line| line.branch).collect();
        branches[current] = Branch {
            marks: Vec::new(),
            lost: Vec::new(),
            ..opened
        };
        let journal = Arc::new(Self {
            path: path.to_path_buf(),
            notes,
            state: Mutex::new(State {
                file,
                _held: held,
                epoch: 0,
                pending: None,
                marks,
                lost,
                title,
                settings,
                changes,
                facts,
                branches,
                current,
            }),
        });
        Ok((journal, history))
    }

    /// Copies the branch the conversation saved at `source` is on, up to the turn `before` when that is given or whole otherwise, to `destination` as a conversation of its own: its messages and turns, its lost turns, its name, its settings, its notebook, the files its tool calls changed and the facts saved with it. The copy has one branch; `source` is only read, so it may be open elsewhere.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Io`] when `source` cannot be read, no turn of its branch has the id `before`, `destination` cannot be opened or written or already holds a history, and [`JournalError::Corrupt`] when `source` is not a history.
    pub fn fork(
        source: &Path,
        destination: &Path,
        before: Option<&str>,
    ) -> Result<(), JournalError> {
        let saved = Self::read(source)?;
        let mut line = Line {
            branch: Branch {
                marks: saved.marks,
                lost: saved.lost,
                ..Branch::default()
            },
            history: saved.history,
            grown: true,
        };
        let failed = |error: io::Error| JournalError::Io {
            path: destination.to_path_buf(),
            source: error,
        };
        if let Some(id) = before {
            let Some((at, lost_index)) = line.find(id) else {
                return Err(JournalError::Io {
                    path: source.to_path_buf(),
                    source: io::Error::other(format!("no turn has the id {id}")),
                });
            };
            line.history.truncate(at);
            if let Some(index) = lost_index {
                line.branch.lost.truncate(index);
            }
        }
        let length = line.history.len();
        line.branch.lost.retain(|held| held.at <= length);
        line.branch.marks.retain(|(at, _)| *at < length);
        let (journal, existing) = Self::open(destination, None)?;
        if !existing.is_empty() || !journal.marks().is_empty() || !journal.lost().is_empty() {
            return Err(failed(io::Error::other(
                "the conversation already has a history",
            )));
        }
        let mut state = journal.lock();
        let file = &mut state.file;
        for (at, message) in line.history.iter().enumerate() {
            if let Some((_, mark)) = line.branch.marks.iter().find(|(start, _)| *start == at) {
                write_record(file, &Written::Turn(mark)).map_err(failed)?;
            }
            write_record(file, &Written::Message(message)).map_err(failed)?;
        }
        for turn in &line.branch.lost {
            write_record(file, &Written::Lost(turn)).map_err(failed)?;
        }
        if !saved.notes.is_empty() {
            write_record(file, &Written::Notes(&saved.notes)).map_err(failed)?;
        }
        if let Some(title) = &saved.title {
            write_record(file, &Written::Title(title)).map_err(failed)?;
        }
        for (id, value) in &saved.settings {
            write_record(file, &Written::Setting(id, value)).map_err(failed)?;
        }
        for (call, files) in &saved.changes {
            write_record(file, &Written::Changes(call, files)).map_err(failed)?;
        }
        for (key, value) in &saved.facts {
            write_record(file, &Written::Fact(key, value)).map_err(failed)?;
        }
        file.sync_data().map_err(failed)
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
        replay(path, &bytes[..complete]).map(Replayed::into_saved)
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

    /// The id of the branch the conversation is on, and every branch, in the order they were made, the one it is on with its turns as they are now.
    pub fn branches(&self) -> (String, Vec<Branch>) {
        let state = self.lock();
        let mut branches = state.branches.clone();
        let id = match branches.get_mut(state.current) {
            Some(branch) => {
                branch.marks.clone_from(&state.marks);
                branch.lost.clone_from(&state.lost);
                branch.id.clone()
            }
            None => FIRST_BRANCH.to_owned(),
        };
        (id, branches)
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

    /// Ends a turn that was stopped without losing what it did, so the model still knows it on the next turn: every step the turn finished stays, a reply whose tool calls have no results gives up its calls but keeps its text, and the reply that was being written when it stopped, `partial`, is kept as far as it got. The kept reply, or a reply of its own when the turn's prompt has none, ends with [`STOPPED_NOTE`]. Turns cut off whole are recorded as lost for `reason`. Returns the messages the history holds afterwards.
    ///
    /// The history is read back from the file, which holds everything the turn wrote, since the turn's own copy is gone with its task.
    ///
    /// # Errors
    ///
    /// Returns the error of the write or the sync, or of reading the file back.
    pub fn settle_stopped(&self, reason: &str, partial: &str) -> io::Result<Vec<Message>> {
        self.lock().settle()?;
        let mut history = Self::read(&self.path).map_err(io::Error::other)?.history;
        let mut said = Vec::new();
        while let Some(Message::Assistant(reply)) = history.last()
            && !reply.tool_calls.is_empty()
        {
            said.push(reply.text.trim().to_owned());
            history.pop();
        }
        said.reverse();
        said.push(partial.trim().to_owned());
        said.retain(|text| !text.is_empty());
        self.cut_losing(history.len(), reason)?;
        if said.is_empty() && !matches!(history.last(), Some(Message::User(_) | Message::Images(_)))
        {
            return Ok(history);
        }
        said.push(STOPPED_NOTE.to_owned());
        let stopped = Message::Assistant(omc_providers::Reply {
            text: said.join("\n\n"),
            ..omc_providers::Reply::default()
        });
        let mut state = self.lock();
        write_record(&mut state.file, &Written::Message(&stopped))?;
        state.file.sync_data()?;
        history.push(stopped);
        Ok(history)
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

/// What replaying a history file gives: every branch with its messages, the one the conversation is on, and what the conversation holds besides.
struct Replayed {
    /// Every branch, in the order they were made.
    lines: Vec<Line>,
    /// The index in `lines` of the branch the conversation is on.
    current: usize,
    /// The conversation's name.
    title: Option<String>,
    /// The latest value of each chosen setting, oldest choice first.
    settings: Vec<(String, String)>,
    /// The notebook.
    notes: String,
    /// The files each tool call changed, by the id of the call.
    changes: std::collections::HashMap<String, Vec<FileChange>>,
    /// The latest value saved under each key, oldest first.
    facts: Vec<(String, serde_json::Value)>,
}

impl Replayed {
    /// The saved conversation this is, on the branch it is on.
    fn into_saved(self) -> Saved {
        let Self {
            lines,
            current,
            title,
            settings,
            notes,
            changes,
            facts,
        } = self;
        let mut branches = Vec::with_capacity(lines.len());
        let mut history = Vec::new();
        for (index, line) in lines.into_iter().enumerate() {
            if index == current {
                history = line.history;
            }
            branches.push(line.branch);
        }
        let on = branches[current].clone();
        Saved {
            history,
            marks: on.marks,
            lost: on.lost,
            title,
            settings,
            notes,
            changes,
            facts,
            branch: on.id,
            branches,
        }
    }
}

/// Builds the saved conversation the complete lines in `bytes` add up to, with every branch it holds.
///
/// A history written before branches were recorded has none of their records, and replays as the one branch [`FIRST_BRANCH`].
///
/// # Errors
///
/// Returns [`JournalError::Corrupt`] for a line that is not a record, a cut-back past the start or past the end, or a switch to a branch that was never made.
fn replay(path: &Path, bytes: &[u8]) -> Result<Replayed, JournalError> {
    let mut lines = vec![Line::first()];
    let mut current = 0;
    let mut notes = String::new();
    let mut title = None;
    let mut settings: Vec<(String, String)> = Vec::new();
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
        let record = serde_json::from_slice(line).map_err(|error| corrupt(error.to_string()))?;
        let on = &mut lines[current];
        match record {
            Record::Turn(mark) => {
                on.grown = true;
                waiting = Some(mark);
            }
            Record::Message(message) => {
                if matches!(message, Message::User(_))
                    && let Some(mark) = waiting.take()
                {
                    on.branch.marks.push((on.history.len(), mark));
                }
                on.grown = true;
                on.history.push(message);
            }
            Record::Notes(text) => notes = text,
            Record::Title(text) => title = Some(text),
            Record::Setting(id, value) => {
                settings.retain(|(held, _)| *held != id);
                settings.push((id, value));
            }
            Record::Lost(turn) => on.branch.lost.push(turn),
            Record::Changes(call, files) => {
                changes.insert(call, files);
            }
            Record::Fact(key, value) => {
                facts.retain(|(held, _)| *held != key);
                facts.push((key, value));
            }
            Record::Forget(id) => {
                if let Some(index) = on.branch.lost.iter().position(|held| held.mark.id == id) {
                    on.branch.lost.truncate(index);
                }
            }
            Record::Cut(length) if length <= on.history.len() => {
                on.history.truncate(length);
                on.branch.marks.retain(|(at, _)| *at < length);
                on.branch.lost.retain(|held| held.at <= length);
                waiting = None;
            }
            Record::Cut(length) => {
                return Err(corrupt(format!(
                    "cut back to {length} of {} messages",
                    on.history.len()
                )));
            }
            Record::Branch(forked) if forked.id == on.branch.id => {
                on.branch.before = Some(forked.before);
                waiting = None;
            }
            Record::Branch(forked) => {
                let made = on.branched(&forked);
                if lines.iter().any(|line| line.branch.id == forked.id) {
                    return Err(corrupt(format!("branch {} is made twice", forked.id)));
                }
                lines.push(made);
                current = lines.len() - 1;
                waiting = None;
            }
            Record::Switch(id) => {
                current = lines
                    .iter()
                    .position(|line| line.branch.id == id)
                    .ok_or_else(|| corrupt(format!("no branch has the id {id}")))?;
                waiting = None;
            }
        }
    }
    Ok(Replayed {
        lines,
        current,
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

    /// A turn stopped after finishing a step keeps that step and loses only the call nothing answered, so the next open still has the work.
    #[test]
    fn a_stopped_turn_keeps_the_steps_it_finished() {
        let path = scratch("stopped.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        let call = |id: &str| {
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: id.into(),
                    name: "read".into(),
                    arguments: json!({}),
                }],
                ..Reply::default()
            })
        };
        journal
            .mark(
                0,
                &Mark {
                    id: "1".into(),
                    text: "go".into(),
                    files: Vec::new(),
                },
            )
            .unwrap();
        append_all(
            &journal,
            &[
                Message::User("go".into()),
                call("c1"),
                Message::ToolResults(vec![ToolResult {
                    call_id: "c1".into(),
                    name: "read".into(),
                    text: "done".into(),
                    is_error: false,
                    images: Vec::new(),
                }]),
                call("c2"),
            ],
        );
        let kept = journal.settle_stopped(STOPPED, "").unwrap();
        assert_eq!(kept.len(), 3);
        drop(journal);
        let (_, reopened) = Journal::open(&path, None).unwrap();
        assert_eq!(reopened.len(), 3);
        assert!(Journal::read(&path).unwrap().lost.is_empty());
    }

    /// A reply stopped while it was being written keeps its prompt and the text written so far, marked as stopped, so the model knows on the next turn what it was saying.
    #[test]
    fn a_reply_stopped_midway_is_kept_as_far_as_it_got() {
        let path = scratch("stopped-midway.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        append_all(&journal, &[Message::User("write a poem".into())]);
        let kept = journal
            .settle_stopped(STOPPED, "The cursor blinks, a patient light")
            .unwrap();
        drop(journal);
        let (_, reopened) = Journal::open(&path, None).unwrap();
        assert_eq!(kept, reopened);
        assert_eq!(reopened.len(), 2);
        let Message::Assistant(reply) = &reopened[1] else {
            panic!("the stopped reply is not kept");
        };
        assert_eq!(
            reply.text,
            format!("The cursor blinks, a patient light\n\n{STOPPED_NOTE}")
        );
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
                images: Vec::new(),
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

    /// A history open in one conversation cannot be opened in another until the first lets go of it. The history file stays readable meanwhile, since the lock is on a file beside it.
    #[test]
    fn a_history_opens_once_at_a_time() {
        let path = scratch("locked.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        assert!(matches!(
            Journal::open(&path, None),
            Err(JournalError::Io { .. })
        ));
        assert!(Journal::read(&path).is_ok());
        assert!(
            lock_path(&path)
                .to_string_lossy()
                .ends_with("locked.jsonl.lock")
        );
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

    /// Saves turns with the ids `ids`, each a prompt and an answer, on the branch `journal` is on, which holds `at` messages so far.
    fn turns(journal: &Arc<Journal>, at: usize, ids: &[&str]) {
        let scribe = journal.scribe();
        for (index, id) in ids.iter().enumerate() {
            journal
                .mark(
                    at + 2 * index,
                    &Mark {
                        id: (*id).into(),
                        text: format!("prompt {id}"),
                        files: Vec::new(),
                    },
                )
                .unwrap();
            scribe.append(&Message::User(format!("u{id}"))).unwrap();
            scribe.append(&said(&format!("a{id}"))).unwrap();
        }
    }

    /// The ids of the turns `branch` holds, in order.
    fn ids(branch: &Branch) -> Vec<&str> {
        branch
            .marks
            .iter()
            .map(|(_, mark)| mark.id.as_str())
            .collect()
    }

    /// Going back to before a turn starts a new branch there; the turns left behind stay in the file on the first branch, which can be opened again at its tip, and each branch keeps the turns added to it.
    #[test]
    fn going_back_branches_and_keeps_the_turns_left_behind() {
        let path = scratch("branches.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        turns(&journal, 0, &["a", "b", "c"]);
        assert_eq!(journal.branches().0, FIRST_BRANCH);
        drop(journal);
        let (journal, history) = Journal::open_before(&path, None, Some("b")).unwrap();
        assert_eq!(history.len(), 2);
        let (on, branches) = journal.branches();
        assert_eq!(on, "2");
        assert_eq!(branches.len(), 2);
        assert_eq!(ids(&branches[0]), ["a", "b", "c"]);
        assert_eq!(
            (branches[1].parent.as_deref(), branches[1].before.as_deref()),
            (Some(FIRST_BRANCH), Some("b"))
        );
        turns(&journal, 2, &["d"]);
        assert_eq!(ids(&journal.branches().1[1]), ["a", "d"]);
        drop(journal);
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.branch, "2");
        assert_eq!(saved.history.len(), 4);
        let (journal, history) =
            Journal::open_branch(&path, None, Some(FIRST_BRANCH), None).unwrap();
        assert_eq!(history, answered(&["a", "b", "c"]));
        assert_eq!(journal.marks().len(), 3);
        assert_eq!(ids(&journal.branches().1[1]), ["a", "d"]);
        turns(&journal, 6, &["e"]);
        drop(journal);
        let (journal, history) = Journal::open_branch(&path, None, Some("2"), None).unwrap();
        assert_eq!(history, answered(&["a", "d"]));
        assert_eq!(ids(&journal.branches().1[0]), ["a", "b", "c", "e"]);
        drop(journal);
        assert_eq!(Journal::read(&path).unwrap().branch, "2");
    }

    /// The prompts and answers `turns` saves for the ids `ids`.
    fn answered(ids: &[&str]) -> Vec<Message> {
        ids.iter()
            .flat_map(|id| [Message::User(format!("u{id}")), said(&format!("a{id}"))])
            .collect()
    }

    /// Going back again before any turn was added moves the branch made by the first going back, instead of making another one, while a branch that had messages added, even without a turn id, is branched again; a branch or turn nobody has fails without touching the file.
    #[test]
    fn going_back_twice_in_a_row_makes_one_branch() {
        let path = scratch("twice.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        turns(&journal, 0, &["a", "b", "c"]);
        drop(journal);
        drop(Journal::open_before(&path, None, Some("c")).unwrap());
        let (journal, history) = Journal::open_before(&path, None, Some("b")).unwrap();
        assert_eq!(history.len(), 2);
        let (on, branches) = journal.branches();
        assert_eq!((on.as_str(), branches.len()), ("2", 2));
        assert_eq!(branches[1].before.as_deref(), Some("b"));
        drop(journal);
        let (journal, _) = Journal::open_branch(&path, None, None, None).unwrap();
        append_all(&journal, &[Message::User("unmarked".into()), said("ok")]);
        drop(journal);
        let (journal, _) = Journal::open_before(&path, None, Some("a")).unwrap();
        assert_eq!(journal.branches().0, "3");
        drop(journal);
        let (journal, history) = Journal::open_branch(&path, None, Some("2"), None).unwrap();
        assert_eq!(history.len(), 4);
        drop(journal);
        let size = std::fs::metadata(&path).unwrap().len();
        assert!(Journal::open_branch(&path, None, Some("9"), None).is_err());
        assert!(Journal::open_branch(&path, None, Some(FIRST_BRANCH), Some("zzz")).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), size);
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.branches.len(), 3);
        assert_eq!(ids(&saved.branches[0]), ["a", "b", "c"]);
        assert_eq!(saved.branches[1].before.as_deref(), Some("b"));
        assert_eq!(saved.branches[2].parent.as_deref(), Some("2"));
    }

    /// A history saved before branches were recorded loads as the one first branch.
    #[test]
    fn a_history_without_branches_is_one_branch() {
        let path = scratch("linear.jsonl");
        std::fs::write(
            &path,
            "{\"turn\":{\"id\":\"t\",\"text\":\"q\"}}\n{\"message\":{\"User\":\"q\"}}\n{\"message\":{\"Assistant\":{\"text\":\"a\"}}}\n",
        )
        .unwrap();
        let saved = Journal::read(&path).unwrap();
        assert_eq!(saved.branch, FIRST_BRANCH);
        assert_eq!(saved.branches.len(), 1);
        assert_eq!(ids(&saved.branches[0]), ["t"]);
        assert_eq!(saved.branches[0].parent, None);
        let (journal, history) = Journal::open(&path, None).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(journal.branches().0, FIRST_BRANCH);
    }

    /// Forking copies the branch the conversation is on up to a turn into a conversation of its own, with its name and settings, leaving the original as it was; a destination with a history is refused.
    #[test]
    fn forking_copies_the_branch_up_to_a_turn() {
        let path = scratch("fork-source.jsonl");
        let copy = scratch("fork-copy.jsonl");
        let (journal, _) = Journal::open(&path, None).unwrap();
        turns(&journal, 0, &["a", "b", "c"]);
        journal.save_title("Source").unwrap();
        journal.save_setting("mode", "yolo").unwrap();
        journal.save_notes("notebook").unwrap();
        Journal::fork(&path, &copy, Some("c")).unwrap();
        assert_eq!(journal.marks().len(), 3);
        drop(journal);
        let saved = Journal::read(&copy).unwrap();
        assert_eq!(saved.history, answered(&["a", "b"]));
        assert_eq!(saved.marks.len(), 2);
        assert_eq!(saved.title.as_deref(), Some("Source"));
        assert_eq!(saved.settings, [("mode".to_owned(), "yolo".to_owned())]);
        assert_eq!(saved.notes, "notebook");
        assert_eq!(saved.branches.len(), 1);
        assert!(Journal::fork(&path, &copy, None).is_err());
        assert!(Journal::fork(&path, &scratch("fork-none.jsonl"), Some("zzz")).is_err());
        assert_eq!(Journal::read(&path).unwrap().history.len(), 6);
    }
}
