//! Git worktrees for subagents that work apart from the main agent.
//!
//! An isolated subagent works in a git worktree of its own, made from the worktree the main agent works in, whichever that is: the repository's own checkout or a linked worktree. The new worktree starts from that one's state as it is, uncommitted and untracked changes included and ignored files left out: the state is written into a commit no branch points at, through an index of its own so that the main agent's index is never touched, and the worktree checks that commit out on a detached head, so no branch name is ever taken. Submodules are not checked out in it, and what changes in them is not handed back.
//!
//! Worktrees are made outside the main agent's working tree, in a folder of the conversation under the harness's cache folder, so its status, searches and file watchers never see them, and they are locked with [`LOCK_REASON`], which tells them apart from every other worktree in `git worktree list --porcelain`.
//!
//! When the subagent ends, what it changed is taken as a patch against that commit and its worktree is removed. The patch is then applied to the main agent's working tree, uncommitted; one that does not apply cleanly is not applied at all, and is saved for the main agent to merge by hand. Worktrees left behind by a run that was stopped or crashed are removed by [`prune`] when a conversation opens.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omc_tools::{Registry, Spill, ToolError};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::Mutex;

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

/// The state of the main agent's working tree, written into a commit for new worktrees to start from.
pub(crate) struct Snapshot {
    /// The root of the main agent's working tree.
    top: PathBuf,
    /// Where in it the main agent works, from its root.
    within: PathBuf,
    /// The commit holding the state.
    commit: String,
    /// The submodules, from the root, which are not checked out in new worktrees.
    pub(crate) submodules: Vec<String>,
}

/// A worktree made for one subagent, removed in the background when it is dropped unless it was removed or kept already.
pub(crate) struct Lease {
    /// The root of the main agent's working tree, which the worktree was made from.
    top: PathBuf,
    /// The folder of the worktree in the store, holding the worktree and what says where it came from.
    folder: PathBuf,
    /// The root of the worktree.
    tree: PathBuf,
    /// Where in it the subagent works, from its root.
    within: PathBuf,
    /// The commit it started from.
    base: String,
    /// Whether it was removed or kept already, so that dropping it does nothing more.
    settled: bool,
}

/// What an isolated subagent changed, taken from its worktree.
pub(crate) struct Handback {
    /// The root of the main agent's working tree, to apply the changes to.
    top: PathBuf,
    /// The folder the subagent worked in.
    pub(crate) directory: PathBuf,
    /// The patch and the files it changes, or why they could not be taken.
    taken: Result<(Vec<u8>, Vec<String>), String>,
}

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

/// Takes the state of the main agent's working tree, as [`Isolation::directory`] says where it is, for worktrees to start from: a commit on top of its head holding every file git does not ignore as it is on disk.
///
/// # Errors
///
/// Fails, saying why, when git cannot be run, the folder is not in a git repository, the store is inside the working tree, or git fails.
pub(crate) async fn snapshot(isolation: &Isolation) -> Result<Snapshot, String> {
    let directory = &isolation.directory;
    let found = git(directory, &["rev-parse", "--show-toplevel"])
        .await
        .ok()
        .filter(|top| !top.is_empty());
    let Some(top) = found.map(PathBuf::from) else {
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
        commit,
        submodules,
    })
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

/// Makes a worktree of the working tree `snapshot` was taken of, in a folder of its own in [`Isolation::store`], checked out at the snapshot on a detached head and locked with [`LOCK_REASON`].
///
/// # Errors
///
/// Fails, saying why, when the folder cannot be made or git fails; nothing is left behind then.
pub(crate) async fn lease(isolation: &Isolation, snapshot: &Snapshot) -> Result<Lease, String> {
    let folder = isolation.store.join(unique());
    let name = snapshot
        .top
        .file_name()
        .map_or_else(|| "worktree".into(), |name| name.to_os_string());
    let tree = folder.join(name);
    std::fs::create_dir_all(&folder)
        .map_err(|error| format!("cannot make {}: {error}", folder.display()))?;
    let origin = format!("{}\n{}\n", snapshot.top.display(), tree.display());
    if let Err(error) = std::fs::write(folder.join(ORIGIN), origin) {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(format!("cannot write in {}: {error}", folder.display()));
    }
    let hooks = format!("core.hooksPath={}", folder.join(NO_HOOKS).display());
    let place = tree.to_string_lossy().into_owned();
    let made = {
        let _held = GIT_LOCK.lock().await;
        git(
            &snapshot.top,
            &[
                "-c",
                &hooks,
                "worktree",
                "add",
                "--detach",
                "--lock",
                "--reason",
                LOCK_REASON,
                &place,
                &snapshot.commit,
            ],
        )
        .await
    };
    if let Err(error) = made {
        let _ = std::fs::remove_dir_all(&folder);
        return Err(format!("cannot make a worktree: {error}"));
    }
    Ok(Lease {
        top: snapshot.top.clone(),
        folder,
        tree,
        within: snapshot.within.clone(),
        base: snapshot.commit.clone(),
        settled: false,
    })
}

impl Lease {
    /// The folder the subagent works in: the worktree's counterpart of the folder the main agent works in.
    pub(crate) fn directory(&self) -> PathBuf {
        self.tree.join(&self.within)
    }

    /// Takes what the subagent changed as a patch against the commit it started from, committed or not, and removes the worktree; a worktree whose changes cannot be taken is kept, and the reason names it.
    pub(crate) async fn hand_back(mut self) -> Handback {
        let taken = self.take().await;
        if taken.is_ok() {
            self.remove().await;
        }
        self.settled = true;
        Handback {
            top: self.top.clone(),
            directory: self.directory(),
            taken: taken.map_err(|reason| {
                format!(
                    "its changes could not be taken ({reason}); its worktree is kept at {}",
                    self.tree.display()
                )
            }),
        }
    }

    /// The patch of everything that differs in the worktree from the commit it started from, and the files it changes.
    ///
    /// # Errors
    ///
    /// Fails with what git said when it fails.
    async fn take(&self) -> Result<(Vec<u8>, Vec<String>), String> {
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
        patch.push(&self.base);
        let mut names = vec!["diff", "--name-only", "-z"];
        names.extend(shared);
        names.push(&self.base);
        let files = String::from_utf8_lossy(&git_with(&self.tree, &names, &[], None).await?)
            .split('\0')
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect();
        Ok((git_with(&self.tree, &patch, &[], None).await?, files))
    }

    /// Removes the worktree and its folder, and has git forget it.
    async fn remove(&self) {
        let _held = GIT_LOCK.lock().await;
        let (top, tree, folder) = (self.top.clone(), self.tree.clone(), self.folder.clone());
        let _ = tokio::task::spawn_blocking(move || remove_now(&top, &tree, &folder)).await;
    }
}

impl Drop for Lease {
    /// Removes the worktree on a thread of its own, when the subagent was stopped before it was handed back.
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let (top, tree, folder) = (self.top.clone(), self.tree.clone(), self.folder.clone());
        std::thread::spawn(move || remove_now(&top, &tree, &folder));
    }
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
    /// Applies the changes to the main agent's working tree when `finished`, the subagent having ended well, and the patch applies cleanly, and says what became of them; a patch that is not applied is saved in `patches`.
    pub(crate) async fn settle(self, finished: bool, patches: &Spill) -> Merge {
        let (patch, files) = match self.taken {
            Ok(taken) => taken,
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
fn held(reason: &str, patch: &[u8], patches: &Spill) -> Merge {
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

/// Removes the worktrees left behind in `root`, the folder each conversation's worktrees are made in a folder of: every one of the conversation `current`, which has nothing running as it opens, and those of the rest made more than `age` ago.
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
/// Tests for the paths handed to git.
mod tests {
    use super::*;

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
