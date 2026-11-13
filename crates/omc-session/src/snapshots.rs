//! The files of the folder the agent works in, kept before and after each turn under git references of their own, so that what a turn changed can be told, taken back out, and the files put back as they were before it.
//!
//! Nothing here touches the branch, the head or what the reader staged: the files are staged into an index of their own and written as commits no branch holds.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use omc_wire::TurnChange;

/// The variables that would point git at another repository, index or working tree than the one it is run in.
const GIT_PLACES: [&str; 4] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_COMMON_DIR",
];

/// Who the commits holding kept files are by, so that no identity of the reader's is needed.
const IDENTITY: [&str; 4] = [
    "-c",
    "user.name=oh-my-crab",
    "-c",
    "user.email=oh-my-crab@localhost",
];

/// The most lines of one file's diff that are sent.
const DIFF_LINES: usize = 2000;

/// The name, among the files git keeps for a worktree, of the index the files are staged into.
const INDEX: &str = "oh-my-crab.index";

/// The name, among the files git keeps for a worktree, of the patch that takes a turn's changes back out.
const PATCH: &str = "oh-my-crab.patch";

/// What a turn's files are set against to tell what it changed.
pub(crate) enum End {
    /// The files as they were kept when the turn ended.
    Kept,
    /// The files as they are now, for a turn that has not ended.
    Now,
}

/// Where the files of one conversation's turns are kept.
#[derive(Clone, Debug)]
pub(crate) struct Snapshots {
    /// The folder the agent works in.
    directory: PathBuf,
    /// The part of the references' names that is the conversation's own.
    scope: String,
}

impl Snapshots {
    /// The keeper of the files of the conversation `id` working in `directory`, whose history is saved in the file `saved`, or of a conversation of this process that is not saved when there is no id.
    ///
    /// Two conversations saved under one id in different folders keep their files apart, since the file is part of what they are kept under.
    pub(crate) fn new(directory: &Path, id: Option<&str>, saved: Option<&Path>) -> Self {
        let scope = match (id, saved) {
            (Some(id), Some(saved)) => format!("{}-{:016x}", safe(id), digest(saved)),
            (Some(id), None) => safe(id),
            (None, _) => format!("unsaved-{}", std::process::id()),
        };
        Self {
            directory: directory.to_owned(),
            scope,
        }
    }

    /// The reference keeping the files as they were before the turn `turn`.
    fn before(&self, turn: &str) -> String {
        format!(
            "refs/worktree/oh-my-crab/{}/before/{}",
            self.scope,
            safe(turn)
        )
    }

    /// The reference keeping the files as they were when the turn `turn` ended.
    fn after(&self, turn: &str) -> String {
        format!(
            "refs/worktree/oh-my-crab/{}/after/{}",
            self.scope,
            safe(turn)
        )
    }

    /// The reference keeping the files as they were when a running turn was last looked at.
    fn now(&self) -> String {
        format!("refs/worktree/oh-my-crab/{}/now", self.scope)
    }

    /// The reference keeping the files as they were before they were last put back or had a turn's changes taken out.
    fn backup(&self) -> String {
        format!("refs/worktree/oh-my-crab/{}/backup", self.scope)
    }

    /// Keeps the files as they are before the turn `turn` starts.
    ///
    /// # Errors
    ///
    /// Fails with what git said, as when the folder is in no git repository.
    pub(crate) fn keep_before(&self, turn: &str) -> Result<(), String> {
        keep(&toplevel(&self.directory)?, &self.before(turn))
    }

    /// Keeps the files as they are now that the turn `turn` ended, when they were kept before it.
    ///
    /// # Errors
    ///
    /// Fails with what git said.
    pub(crate) fn keep_after(&self, turn: &str) -> Result<(), String> {
        let top = toplevel(&self.directory)?;
        resolve(&top, &self.before(turn))?;
        keep(&top, &self.after(turn))
    }

    /// The references the turn `turn` runs between: the files kept before it and, as `end` says, those kept when it ended or the files as they are now, kept first.
    ///
    /// # Errors
    ///
    /// Says that no files were kept before the turn, or after it, and fails with what git said otherwise.
    fn span(&self, top: &Path, turn: &str, end: &End) -> Result<(String, String), String> {
        let before = self.before(turn);
        resolve(top, &before).map_err(|_| format!("no files were kept before the turn {turn}"))?;
        let after = match end {
            End::Kept => {
                let after = self.after(turn);
                resolve(top, &after)
                    .map_err(|_| format!("no files were kept after the turn {turn}"))?;
                after
            }
            End::Now => {
                let now = self.now();
                keep(top, &now)?;
                now
            }
        };
        Ok((before, after))
    }

    /// The files the turn `turn` changed, each with its diff, set against the end `end` names.
    ///
    /// # Errors
    ///
    /// Says that no files were kept for the turn, and fails with what git said otherwise.
    pub(crate) fn changes(&self, turn: &str, end: &End) -> Result<Vec<TurnChange>, String> {
        let top = toplevel(&self.directory)?;
        let (before, after) = self.span(&top, turn, end)?;
        let listed = git(
            &top,
            &[
                "diff",
                "--name-status",
                "-z",
                "-M",
                "--no-color",
                &before,
                &after,
            ],
        )?;
        let mut found = Vec::new();
        let mut entries = listed.split('\0').filter(|entry| !entry.is_empty());
        while let Some(status) = entries.next() {
            let letter = status.chars().next().unwrap_or('M');
            let first = entries.next().unwrap_or_default();
            let (path, from) = match letter {
                'R' | 'C' => (entries.next().unwrap_or(first), Some(first.to_owned())),
                _ => (first, None),
            };
            let mut arguments = vec!["diff", "-M", "--no-color", &before, &after, "--"];
            arguments.extend(from.as_deref());
            arguments.push(path);
            let shown = git(&top, &arguments).unwrap_or_default();
            let (diff, truncated) = hunks(&shown);
            let (added, removed) = tally(&diff);
            found.push(TurnChange {
                path: path.to_owned(),
                from,
                status: letter.to_string(),
                diff,
                added,
                removed,
                truncated,
            });
        }
        Ok(found)
    }

    /// Puts the files back as they were before the turn `turn`, after keeping them as they are, and returns the files that changed, from the root of the repository; files made since are deleted.
    ///
    /// # Errors
    ///
    /// Says that no files were kept before the turn, and fails with what git said otherwise.
    pub(crate) fn restore(&self, turn: &str) -> Result<Vec<String>, String> {
        let top = toplevel(&self.directory)?;
        let target = self.before(turn);
        resolve(&top, &target).map_err(|_| format!("no files were kept before the turn {turn}"))?;
        let backup = self.backup();
        keep(&top, &backup)?;
        let changed = listed(&git(
            &top,
            &[
                "diff",
                "--name-only",
                "--no-renames",
                "-z",
                &backup,
                &target,
                "--",
            ],
        )?);
        let held: std::collections::HashSet<String> = listed(&git(
            &top,
            &["ls-tree", "-r", "--name-only", "-z", &target],
        )?)
        .into_iter()
        .collect();
        let (back, gone): (Vec<&String>, Vec<&String>) =
            changed.iter().partition(|path| held.contains(*path));
        for path in gone {
            let file = top.join(path);
            if !file.is_dir() {
                fs::remove_file(&file).or_else(|error| match error.kind() {
                    std::io::ErrorKind::NotFound => Ok(()),
                    _ => Err(format!("could not delete {path}: {error}")),
                })?;
            }
        }
        if !back.is_empty() {
            let source = format!("--source={target}");
            let mut arguments = vec!["restore", &source, "--worktree", "--"];
            arguments.extend(back.iter().map(|path| path.as_str()));
            git(&top, &arguments)?;
        }
        Ok(changed)
    }

    /// Takes what the turn `turn` changed in the files `paths`, from the root of the repository, or in every file when there are none, back out of them, after keeping them as they are, and returns the files that changed; what was done to the files since stays.
    ///
    /// # Errors
    ///
    /// Says that no files were kept for the turn; fails, leaving every file as it is, when lines the turn changed were changed again since, and with what git said otherwise.
    pub(crate) fn undo(&self, turn: &str, paths: &[String]) -> Result<Vec<String>, String> {
        let top = toplevel(&self.directory)?;
        let (before, after) = self.span(&top, turn, &End::Kept)?;
        let mut naming = vec![
            "diff",
            "--name-only",
            "--no-renames",
            "-z",
            &before,
            &after,
            "--",
        ];
        naming.extend(paths.iter().map(String::as_str));
        let changed = listed(&git(&top, &naming)?);
        if changed.is_empty() {
            return Ok(changed);
        }
        keep(&top, &self.backup())?;
        let patch = git_path(&top, PATCH)?;
        let output = format!("--output={}", patch.display());
        let mut writing = vec![
            "diff",
            "--binary",
            "--no-renames",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--src-prefix=a/",
            "--dst-prefix=b/",
            &output,
            &before,
            &after,
            "--",
        ];
        writing.extend(paths.iter().map(String::as_str));
        let file = patch.to_string_lossy();
        let applied = git(&top, &writing)
            .and_then(|_| git(&top, &["apply", "-R", "--whitespace=nowarn", &file]));
        let _ = fs::remove_file(&patch);
        applied
            .map(|_| changed)
            .map_err(|reason| format!("the files were changed again since: {reason}"))
    }
}

/// `name` as part of a reference's name: letters, digits and `-` as they are, every other byte as `_` and its two hexadecimal digits, so that no two names come out alike and none is refused by git.
fn safe(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' => char::from(byte).to_string(),
            other => format!("_{other:02x}"),
        })
        .collect()
}

/// A number that stands for `path` and stays the same from one run and one build to the next: the FNV-1a hash of its bytes.
fn digest(path: &Path) -> u64 {
    path.to_string_lossy()
        .bytes()
        .fold(0xcbf2_9ce4_8422_2325, |hash: u64, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

/// Runs `git` with `arguments` in `directory`, staging into the file `index` in place of the repository's own when one is given, and returns what it wrote, or what it complained of.
fn git_indexed(
    directory: &Path,
    arguments: &[&str],
    index: Option<&Path>,
) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(arguments)
        .current_dir(directory)
        .stdin(Stdio::null());
    for place in GIT_PLACES {
        command.env_remove(place);
    }
    command.env("GIT_LITERAL_PATHSPECS", "1");
    if let Some(index) = index {
        command.env("GIT_INDEX_FILE", index);
    }
    let output = command
        .output()
        .map_err(|error| format!("could not run git: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
    }
}

/// Runs `git` with `arguments` in `directory` and returns what it wrote, or what it complained of.
fn git(directory: &Path, arguments: &[&str]) -> Result<String, String> {
    git_indexed(directory, arguments, None)
}

/// The folder at the top of the worktree `directory` is in.
fn toplevel(directory: &Path) -> Result<PathBuf, String> {
    let top = git(directory, &["rev-parse", "--show-toplevel"])?;
    Ok(PathBuf::from(top.trim()))
}

/// Where the repository keeps its file `name` for the worktree `top` is the top of.
fn git_path(top: &Path, name: &str) -> Result<PathBuf, String> {
    let path = git(top, &["rev-parse", "--git-path", name])?;
    Ok(top.join(path.trim()))
}

/// The commit the reference `name` keeps.
fn resolve(top: &Path, name: &str) -> Result<String, String> {
    git(
        top,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{name}^{{commit}}"),
        ],
    )
    .map(|commit| commit.trim().to_owned())
}

/// The paths of `listing`, which separates them with nul characters.
fn listed(listing: &str) -> Vec<String> {
    listing
        .split('\0')
        .filter(|path| !path.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Keeps every file under `top` that the repository does not ignore, tracked or not, as it is on disk, under the reference `name`.
fn keep(top: &Path, name: &str) -> Result<(), String> {
    let index = git_path(top, INDEX)?;
    let own = git_path(top, "index")?;
    if fs::copy(&own, &index).is_err() {
        let _ = fs::remove_file(&index);
    }
    let staged = git_indexed(top, &["add", "--all", "--", "."], Some(&index));
    let tree = staged.and_then(|_| git_indexed(top, &["write-tree"], Some(&index)));
    let _ = fs::remove_file(&index);
    let tree = tree?;
    let mut arguments = IDENTITY.to_vec();
    arguments.extend(["commit-tree", tree.trim(), "-m", "Files kept by oh-my-crab"]);
    let commit = git(top, &arguments)?;
    git(top, &["update-ref", name, commit.trim()]).map(|_| ())
}

/// The lines of the diff `shown` from its first hunk on, at most [`DIFF_LINES`] of them, and whether some were left out.
fn hunks(shown: &str) -> (String, bool) {
    let mut lines = shown.lines().skip_while(|line| !line.starts_with("@@"));
    let kept: Vec<&str> = lines.by_ref().take(DIFF_LINES).collect();
    (kept.join("\n"), lines.next().is_some())
}

/// How many lines `diff` puts in and takes out.
fn tally(diff: &str) -> (u64, u64) {
    diff.lines().fold((0, 0), |(added, removed), line| {
        match line.as_bytes().first() {
            Some(b'+') => (added + 1, removed),
            Some(b'-') => (added, removed + 1),
            _ => (added, removed),
        }
    })
}

#[cfg(test)]
/// Tests of keeping, telling, undoing and restoring the files of turns against real repositories.
mod tests {
    use super::*;

    /// A fresh repository in a scratch folder named `name`, with one commit holding `a.txt`, and the keeper of an unsaved conversation in it.
    fn repository(name: &str) -> (PathBuf, Snapshots) {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q"]).unwrap();
        fs::write(root.join("a.txt"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
        git(&root, &["add", "."]).unwrap();
        let mut commit = IDENTITY.to_vec();
        commit.extend(["commit", "-q", "-m", "first"]);
        git(&root, &commit).unwrap();
        let snapshots = Snapshots::new(&root, Some("c/1"), None);
        (root, snapshots)
    }

    /// Ids become reference names git takes, and different ids stay different.
    #[test]
    fn ids_are_made_safe_for_references() {
        assert_eq!(safe("t-12"), "t-12");
        assert_eq!(safe("a/b c"), "a_2fb_20c");
        assert_ne!(safe("a_b"), safe("a b"));
        let one = Snapshots::new(
            Path::new("."),
            Some("history"),
            Some(Path::new("/s/1/history.jsonl")),
        );
        let other = Snapshots::new(
            Path::new("."),
            Some("history"),
            Some(Path::new("/s/2/history.jsonl")),
        );
        assert_ne!(one.before("1"), other.before("1"));
        assert_eq!(digest(Path::new("a")), 0xaf63_dc4c_8601_ec8c);
    }

    /// What a turn changed is told with its diffs while it runs and after it ended, what came after it left out, and nothing the reader staged is touched.
    #[test]
    fn a_turn_tells_what_it_changed() {
        let (root, snapshots) = repository("snapshots-changes");
        assert!(snapshots.changes("t 1", &End::Now).is_err());
        snapshots.keep_before("t 1").unwrap();
        fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\nsix\n").unwrap();
        fs::write(root.join("new.txt"), "made\n").unwrap();
        assert_eq!(snapshots.changes("t 1", &End::Now).unwrap().len(), 2);
        assert!(snapshots.changes("t 1", &End::Kept).is_err());
        snapshots.keep_after("t 1").unwrap();
        fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\nSIX\n").unwrap();
        let changes = snapshots.changes("t 1", &End::Kept).unwrap();
        let told: Vec<(&str, &str, u64, u64)> = changes
            .iter()
            .map(|change| {
                (
                    change.path.as_str(),
                    change.status.as_str(),
                    change.added,
                    change.removed,
                )
            })
            .collect();
        assert_eq!(told, [("a.txt", "M", 1, 1), ("new.txt", "A", 1, 0)]);
        assert!(changes[0].diff.starts_with("@@"));
        assert!(!changes[0].diff.contains("SIX"));
        assert!(
            git(&root, &["diff", "--cached", "--name-only"])
                .unwrap()
                .is_empty()
        );
        assert!(snapshots.keep_after("never started").is_err());
    }

    /// Undoing takes out one turn's change and keeps later work on the same file, and is refused, touching nothing, when the same lines were changed again.
    #[test]
    fn undoing_takes_out_only_the_turn() {
        let (root, snapshots) = repository("snapshots-undo");
        snapshots.keep_before("t1").unwrap();
        fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\nsix\n").unwrap();
        fs::write(root.join("new.txt"), "made\n").unwrap();
        snapshots.keep_after("t1").unwrap();
        fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\nfive\nSIX\n").unwrap();
        let changed = snapshots.undo("t1", &["a.txt".to_owned()]).unwrap();
        assert_eq!(changed, ["a.txt"]);
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\nthree\nfour\nfive\nSIX\n"
        );
        assert!(root.join("new.txt").exists());
        assert!(snapshots.undo("t1", &[]).is_err());
        assert!(root.join("new.txt").exists());
    }

    /// Restoring puts every file back as it was before the turn, deleting what was made since, and keeps what was there under the backup.
    #[test]
    fn restoring_puts_the_files_back() {
        let (root, snapshots) = repository("snapshots-restore");
        snapshots.keep_before("t1").unwrap();
        fs::write(root.join("a.txt"), "changed\n").unwrap();
        fs::write(root.join("new.txt"), "made\n").unwrap();
        let mut changed = snapshots.restore("t1").unwrap();
        changed.sort();
        assert_eq!(changed, ["a.txt", "new.txt"]);
        assert_eq!(
            fs::read_to_string(root.join("a.txt")).unwrap(),
            "one\ntwo\nthree\nfour\nfive\nsix\n"
        );
        assert!(!root.join("new.txt").exists());
        assert!(resolve(&root, &snapshots.backup()).is_ok());
        assert!(snapshots.restore("t9").is_err());
    }
}
