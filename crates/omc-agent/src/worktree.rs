//! Copies of the working tree for subagents that work apart from the main agent: copy-on-write clones where the file system can make them, git worktrees checked out as git makes them where it cannot.
//!
//! An isolated subagent works in a copy of its own of the working tree the main agent works in, whichever that is: the repository's own checkout, a linked worktree, or a folder outside any repository. [`Backend`] picks how the copy is made, as [`ISOLATION_VARIABLE`] says.
//!
//! Inside a git repository, the working tree's state is first written into a commit no branch points at, uncommitted and untracked changes included and ignored files left out, through an index of its own so that the main agent's index is never touched. Each subagent then gets a worktree of the repository on a detached head at that commit, so no branch name is ever taken. For a copy-on-write clone the worktree is registered without checking anything out and filled with clones of the working tree's own files, as [`crate::cow`] makes them, ignored files such as dependencies and build output included, so it costs little whatever the tree holds; where the file system cannot clone, the worktree checks the commit out, which leaves ignored files out. Submodules are not checked out in either, and what changes in them is not handed back.
//!
//! Outside a git repository, which needs copy-on-write clones, the folder is cloned once, untouched, as the state the subagents start from, and each subagent works in a clone of that; what it changed is found and applied file by file, as [`crate::files`] tells.
//!
//! Copies are made outside the main agent's working tree, in a folder of the conversation under the harness's cache folder, so its status, searches and file watchers never see them, and worktrees are locked with [`LOCK_REASON`], which tells them apart from every other worktree in `git worktree list --porcelain`.
//!
//! When the subagent ends, what it changed is taken against the state it started from and its copy is removed. The changes are then applied to the main agent's working tree, uncommitted; changes that do not apply cleanly are not applied at all, and are saved as a patch for the main agent to merge by hand. Copies left behind by a run that was stopped or crashed are removed by [`prune`] when a conversation opens.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omc_tools::{Registry, Spill, ToolError};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

use crate::backend::Backend;
use crate::cow;
use crate::files::{self, Change};

/// The reason every worktree made for a subagent is locked with, which is how other programs listing a repository's worktrees tell them apart.
pub const LOCK_REASON: &str = "oh-my-crab task worktree";

/// The file in a worktree's folder that says which working tree it was made from and where it is, so that it can be removed after a crash.
const ORIGIN: &str = "origin";

/// The name, in a worktree's folder, of a hooks folder that never exists, so that no hook of the repository runs when a worktree is made.
const NO_HOOKS: &str = "no-hooks";

/// The variables that would point git at another repository, index or working tree than the one it is run in.
const GIT_PLACES: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
];

/// The number the next worktree folder of this process gets.
static NEXT: AtomicU64 = AtomicU64::new(1);

/// Held while a worktree is made or removed or a patch applied, so that git never does two of those to one repository at once.
static GIT_LOCK: Mutex<()> = Mutex::const_new(());

/// Builds the tools that touch files, confined to the folder it is given: what an isolated subagent is given instead of the main agent's.
pub type ToolsFor = dyn Fn(&Path) -> Result<Registry, ToolError> + Send + Sync;

/// What the `task` tool needs to run subagents in worktrees of their own.
pub struct Isolation {
    /// The folder the main agent works in.
    pub directory: PathBuf,
    /// The folder the worktrees of the conversation are made in, outside the main agent's working tree.
    pub store: PathBuf,
    /// Where patches that were not applied are saved, so that the main agent can read them.
    pub patches: Spill,
    /// Builds the tools that touch files for the folder an isolated subagent works in.
    pub tools: Arc<ToolsFor>,
    /// How the copies are made.
    pub backend: Backend,
}

/// What became of the changes an isolated subagent made in its worktree.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Merge {
    /// They were applied to the main agent's working tree; these files, from the root of the repository, changed.
    Applied(Vec<String>),
    /// It changed nothing.
    Unchanged,
    /// They did not apply cleanly to the main agent's working tree, so none of them were applied.
    Conflicted {
        /// The file the whole patch was saved in.
        patch: PathBuf,
        /// The files whose part of the patch does not apply, from the root of the repository.
        files: Vec<String>,
    },
    /// They were not applied, for a reason other than a conflict.
    Held {
        /// Why they were not applied.
        reason: String,
        /// The file the whole patch was saved in, when it could be saved.
        patch: Option<PathBuf>,
    },
}

impl Merge {
    /// What the main agent is told about it, after the subagent's own result.
    pub(crate) fn note(&self) -> String {
        match self {
            Merge::Applied(files) => format!(
                "Its changes were applied to your working tree, uncommitted. Files changed:\n{}",
                listed(files)
            ),
            Merge::Unchanged => "It changed no files.".to_owned(),
            Merge::Conflicted { patch, files } => format!(
                "Its changes did NOT apply cleanly to your working tree, so none of them were applied. Files that conflict:\n{}\nThe whole patch is in {}: read it and make the changes yourself, or try `git apply --3way` with it.",
                listed(files),
                patch.display()
            ),
            Merge::Held {
                reason,
                patch: Some(patch),
            } => format!(
                "Its changes were not applied: {reason}. The whole patch is in {}.",
                patch.display()
            ),
            Merge::Held {
                reason,
                patch: None,
            } => format!("Its changes were not applied: {reason}."),
        }
    }
}

/// `files` as a list, one to a line.
fn listed(files: &[String]) -> String {
    files
        .iter()
        .map(|file| format!("- {file}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The state of the main agent's working tree, for new copies to start from.
pub(crate) struct Snapshot {
    /// The root of the main agent's working tree.
    top: PathBuf,
    /// Where in it the main agent works, from its root.
    within: PathBuf,
    /// What holds the state.
    base: Base,
    /// The submodules, from the root, which are not checked out in new copies.
    pub(crate) submodules: Vec<String>,
}

/// What holds the state a copy starts from, which its changes are taken against.
#[derive(Clone)]
enum Base {
    /// A commit of the repository the working tree belongs to.
    Commit(String),
    /// An untouched clone of a folder outside any repository.
    Pristine(Arc<Pristine>),
}

/// An untouched clone of a folder outside any repository, removed in the background once nothing needs it.
struct Pristine {
    /// Its folder in the store.
    folder: PathBuf,
    /// The clone itself, inside `folder`.
    tree: PathBuf,
}

impl Drop for Pristine {
    /// Removes the clone on a thread of its own.
    fn drop(&mut self) {
        let folder = self.folder.clone();
        std::thread::spawn(move || std::fs::remove_dir_all(folder));
    }
}

/// A copy made for one subagent, removed in the background when it is dropped unless it was removed or kept already.
pub(crate) struct Lease {
    /// The root of the main agent's working tree, which the copy was made from.
    top: PathBuf,
    /// The folder of the copy in the store, holding the copy and what says where it came from.
    folder: PathBuf,
    /// The root of the copy.
    tree: PathBuf,
    /// Where in it the subagent works, from its root.
    within: PathBuf,
    /// The state it started from.
    base: Base,
    /// Whether it is a copy-on-write clone, ignored files included, rather than a checkout.
    cloned: bool,
    /// Whether it was removed or kept already, so that dropping it does nothing more.
    settled: bool,
}

/// What an isolated subagent changed, taken from its copy.
pub(crate) struct Handback {
    /// The root of the main agent's working tree, to apply the changes to.
    top: PathBuf,
    /// The folder the subagent worked in.
    pub(crate) directory: PathBuf,
    /// The changes, or why they could not be taken.
    taken: Result<Taken, String>,
}

/// The changes taken from a copy.
enum Taken {
    /// A patch git made against a commit, and the files it changes.
    Patch(Vec<u8>, Vec<String>),
    /// Files that differ from the untouched clone, which they are checked against.
    Files(Vec<Change>, Arc<Pristine>),
}

/// Why a copy-on-write clone was not made.
enum Refusal {
    /// The file system or platform cannot make one.
    Unsupported(String),
    /// Something else failed.
    Failed(String),
}

impl Refusal {
    /// What to say about it.
    fn text(self) -> String {
        match self {
            Refusal::Unsupported(text) | Refusal::Failed(text) => text,
        }
    }
}

/// The settings every git run of the worktrees is given, so that files travel between the main agent's working tree and a subagent's byte for byte: git's own conversion of line endings, which `core.autocrlf` turns on for many Windows installs, is off.
const BYTE_FOR_BYTE: [&str; 2] = ["-c", "core.autocrlf=false"];

/// Runs git in `directory` with `arguments`, `environment` set and `input` on its standard input, and returns what it printed.
///
/// # Errors
///
/// Fails with what git said when it fails, or why it could not be run.
async fn git_with(
    directory: &Path,
    arguments: &[&str],
    environment: &[(&str, &str)],
    input: Option<&[u8]>,
) -> Result<Vec<u8>, String> {
    let mut command = tokio::process::Command::new("git");
    command
        .args(BYTE_FOR_BYTE)
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in GIT_PLACES {
        command.env_remove(name);
    }
    for (name, value) in environment {
        command.env(name, value);
    }
    let mut child = command.spawn().map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => "git is not installed, or not on the PATH".to_owned(),
        _ => format!("could not run git: {error}"),
    })?;
    if let (Some(input), Some(mut stdin)) = (input, child.stdin.take()) {
        stdin
            .write_all(input)
            .await
            .map_err(|error| format!("could not hand git its input: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Runs git in `directory` with `arguments` and returns what it printed, trimmed, as text.
///
/// # Errors
///
/// Same as [`git_with`].
async fn git(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    git_with(directory, arguments, &[], None)
        .await
        .map(|out| String::from_utf8_lossy(&out).trim().to_owned())
}

/// Runs git in `directory` with `arguments`, waiting for it to end, for the clean-ups that run outside of any task; whether it worked.
fn git_now(directory: &Path, arguments: &[&str]) -> bool {
    let mut command = std::process::Command::new("git");
    command
        .args(BYTE_FOR_BYTE)
        .arg("-C")
        .arg(directory)
        .args(arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for name in GIT_PLACES {
        command.env_remove(name);
    }
    command.status().is_ok_and(|status| status.success())
}

/// A name no other worktree folder made by any process gets.
fn unique() -> String {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!(
        "{}-{stamp}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// `path` with its symbolic links resolved, or as it is when it cannot be, in the ordinary form git can work with: Windows gives resolved paths the `\\?\` form for long paths, under which git cannot make the files it locks.
fn real(path: &Path) -> PathBuf {
    ordinary(&path.canonicalize().unwrap_or_else(|_| path.to_path_buf()))
}

/// `path` without the `\\?\` mark of Windows' long-path form: `\\?\C:\a` becomes `C:\a` and `\\?\UNC\host\share` becomes `\\host\share`; any other path is left as it is.
fn ordinary(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(shared) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{shared}"));
    }
    match text.strip_prefix(r"\\?\") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => path.to_path_buf(),
    }
}

/// Takes the state of the main agent's working tree, as [`Isolation::directory`] says where it is, for copies to start from: inside a git repository, a commit on top of its head holding every file git does not ignore as it is on disk; outside one, unless [`Backend::Worktree`] is asked for, an untouched copy-on-write clone of the folder.
///
/// # Errors
///
/// Fails, saying why, when the folder is in no git repository and cannot be cloned, git cannot be run, the store is inside the working tree, or git fails.
pub(crate) async fn snapshot(isolation: &Isolation) -> Result<Snapshot, String> {
    let directory = &isolation.directory;
    let found = git(directory, &["rev-parse", "--show-toplevel"])
        .await
        .ok()
        .filter(|top| !top.is_empty());
    let Some(top) = found.map(PathBuf::from) else {
        if isolation.backend != Backend::Worktree {
            return pristine(isolation).await;
        }
        if git(directory, &["--version"]).await.is_err() {
            return Err("git is not installed, or not on the PATH, so subagents cannot work in worktrees of their own; run them without `isolated`".to_owned());
        }
        return Err(format!(
            "{} is not in a git working tree, so subagents cannot work in worktrees of their own; run them without `isolated`",
            directory.display()
        ));
    };
    let within = PathBuf::from(git(directory, &["rev-parse", "--show-prefix"]).await?);
    std::fs::create_dir_all(&isolation.store)
        .map_err(|error| format!("cannot make {}: {error}", isolation.store.display()))?;
    let store = real(&isolation.store);
    let common = git(
        &top,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await
    .map(PathBuf::from)?;
    let main = (common.file_name().is_some_and(|name| name == ".git"))
        .then(|| common.parent().map(Path::to_path_buf))
        .flatten();
    if [Some(top.clone()), main]
        .into_iter()
        .flatten()
        .any(|tree| store.starts_with(real(&tree)))
    {
        let _ = std::fs::remove_dir(&isolation.store);
        return Err(format!(
            "the folder worktrees are made in, {}, is inside the repository's working tree, so subagents cannot work in worktrees of their own there",
            store.display()
        ));
    }
    let index = store.join(format!("index-{}", unique()));
    let taken = write_state(&top, &index).await;
    let _ = std::fs::remove_file(&index);
    let (commit, tree) = taken?;
    let listing = git_with(&top, &["ls-tree", "-r", "-z", &tree], &[], None).await?;
    let submodules = String::from_utf8_lossy(&listing)
        .split('\0')
        .filter_map(|entry| {
            let (meta, path) = entry.split_once('\t')?;
            (meta.split(' ').nth(1) == Some("commit")).then(|| path.to_owned())
        })
        .collect();
    Ok(Snapshot {
        top,
        within,
        base: Base::Commit(commit),
        submodules,
    })
}

/// Takes the state of the folder the main agent works in, which is in no git repository, as an untouched copy-on-write clone of it in [`Isolation::store`].
///
/// # Errors
///
/// Fails, saying why, when the store is inside the folder or cannot be made, or the folder cannot be cloned; nothing is left behind then.
async fn pristine(isolation: &Isolation) -> Result<Snapshot, String> {
    let directory = real(&isolation.directory);
    std::fs::create_dir_all(&isolation.store)
        .map_err(|error| format!("cannot make {}: {error}", isolation.store.display()))?;
    let store = real(&isolation.store);
    if store.starts_with(&directory) {
        let _ = std::fs::remove_dir(&isolation.store);
        return Err(format!(
            "{} is not in a git working tree, and the folder copies are made in, {}, is inside it, so subagents cannot work apart from you there; run them without `isolated`",
            directory.display(),
            store.display()
        ));
    }
    let folder = isolation.store.join(format!("base-{}", unique()));
    let tree = folder.join(named(&directory));
    let (from, to) = (directory.clone(), tree.clone());
    let cloned = tokio::task::spawn_blocking(move || cow::clone_entries(&from, &to, &[]))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    if let Err(error) = cloned {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(format!(
            "{} is not in a git working tree, and its files cannot be cloned copy-on-write here ({error}), so subagents cannot work apart from you; run them without `isolated`",
            directory.display()
        ));
    }
    Ok(Snapshot {
        top: directory,
        within: PathBuf::new(),
        base: Base::Pristine(Arc::new(Pristine { folder, tree })),
        submodules: Vec::new(),
    })
}

/// The name a copy of the folder `top` gets: its own, or `worktree` when it has none.
fn named(top: &Path) -> std::ffi::OsString {
    top.file_name()
        .map_or_else(|| "worktree".into(), |name| name.to_os_string())
}

/// Writes every file of the working tree at `top` that git does not ignore, as it is on disk, into a commit on top of its head, through the index file `index`, and returns the commit and its tree.
///
/// # Errors
///
/// Fails with what git said when it fails.
async fn write_state(top: &Path, index: &Path) -> Result<(String, String), String> {
    let index = index.to_string_lossy().into_owned();
    let indexed = [("GIT_INDEX_FILE", index.as_str())];
    let head = git(top, &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"])
        .await
        .ok()
        .filter(|head| !head.is_empty());
    let start = match &head {
        Some(head) => vec!["read-tree", head.as_str()],
        None => vec!["read-tree", "--empty"],
    };
    git_with(top, &start, &indexed, None).await?;
    git_with(top, &["add", "--all", "--", "."], &indexed, None).await?;
    let tree = String::from_utf8_lossy(&git_with(top, &["write-tree"], &indexed, None).await?)
        .trim()
        .to_owned();
    let mut arguments = vec![
        "commit-tree",
        "--no-gpg-sign",
        "-m",
        "State of the working tree an isolated subagent started from",
    ];
    if let Some(head) = &head {
        arguments.extend(["-p", head.as_str()]);
    }
    arguments.push(&tree);
    let identity = [
        ("GIT_AUTHOR_NAME", "oh-my-crab"),
        ("GIT_AUTHOR_EMAIL", "oh-my-crab@localhost"),
        ("GIT_COMMITTER_NAME", "oh-my-crab"),
        ("GIT_COMMITTER_EMAIL", "oh-my-crab@localhost"),
    ];
    let commit = String::from_utf8_lossy(&git_with(top, &arguments, &identity, None).await?)
        .trim()
        .to_owned();
    Ok((commit, tree))
}

/// Makes a copy of the working tree `snapshot` was taken of, in a folder of its own in [`Isolation::store`], as [`Isolation::backend`] says: inside a git repository a worktree on a detached head at the snapshot, locked with [`LOCK_REASON`], filled with copy-on-write clones or checked out; outside one, a clone of the untouched clone.
///
/// # Errors
///
/// Fails, saying why, when the folder cannot be made, git fails, or a clone is asked for and cannot be made; nothing is left behind then.
pub(crate) async fn lease(isolation: &Isolation, snapshot: &Snapshot) -> Result<Lease, String> {
    let folder = isolation.store.join(unique());
    let tree = folder.join(named(&snapshot.top));
    let mut lease = Lease {
        top: snapshot.top.clone(),
        folder,
        tree,
        within: snapshot.within.clone(),
        base: snapshot.base.clone(),
        cloned: true,
        settled: true,
    };
    let commit = match &snapshot.base {
        Base::Pristine(pristine) => {
            clone_into(&pristine.tree, &lease.tree, &[])
                .await
                .map_err(|refusal| {
                    let _ = std::fs::remove_dir_all(&lease.folder);
                    format!("cannot clone the folder: {}", refusal.text())
                })?;
            lease.settled = false;
            return Ok(lease);
        }
        Base::Commit(commit) => commit.clone(),
    };
    std::fs::create_dir_all(&lease.folder)
        .map_err(|error| format!("cannot make {}: {error}", lease.folder.display()))?;
    let origin = format!("{}\n{}\n", lease.top.display(), lease.tree.display());
    if let Err(error) = std::fs::write(lease.folder.join(ORIGIN), origin) {
        let _ = std::fs::remove_dir_all(&lease.folder);
        return Err(format!(
            "cannot write in {}: {error}",
            lease.folder.display()
        ));
    }
    let checkout = isolation.backend == Backend::Worktree;
    let hooks = format!("core.hooksPath={}", lease.folder.join(NO_HOOKS).display());
    let place = lease.tree.to_string_lossy().into_owned();
    let mut arguments = vec!["-c", &hooks, "worktree", "add", "--detach", "--lock"];
    if !checkout {
        arguments.push("--no-checkout");
    }
    arguments.extend(["--reason", LOCK_REASON, &place, &commit]);
    let made = {
        let _held = GIT_LOCK.lock().await;
        git(&lease.top, &arguments).await
    };
    if let Err(error) = made {
        let _ = std::fs::remove_dir_all(&lease.folder);
        return Err(format!("cannot make a worktree: {error}"));
    }
    lease.settled = false;
    lease.cloned = false;
    if checkout {
        return Ok(lease);
    }
    match fill(&lease, &commit, &snapshot.submodules).await {
        Ok(()) => {
            lease.cloned = true;
            Ok(lease)
        }
        Err(Refusal::Unsupported(_)) if isolation.backend == Backend::Auto => {
            check_out(&lease, &hooks).await?;
            Ok(lease)
        }
        Err(refusal) => Err(format!("cannot clone the working tree: {}", refusal.text())),
    }
}

/// Clones every entry of the folder `from` except those named in `skip` into the folder `to`, on a thread that may block.
///
/// # Errors
///
/// Returns [`Refusal::Unsupported`] when the file system cannot clone, and [`Refusal::Failed`] when anything else fails.
async fn clone_into(from: &Path, to: &Path, skip: &'static [&'static str]) -> Result<(), Refusal> {
    let (from, to) = (from.to_path_buf(), to.to_path_buf());
    let cloned = tokio::task::spawn_blocking(move || {
        let skip: Vec<&std::ffi::OsStr> = skip.iter().map(std::ffi::OsStr::new).collect();
        cow::clone_entries(&from, &to, &skip)
    })
    .await
    .unwrap_or_else(|error| Err(std::io::Error::other(error.to_string())));
    cloned.map_err(|error| {
        if cow::unsupported(&error) {
            Refusal::Unsupported(error.to_string())
        } else {
            Refusal::Failed(error.to_string())
        }
    })
}

/// Fills the worktree of `lease`, registered at `commit` without a checkout, with copy-on-write clones of the working tree's own files, its `.git` left out, and has its index hold `commit`; the `submodules` are left empty, as a checkout leaves them.
///
/// # Errors
///
/// Returns what [`clone_into`] does, and [`Refusal::Failed`] when git fails; the worktree is emptied again then, its `.git` kept.
async fn fill(lease: &Lease, commit: &str, submodules: &[String]) -> Result<(), Refusal> {
    let filled = async {
        clone_into(&lease.top, &lease.tree, &[".git"]).await?;
        for submodule in submodules {
            let path = lease.tree.join(submodule);
            if path.is_dir() {
                let _ = std::fs::remove_dir_all(&path);
                let _ = std::fs::create_dir_all(&path);
            }
        }
        git(&lease.tree, &["read-tree", commit])
            .await
            .map(drop)
            .map_err(Refusal::Failed)
    }
    .await;
    if filled.is_err() {
        empty_but_git(&lease.tree);
    }
    filled
}

/// Removes every entry of the folder `tree` except its `.git`.
fn empty_but_git(tree: &Path) {
    let Ok(entries) = std::fs::read_dir(tree) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(&path);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Checks the commit the worktree of `lease` was registered at out into it, with the repository's hooks turned off by `hooks`, when it could not be filled with clones.
///
/// # Errors
///
/// Fails with what git said when it fails.
async fn check_out(lease: &Lease, hooks: &str) -> Result<(), String> {
    git(&lease.tree, &["-c", hooks, "reset", "--hard", "--quiet"])
        .await
        .map(drop)
        .map_err(|error| format!("cannot check the worktree out: {error}"))
}

impl Lease {
    /// The folder the subagent works in: the copy's counterpart of the folder the main agent works in.
    pub(crate) fn directory(&self) -> PathBuf {
        self.tree.join(&self.within)
    }

    /// Whether the copy is a copy-on-write clone, which holds the files git ignores too.
    pub(crate) fn cloned(&self) -> bool {
        self.cloned
    }

    /// Takes what the subagent changed against the state it started from, committed or not, and removes the copy; a copy whose changes cannot be taken is kept, and the reason names it.
    pub(crate) async fn hand_back(mut self) -> Handback {
        let taken = match &self.base {
            Base::Commit(commit) => self
                .take(commit)
                .await
                .map(|(patch, files)| Taken::Patch(patch, files)),
            Base::Pristine(pristine) => {
                let (before, after) = (pristine.tree.clone(), self.tree.clone());
                tokio::task::spawn_blocking(move || files::changes(&before, &after))
                    .await
                    .unwrap_or_else(|error| Err(error.to_string()))
                    .map(|changes| Taken::Files(changes, pristine.clone()))
            }
        };
        if taken.is_ok() {
            self.remove().await;
        }
        self.settled = true;
        Handback {
            top: self.top.clone(),
            directory: self.directory(),
            taken: taken.map_err(|reason| {
                format!(
                    "its changes could not be taken ({reason}); its copy is kept at {}",
                    self.tree.display()
                )
            }),
        }
    }

    /// The patch of everything that differs in the worktree from `commit`, which it started from, and the files it changes.
    ///
    /// # Errors
    ///
    /// Fails with what git said when it fails.
    async fn take(&self, commit: &str) -> Result<(Vec<u8>, Vec<String>), String> {
        git(&self.tree, &["add", "--all", "--", "."]).await?;
        let shared = [
            "--cached",
            "--no-renames",
            "--no-relative",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--ignore-submodules=all",
        ];
        let mut patch = vec!["diff", "--binary", "--src-prefix=a/", "--dst-prefix=b/"];
        patch.extend(shared);
        patch.push(commit);
        let mut names = vec!["diff", "--name-only", "-z"];
        names.extend(shared);
        names.push(commit);
        let files = String::from_utf8_lossy(&git_with(&self.tree, &names, &[], None).await?)
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        Ok((git_with(&self.tree, &patch, &[], None).await?, files))
    }

    /// Removes the copy and its folder, and has git forget a worktree. A clone, which may hold many ignored files, is moved aside and git forgets it at once, and its files are deleted on a thread of its own, so that handing changes back does not wait for that.
    async fn remove(&self) {
        let (top, tree, folder) = (self.top.clone(), self.tree.clone(), self.folder.clone());
        if matches!(self.base, Base::Pristine(_)) {
            std::thread::spawn(move || std::fs::remove_dir_all(folder));
            return;
        }
        let _held = GIT_LOCK.lock().await;
        if self.cloned {
            let _ = tokio::task::spawn_blocking(move || discard_now(&top, &tree, &folder)).await;
        } else {
            let _ = tokio::task::spawn_blocking(move || remove_now(&top, &tree, &folder)).await;
        }
    }
}

impl Drop for Lease {
    /// Removes the copy on a thread of its own, when the subagent was stopped before it was handed back.
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let (top, tree, folder) = (self.top.clone(), self.tree.clone(), self.folder.clone());
        if matches!(self.base, Base::Pristine(_)) {
            std::thread::spawn(move || std::fs::remove_dir_all(folder));
        } else {
            std::thread::spawn(move || remove_now(&top, &tree, &folder));
        }
    }
}

/// Has git forget the cloned worktree at `tree`, made from the working tree at `top`, by moving its folder `folder` aside and pruning, then deletes the moved folder on a thread of its own; when it cannot be moved, removes it as [`remove_now`] does.
fn discard_now(top: &Path, tree: &Path, folder: &Path) {
    let place = tree.to_string_lossy();
    let aside = folder.with_file_name(format!("trash-{}", unique()));
    if !top.is_dir() || !git_now(top, &["worktree", "unlock", &place]) {
        remove_now(top, tree, folder);
        return;
    }
    if std::fs::rename(folder, &aside).is_err() {
        remove_now(top, tree, folder);
        return;
    }
    git_now(top, &["worktree", "prune"]);
    std::thread::spawn(move || std::fs::remove_dir_all(aside));
}

/// Removes the worktree at `tree`, made from the working tree at `top`, and its folder `folder`, and has git forget it; a worktree git cannot remove is unlocked and deleted by hand. When `top` is gone, git is run in the repository's own folder, which the worktree names.
fn remove_now(top: &Path, tree: &Path, folder: &Path) {
    let repository = if top.is_dir() {
        top.to_path_buf()
    } else {
        match common_folder(tree) {
            Some(common) => common,
            None => {
                let _ = std::fs::remove_dir_all(folder);
                return;
            }
        }
    };
    let place = tree.to_string_lossy();
    if !git_now(
        &repository,
        &["worktree", "remove", "--force", "--force", &place],
    ) {
        git_now(&repository, &["worktree", "unlock", &place]);
        let _ = std::fs::remove_dir_all(tree);
    }
    let _ = std::fs::remove_dir_all(folder);
    git_now(&repository, &["worktree", "prune"]);
}

/// The folder git shares between every worktree of the repository the worktree at `tree` belongs to, as its `.git` file names it.
fn common_folder(tree: &Path) -> Option<PathBuf> {
    let pointer = std::fs::read_to_string(tree.join(".git")).ok()?;
    let own = PathBuf::from(pointer.trim().strip_prefix("gitdir:")?.trim());
    Some(own.parent()?.parent()?.to_path_buf())
}

impl Handback {
    /// Applies the changes to the main agent's working tree when `finished`, the subagent having ended well, and they apply cleanly, as a patch through git or file by file outside a repository, and says what became of them; changes that are not applied are saved in `patches` as a patch.
    pub(crate) async fn settle(self, finished: bool, patches: &Spill) -> Merge {
        let (patch, files) = match self.taken {
            Ok(Taken::Patch(patch, files)) => (patch, files),
            Ok(Taken::Files(changes, pristine)) => {
                let _held = GIT_LOCK.lock().await;
                return files::settle(&self.top, &pristine.tree, &changes, finished, patches);
            }
            Err(reason) => {
                return Merge::Held {
                    reason,
                    patch: None,
                };
            }
        };
        if files.is_empty() {
            return Merge::Unchanged;
        }
        if !finished {
            return held("the subagent did not finish", &patch, patches);
        }
        let _held = GIT_LOCK.lock().await;
        let checked = apply(&self.top, &patch, &["--check"]).await;
        let applied = match checked {
            Ok(()) => apply(&self.top, &patch, &[]).await,
            Err(error) => Err(error),
        };
        match applied {
            Ok(()) => Merge::Applied(files),
            Err(why) => {
                let conflicting = conflicts(&self.top, &patch, &files).await;
                match patches.save("worktree-patch", &patch) {
                    Ok(saved) => Merge::Conflicted {
                        patch: saved,
                        files: if conflicting.is_empty() {
                            files
                        } else {
                            conflicting
                        },
                    },
                    Err(error) => Merge::Held {
                        reason: format!(
                            "it does not apply cleanly ({why}), and the patch could not be saved: {error}"
                        ),
                        patch: None,
                    },
                }
            }
        }
    }
}

/// The changes `patch` holds, held back for `reason` and saved in `patches`.
pub(crate) fn held(reason: &str, patch: &[u8], patches: &Spill) -> Merge {
    match patches.save("worktree-patch", patch) {
        Ok(saved) => Merge::Held {
            reason: reason.to_owned(),
            patch: Some(saved),
        },
        Err(error) => Merge::Held {
            reason: format!("{reason}, and the patch could not be saved: {error}"),
            patch: None,
        },
    }
}

/// Applies `patch` to the working tree at `top`, leaving its index alone, with `extra` arguments such as `--check`.
///
/// # Errors
///
/// Fails with what git said when the patch does not apply; nothing is changed then.
async fn apply(top: &Path, patch: &[u8], extra: &[&str]) -> Result<(), String> {
    let mut arguments = vec!["apply", "--whitespace=nowarn"];
    arguments.extend(extra);
    arguments.push("-");
    git_with(top, &arguments, &[], Some(patch)).await.map(drop)
}

/// The files among `files`, all of which `patch` changes, whose part of it does not apply to the working tree at `top`.
async fn conflicts(top: &Path, patch: &[u8], files: &[String]) -> Vec<String> {
    let mut found = Vec::new();
    for file in files {
        let include = format!("--include={}", escape_glob(file));
        if apply(top, patch, &["--check", &include]).await.is_err() {
            found.push(file.clone());
        }
    }
    found
}

/// `path` with every character a glob pattern gives a meaning to escaped, so that it matches only itself.
fn escape_glob(path: &str) -> String {
    path.chars().fold(String::new(), |mut escaped, c| {
        if matches!(c, '*' | '?' | '[' | ']' | '\\') {
            escaped.push('\\');
        }
        escaped.push(c);
        escaped
    })
}

/// Removes the copies left behind in `root`, the folder each conversation's copies are made in a folder of: every one of the conversation `current`, which has nothing running as it opens, and those of the rest made more than `age` ago. Worktrees, cloned or checked out, are removed through git; untouched clones of folders outside a repository and clones moved aside to be deleted are deleted.
pub fn prune(root: &Path, current: Option<&str>, age: Duration) {
    let Ok(conversations) = std::fs::read_dir(root) else {
        return;
    };
    let now = SystemTime::now();
    for conversation in conversations.flatten() {
        let whole = current.is_some_and(|current| conversation.file_name() == current);
        let Ok(folders) = std::fs::read_dir(conversation.path()) else {
            continue;
        };
        for folder in folders.flatten() {
            let path = folder.path();
            let old = folder
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|made| now.duration_since(made).ok())
                .is_some_and(|since| since > age);
            if !(whole || old) {
                continue;
            }
            match std::fs::read_to_string(path.join(ORIGIN)) {
                Ok(origin) => {
                    let mut lines = origin.lines();
                    if let (Some(top), Some(tree)) = (lines.next(), lines.next()) {
                        remove_now(Path::new(top), Path::new(tree), &path);
                    }
                    let _ = std::fs::remove_dir_all(&path);
                }
                Err(_) if path.is_dir() => {
                    let _ = std::fs::remove_dir_all(&path);
                }
                Err(_) => {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
        let _ = std::fs::remove_dir(conversation.path());
    }
}

#[cfg(test)]
/// Tests for the paths handed to git and the fallback from clones to checkouts.
mod tests {
    use super::*;

    /// A repository in a scratch folder called `name`, holding `a.txt` committed, an ignored `noise.log`, and the file that has clones of it refused; with how its subagents get copies by `backend`.
    fn refusing(name: &str, backend: Backend) -> (PathBuf, Isolation) {
        let root = crate::testkit::scratch(name);
        let top = root.join("repo");
        std::fs::create_dir_all(&top).unwrap();
        for arguments in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "test"],
            &["config", "user.email", "test@localhost"],
            &["config", "commit.gpgsign", "false"],
        ] {
            assert!(git_now(&top, arguments));
        }
        std::fs::write(top.join("a.txt"), "alpha\n").unwrap();
        std::fs::write(top.join(".gitignore"), "*.log\n").unwrap();
        std::fs::write(top.join("noise.log"), "noise\n").unwrap();
        std::fs::write(top.join(".git/info/exclude"), format!("{}\n", cow::REFUSE)).unwrap();
        std::fs::write(top.join(cow::REFUSE), "").unwrap();
        assert!(git_now(&top, &["add", "."]));
        assert!(git_now(&top, &["commit", "-q", "-m", "start"]));
        let isolation = Isolation {
            directory: top.clone(),
            store: root.join("store"),
            patches: Spill::new(root.join("patches")),
            tools: Arc::new(|folder: &Path| Registry::builtin(folder)),
            backend,
        };
        (top, isolation)
    }

    /// Where clones cannot be made, `auto` checks a worktree out instead, without the ignored files, and its changes come back; `cow` refuses.
    #[tokio::test]
    async fn auto_falls_back_to_a_checkout() {
        let (top, isolation) = refusing("fallback-auto", Backend::Auto);
        let snapshot = snapshot(&isolation).await.unwrap();
        let lease = lease(&isolation, &snapshot).await.unwrap();
        assert!(!lease.cloned());
        let copy = lease.directory();
        assert!(!copy.join("noise.log").exists());
        assert_eq!(
            std::fs::read_to_string(copy.join("a.txt")).unwrap(),
            "alpha\n"
        );
        std::fs::write(copy.join("a.txt"), "alpha\nfallback\n").unwrap();
        let merge = lease
            .hand_back()
            .await
            .settle(true, &isolation.patches)
            .await;
        assert_eq!(merge, Merge::Applied(vec!["a.txt".into()]));
        assert_eq!(
            std::fs::read_to_string(top.join("a.txt")).unwrap(),
            "alpha\nfallback\n"
        );
        assert!(!copy.exists());

        let (_, strict) = refusing("fallback-cow", Backend::Cow);
        let snapshot = super::snapshot(&strict).await.unwrap();
        let refused = super::lease(&strict, &snapshot).await.err().unwrap();
        assert!(refused.contains("cannot clone"), "{refused}");
    }

    /// The long-path mark is taken off drive and network paths, and every other path is kept.
    #[test]
    fn long_paths_become_ordinary() {
        assert_eq!(ordinary(Path::new(r"\\?\D:\a\b")), PathBuf::from(r"D:\a\b"));
        assert_eq!(
            ordinary(Path::new(r"\\?\UNC\host\share\x")),
            PathBuf::from(r"\\host\share\x")
        );
        assert_eq!(
            ordinary(Path::new(r"\\?\Volume{1}\x")),
            PathBuf::from(r"\\?\Volume{1}\x")
        );
        assert_eq!(ordinary(Path::new("/home/a")), PathBuf::from("/home/a"));
    }
}
