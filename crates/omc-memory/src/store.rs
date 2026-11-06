//! The files memories are kept in, and the changes made to them.
//!
//! Each scope keeps its memories in a folder of its own, as `memories.jsonl`: one memory to a line, as JSON, which a person can read and edit. Every change takes an exclusive lock on `memories.lock` beside it, reads the file again, and replaces it whole by writing a temporary file and renaming it over the old one, so that conversations writing at once never lose each other's changes and a reader never sees half a file. Lines that are not memories, which a hand edit can leave, are kept as they are.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::Project;
use crate::project::fnv;

/// The environment variable that moves the folder memories are kept in.
pub const ROOT_VARIABLE: &str = "OMC_MEMORY_DIR";

/// The environment variable that turns memory off for every conversation when it is `0`, `off`, `false` or `no`.
pub const SWITCH_VARIABLE: &str = "OMC_MEMORY";

/// The most bytes the text of one memory may hold.
pub const MEMORY_LIMIT: usize = 2000;

/// The most tags one memory may carry.
pub const TAG_LIMIT: usize = 8;

/// The most characters one tag may hold.
const TAG_LENGTH: usize = 40;

/// The most memories one scope may hold.
const SHELF_LIMIT: usize = 1000;

/// The file a scope's memories are kept in.
const FILE: &str = "memories.jsonl";

/// The file whose lock every change to a scope's memories holds.
const LOCK: &str = "memories.lock";

/// The file that says which project a project's folder is for.
const ABOUT: &str = "project.json";

/// Tells temporary files written at once by one process apart.
static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Whether [`SWITCH_VARIABLE`] turns memory off.
pub fn disabled_by_environment() -> bool {
    std::env::var(SWITCH_VARIABLE).is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "off" | "false" | "no"
        )
    })
}

/// What can go wrong with memories.
#[derive(Debug, thiserror::Error)]
pub enum MemoryError {
    /// A file could not be read or written.
    #[error("cannot use the memory file {path}: {source}")]
    Io {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        source: io::Error,
    },
    /// The change asked for cannot be made, for the reason given, which is written for the model.
    #[error("{0}")]
    Invalid(String),
}

/// Who a memory is for.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// The project, shared by every conversation in it.
    #[default]
    Project,
    /// The user, shared by every conversation in every project.
    User,
}

impl Scope {
    /// The name of the scope, as tools take it.
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Project => "project",
            Scope::User => "user",
        }
    }

    /// The scope `name` stands for.
    pub fn parse(name: &str) -> Option<Self> {
        [Scope::Project, Scope::User]
            .into_iter()
            .find(|scope| scope.as_str() == name)
    }
}

/// One fact or lesson the agent saved.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    /// What tools name it by: eight hex digits, unique within its scope.
    pub id: String,
    /// What it says.
    pub text: String,
    /// Words it is filed under, lowercase.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// When it was saved, as `YYYY-MM-DDTHH:MM:SSZ`.
    pub created: String,
    /// The conversation that saved it, when that conversation had an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation: Option<String>,
    /// When it was last changed, when it was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<String>,
    /// The conversation that last changed it, when it was changed by one with an id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_in: Option<String>,
    /// Who it is for, which is where it is kept rather than part of what is saved.
    #[serde(skip)]
    pub scope: Scope,
}

impl Memory {
    /// When it was last saved or changed, which sorts in time order as text.
    pub fn changed(&self) -> &str {
        self.updated.as_deref().unwrap_or(&self.created)
    }
}

/// What saving a memory came to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Added {
    /// The memory as it is kept.
    pub memory: Memory,
    /// Whether the same text was already kept, in which case the memory is the one that was, with the new tags added.
    pub duplicate: bool,
}

/// The memories of one scope, in a folder of their own.
#[derive(Clone, Debug)]
pub struct Shelf {
    /// The folder.
    folder: PathBuf,
    /// Who the memories are for.
    scope: Scope,
    /// What is written to [`ABOUT`] the first time the folder is written to, when anything is.
    about: Option<serde_json::Value>,
}

impl Shelf {
    /// The memories of `scope` in `folder`.
    pub fn new(folder: impl Into<PathBuf>, scope: Scope) -> Self {
        Self {
            folder: folder.into(),
            scope,
            about: None,
        }
    }

    /// The folder the memories are kept in.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Who the memories are for.
    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The memories, oldest first, read without waiting for a writer, which never leaves half a file.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] when the file is there but cannot be read.
    pub fn list(&self) -> Result<Vec<Memory>, MemoryError> {
        self.read().map(|(memories, _)| memories)
    }

    /// Saves `text` with `tags` as a new memory, recording `conversation` as the one that saved it at `now`; text that is already kept, whatever its case and spacing, gets the new tags instead.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Invalid`] when the text is empty or too long, a tag is too long or there are too many, or the scope is full, and [`MemoryError::Io`] when the files cannot be used.
    pub fn add(
        &self,
        text: &str,
        tags: &[String],
        conversation: Option<&str>,
        now: &str,
    ) -> Result<Added, MemoryError> {
        let text = clean_text(text)?;
        let tags = clean_tags(tags)?;
        self.change(|memories| {
            let same = normalized(&text);
            if let Some(kept) = memories
                .iter_mut()
                .find(|memory| normalized(&memory.text) == same)
            {
                let before = kept.tags.len();
                for tag in tags {
                    if !kept.tags.contains(&tag) {
                        kept.tags.push(tag);
                    }
                }
                if kept.tags.len() > TAG_LIMIT {
                    return Err(too_many_tags());
                }
                if kept.tags.len() != before {
                    stamp(kept, conversation, now);
                }
                return Ok(Added {
                    memory: kept.clone(),
                    duplicate: true,
                });
            }
            if memories.len() >= SHELF_LIMIT {
                return Err(MemoryError::Invalid(format!(
                    "the {} memory already holds {SHELF_LIMIT} memories, the most it may; forget stale ones or merge related ones with `memory_edit` first",
                    self.scope.as_str()
                )));
            }
            let memory = Memory {
                id: fresh_id(memories, &text),
                text,
                tags,
                created: now.to_owned(),
                conversation: conversation.map(str::to_owned),
                updated: None,
                updated_in: None,
                scope: self.scope,
            };
            memories.push(memory.clone());
            Ok(Added {
                memory,
                duplicate: false,
            })
        })
    }

    /// Gives the memory `id` the text `text` and the tags `tags`, each when it is given, recording `conversation` as the one that changed it at `now`; nothing when no memory here has that id.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Invalid`] when the text or tags are not acceptable, as [`Shelf::add`] says, and [`MemoryError::Io`] when the files cannot be used.
    pub fn update(
        &self,
        id: &str,
        text: Option<&str>,
        tags: Option<&[String]>,
        conversation: Option<&str>,
        now: &str,
    ) -> Result<Option<Memory>, MemoryError> {
        let text = text.map(clean_text).transpose()?;
        let tags = tags.map(clean_tags).transpose()?;
        self.change(|memories| {
            let Some(memory) = memories.iter_mut().find(|memory| memory.id == id) else {
                return Ok(None);
            };
            if let Some(text) = text {
                memory.text = text;
            }
            if let Some(tags) = tags {
                memory.tags = tags;
            }
            stamp(memory, conversation, now);
            Ok(Some(memory.clone()))
        })
    }

    /// Removes the memory `id` and returns it; nothing when no memory here has that id.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] when the files cannot be used.
    pub fn forget(&self, id: &str) -> Result<Option<Memory>, MemoryError> {
        self.change(|memories| {
            let at = memories.iter().position(|memory| memory.id == id);
            Ok(at.map(|at| memories.remove(at)))
        })
    }

    /// The path of the file named `name` in the folder.
    fn path(&self, name: &str) -> PathBuf {
        self.folder.join(name)
    }

    /// The memories in the file, and the lines of it that are not memories; both empty when there is no file yet.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] when the file is there but cannot be read.
    fn read(&self) -> Result<(Vec<Memory>, Vec<String>), MemoryError> {
        let path = self.path(FILE);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            Err(source) => return Err(MemoryError::Io { path, source }),
        };
        let mut memories = Vec::new();
        let mut unread = Vec::new();
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            match serde_json::from_str::<Memory>(line) {
                Ok(mut memory) => {
                    memory.scope = self.scope;
                    memories.push(memory);
                }
                Err(_) => unread.push(line.to_owned()),
            }
        }
        Ok((memories, unread))
    }

    /// Runs `edit` on the memories while holding the lock, and writes them back when it changed them.
    ///
    /// # Errors
    ///
    /// Returns what `edit` fails with, in which case nothing is written, and [`MemoryError::Io`] when the folder, the lock or the file cannot be used.
    fn change<T>(
        &self,
        edit: impl FnOnce(&mut Vec<Memory>) -> Result<T, MemoryError>,
    ) -> Result<T, MemoryError> {
        let failed = |path: PathBuf| move |source| MemoryError::Io { path, source };
        create_folder(&self.folder).map_err(failed(self.folder.clone()))?;
        let lock_path = self.path(LOCK);
        let lock = open_private(&lock_path).map_err(failed(lock_path.clone()))?;
        lock.lock().map_err(failed(lock_path))?;
        let (mut memories, unread) = self.read()?;
        let before = memories.clone();
        let outcome = edit(&mut memories)?;
        if memories != before {
            if let Some(about) = &self.about {
                let path = self.path(ABOUT);
                if !path.exists() {
                    let text = serde_json::to_string_pretty(about).unwrap_or_default();
                    write_atomic(&path, format!("{text}\n").as_bytes())
                        .map_err(failed(path.clone()))?;
                }
            }
            let mut text = String::new();
            for memory in &memories {
                text.push_str(&serde_json::to_string(memory).unwrap_or_default());
                text.push('\n');
            }
            for line in &unread {
                text.push_str(line);
                text.push('\n');
            }
            let path = self.path(FILE);
            write_atomic(&path, text.as_bytes()).map_err(failed(path.clone()))?;
        }
        drop(lock);
        Ok(outcome)
    }
}

/// Where every scope's memories are kept.
#[derive(Clone, Debug)]
pub struct Store {
    /// The folder that holds them.
    root: PathBuf,
}

impl Store {
    /// The memories kept under `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The memories kept where [`ROOT_VARIABLE`] says, or else in `oh-my-crab/memory` in the user's data folder; nothing when there is no data folder.
    pub fn locate() -> Option<Self> {
        match std::env::var_os(ROOT_VARIABLE).filter(|root| !root.is_empty()) {
            Some(root) => Some(Self::new(root)),
            None => dirs::data_dir().map(|data| Self::new(data.join("oh-my-crab").join("memory"))),
        }
    }

    /// The folder that holds every scope's memories.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The memories of `project`, in `projects/<name>-<hash>`, whose folder says which project it is for once something is written to it.
    pub fn project(&self, project: &Project) -> Shelf {
        Shelf {
            folder: self.root.join("projects").join(project.folder_name()),
            scope: Scope::Project,
            about: Some(serde_json::json!({
                "name": project.name(),
                "root": project.root.display().to_string(),
                "key": project.key.display().to_string(),
            })),
        }
    }

    /// The memories kept for the user across projects, in `user`.
    pub fn user(&self) -> Shelf {
        Shelf::new(self.root.join("user"), Scope::User)
    }
}

/// The memories one conversation reads and writes: its project's and the user's.
#[derive(Clone, Debug)]
pub struct Memories {
    /// The project the conversation works in.
    project: Project,
    /// The project's memories.
    project_shelf: Shelf,
    /// The user's memories.
    user_shelf: Shelf,
    /// The conversation's id, which is recorded with what it saves, when it has one.
    conversation: Option<String>,
}

impl Memories {
    /// The memories of `project` and of the user in `store`, for the conversation `conversation`.
    pub fn new(store: &Store, project: Project, conversation: Option<String>) -> Self {
        Self {
            project_shelf: store.project(&project),
            user_shelf: store.user(),
            project,
            conversation,
        }
    }

    /// The memories of the project `directory` belongs to and of the user, in the store [`Store::locate`] finds, for the conversation `conversation`; nothing when there is no store.
    pub fn open(directory: &Path, conversation: Option<String>) -> Option<Self> {
        Some(Self::new(
            &Store::locate()?,
            Project::of(directory),
            conversation,
        ))
    }

    /// The project the conversation works in.
    pub fn project(&self) -> &Project {
        &self.project
    }

    /// The memories of `scope`.
    pub fn shelf(&self, scope: Scope) -> &Shelf {
        match scope {
            Scope::Project => &self.project_shelf,
            Scope::User => &self.user_shelf,
        }
    }

    /// Every memory, the project's first, each scope oldest first.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Io`] when a file is there but cannot be read.
    pub fn all(&self) -> Result<Vec<Memory>, MemoryError> {
        let mut all = self.project_shelf.list()?;
        all.extend(self.user_shelf.list()?);
        Ok(all)
    }

    /// Saves `text` with `tags` for `scope`, as [`Shelf::add`] does, as said now by this conversation.
    ///
    /// # Errors
    ///
    /// Returns what [`Shelf::add`] does.
    pub fn remember(
        &self,
        text: &str,
        tags: &[String],
        scope: Scope,
    ) -> Result<Added, MemoryError> {
        self.shelf(scope)
            .add(text, tags, self.conversation.as_deref(), &now())
    }

    /// Changes the text, the tags or both of the memory `id` of either scope, the project's looked in first.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Invalid`] when neither is given or no memory has that id, and what [`Shelf::update`] fails with.
    pub fn update(
        &self,
        id: &str,
        text: Option<&str>,
        tags: Option<&[String]>,
    ) -> Result<Memory, MemoryError> {
        if text.is_none() && tags.is_none() {
            return Err(MemoryError::Invalid(
                "give the new `text`, the new `tags`, or `forget: true`".to_owned(),
            ));
        }
        let now = now();
        for shelf in [&self.project_shelf, &self.user_shelf] {
            if let Some(memory) =
                shelf.update(id, text, tags, self.conversation.as_deref(), &now)?
            {
                return Ok(memory);
            }
        }
        Err(unknown(id))
    }

    /// Removes the memory `id` of either scope, the project's looked in first, and returns it.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Invalid`] when no memory has that id, and [`MemoryError::Io`] when the files cannot be used.
    pub fn forget(&self, id: &str) -> Result<Memory, MemoryError> {
        for shelf in [&self.project_shelf, &self.user_shelf] {
            if let Some(memory) = shelf.forget(id)? {
                return Ok(memory);
            }
        }
        Err(unknown(id))
    }
}

/// What the model is told when no memory has the id `id`.
fn unknown(id: &str) -> MemoryError {
    MemoryError::Invalid(format!(
        "no memory has the id `{id}`; find it with `memory_search`"
    ))
}

/// What the model is told when a memory would carry more than [`TAG_LIMIT`] tags.
fn too_many_tags() -> MemoryError {
    MemoryError::Invalid(format!("a memory carries at most {TAG_LIMIT} tags"))
}

/// Records that `memory` was changed by `conversation` at `now`.
fn stamp(memory: &mut Memory, conversation: Option<&str>, now: &str) {
    memory.updated = Some(now.to_owned());
    memory.updated_in = conversation.map(str::to_owned);
}

/// The time now, as memories record it.
fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    omc_context::timestamp(seconds)
}

/// `text` without the space around it.
///
/// # Errors
///
/// Returns [`MemoryError::Invalid`] when nothing is left or it is longer than [`MEMORY_LIMIT`] bytes.
fn clean_text(text: &str) -> Result<String, MemoryError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(MemoryError::Invalid("a memory needs some text".to_owned()));
    }
    if text.len() > MEMORY_LIMIT {
        return Err(MemoryError::Invalid(format!(
            "a memory holds at most {MEMORY_LIMIT} bytes and this one is {} bytes; keep to one fact and say it shorter",
            text.len()
        )));
    }
    Ok(text.to_owned())
}

/// `tags` lowercased, with their spaces turned into `-`, without empty ones or repeats.
///
/// # Errors
///
/// Returns [`MemoryError::Invalid`] when a tag is longer than its limit or more than [`TAG_LIMIT`] are left.
fn clean_tags(tags: &[String]) -> Result<Vec<String>, MemoryError> {
    let mut clean: Vec<String> = Vec::new();
    for tag in tags {
        let tag = tag
            .split_whitespace()
            .collect::<Vec<_>>()
            .join("-")
            .to_lowercase();
        if tag.chars().count() > TAG_LENGTH {
            return Err(MemoryError::Invalid(format!(
                "a tag holds at most {TAG_LENGTH} characters"
            )));
        }
        if !tag.is_empty() && !clean.contains(&tag) {
            clean.push(tag);
        }
    }
    if clean.len() > TAG_LIMIT {
        return Err(too_many_tags());
    }
    Ok(clean)
}

/// `text` lowercased with its runs of space made one, which two memories saying the same thing share.
fn normalized(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// An id no memory in `memories` has, made from `text`, the time and the process.
fn fresh_id(memories: &[Memory], text: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    (0u32..)
        .map(|attempt| {
            let seed = format!("{text}\n{nanos}\n{}\n{attempt}", std::process::id());
            format!("{:08x}", fnv(seed.as_bytes()) as u32)
        })
        .find(|id| !memories.iter().any(|memory| &memory.id == id))
        .unwrap_or_default()
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

/// Opens `path` for writing, creating it readable by the owner only on systems that have file modes.
///
/// # Errors
///
/// Returns the error of the open.
fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

/// Replaces `path` with `content` by writing a temporary file beside it and renaming that over it, so that a reader sees the old file or the new one and never part of either; the temporary file is removed when a step fails.
///
/// # Errors
///
/// Returns the error of the step that failed.
fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(
        ".{name}.{}-{}.tmp",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let written = (|| {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&temp)?;
        file.write_all(content)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

#[cfg(test)]
/// Tests for keeping memories.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// The store of a scratch folder named `name`, and the memories of a made-up project and conversation in it.
    fn memories(name: &str) -> (Store, Memories) {
        let root = scratch(name);
        let store = Store::new(root.join("memory"));
        let project = Project {
            root: root.join("app"),
            key: root.join("app/.git"),
        };
        let memories = Memories::new(&store, project, Some("conv-1".to_owned()));
        (store, memories)
    }

    /// A saved memory reads back the same, with its scope, tags, time and conversation, from a file of JSON lines; the project folder says which project it is for.
    #[test]
    fn memories_round_trip() {
        let (store, memories) = memories("store-round-trip");
        let added = memories
            .remember(
                "  Run tests with `make check`, not cargo test.  ",
                &["Build Tools".to_owned(), "tests".to_owned()],
                Scope::Project,
            )
            .unwrap();
        assert!(!added.duplicate);
        let memory = added.memory;
        assert_eq!(memory.text, "Run tests with `make check`, not cargo test.");
        assert_eq!(memory.tags, ["build-tools", "tests"]);
        assert_eq!(memory.conversation.as_deref(), Some("conv-1"));
        assert_eq!(memory.id.len(), 8);
        assert!(memory.created.ends_with('Z'));
        let user = memories
            .remember("Prefers British spelling.", &[], Scope::User)
            .unwrap()
            .memory;
        let again = Memories::new(&store, memories.project().clone(), None);
        let all = again.all().unwrap();
        assert_eq!(all, vec![memory.clone(), user.clone()]);
        assert_eq!((all[0].scope, all[1].scope), (Scope::Project, Scope::User));
        let shelf = memories.shelf(Scope::Project);
        let text = std::fs::read_to_string(shelf.folder().join(FILE)).unwrap();
        assert_eq!(text.lines().count(), 1);
        assert!(text.contains("\"conversation\":\"conv-1\""));
        let about = std::fs::read_to_string(shelf.folder().join(ABOUT)).unwrap();
        assert!(about.contains("\"name\": \"app\""));
        assert!(store.user().folder().join(FILE).exists());
    }

    /// Updating changes the text or tags and records when and by whom; forgetting removes it; unknown ids and empty updates are refused.
    #[test]
    fn update_and_forget() {
        let (_store, memories) = memories("store-update");
        let id = memories
            .remember("Old fact.", &[], Scope::User)
            .unwrap()
            .memory
            .id;
        let updated = memories.update(&id, Some("New fact."), None).unwrap();
        assert_eq!(updated.text, "New fact.");
        assert_eq!(updated.scope, Scope::User);
        assert!(updated.updated.is_some());
        assert_eq!(updated.updated_in.as_deref(), Some("conv-1"));
        let tagged = memories.update(&id, None, Some(&["a".to_owned()])).unwrap();
        assert_eq!((tagged.text.as_str(), tagged.tags.len()), ("New fact.", 1));
        assert!(memories.update(&id, None, None).is_err());
        assert!(memories.update("nope", Some("x"), None).is_err());
        assert_eq!(memories.forget(&id).unwrap().text, "New fact.");
        assert!(memories.all().unwrap().is_empty());
        assert!(memories.forget(&id).is_err());
    }

    /// The same text saved twice is kept once, with the tags of both; empty and over-long text and too many tags are refused.
    #[test]
    fn duplicates_and_limits() {
        let (_store, memories) = memories("store-limits");
        let first = memories
            .remember("Use tabs.", &["style".to_owned()], Scope::Project)
            .unwrap();
        let second = memories
            .remember("  use   TABS. ", &["format".to_owned()], Scope::Project)
            .unwrap();
        assert!(second.duplicate);
        assert_eq!(second.memory.id, first.memory.id);
        assert_eq!(second.memory.tags, ["style", "format"]);
        assert_eq!(memories.all().unwrap().len(), 1);
        assert!(memories.remember(" ", &[], Scope::Project).is_err());
        let long = "x".repeat(MEMORY_LIMIT + 1);
        assert!(memories.remember(&long, &[], Scope::Project).is_err());
        let tags: Vec<String> = (0..=TAG_LIMIT).map(|n| n.to_string()).collect();
        assert!(memories.remember("Other.", &tags, Scope::Project).is_err());
    }

    /// A line that is not a memory, such as one a hand edit broke, is kept through changes and skipped when reading.
    #[test]
    fn unreadable_lines_are_kept() {
        let (_store, memories) = memories("store-unreadable");
        let shelf = memories.shelf(Scope::Project);
        std::fs::create_dir_all(shelf.folder()).unwrap();
        std::fs::write(shelf.folder().join(FILE), "{broken\n").unwrap();
        memories.remember("Fact.", &[], Scope::Project).unwrap();
        assert_eq!(shelf.list().unwrap().len(), 1);
        let text = std::fs::read_to_string(shelf.folder().join(FILE)).unwrap();
        assert!(text.ends_with("{broken\n"));
    }

    /// Writers in many threads at once, each with a store of its own as separate conversations have, lose nothing.
    #[test]
    fn concurrent_writers_lose_nothing() {
        let (store, memories) = memories("store-concurrent");
        let project = memories.project().clone();
        let writers: Vec<_> = (0..8)
            .map(|writer| {
                let store = store.clone();
                let project = project.clone();
                std::thread::spawn(move || {
                    let mine = Memories::new(&store, project, Some(format!("conv-{writer}")));
                    for n in 0..10 {
                        mine.remember(&format!("Fact {writer}-{n}."), &[], Scope::Project)
                            .unwrap();
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }
        let all = memories.all().unwrap();
        assert_eq!(all.len(), 80);
        let mut ids: Vec<&str> = all.iter().map(|memory| memory.id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 80);
    }

    /// Scopes parse from their names.
    #[test]
    fn scopes_parse() {
        for scope in [Scope::Project, Scope::User] {
            assert_eq!(Scope::parse(scope.as_str()), Some(scope));
        }
        assert_eq!(Scope::parse("team"), None);
    }
}
