//! The editing tasks: what the model is asked, the files it starts from and the files it should leave.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

/// The file in a task's folder that holds what the model is asked.
const PROMPT: &str = "prompt.md";

/// The folder in a task's folder that holds the files the model starts from.
const BEFORE: &str = "before";

/// The folder in a task's folder that holds the files the model should leave.
const AFTER: &str = "after";

/// The file in a task's folder that names the shared fixture the model starts from, in place of a folder of the task's own.
const FIXTURE: &str = "fixture";

/// The folder beside the tasks' folder that holds the shared fixtures, each in a folder of its name.
const FIXTURES: &str = "fixtures";

/// The file in a task's folder that holds the change the model should make, as a patch `git apply` takes, in place of a folder of the files it should leave.
const PATCH: &str = "change.patch";

/// The file in a task's folder that names the tools the model is given for it, separated by commas or lines, in place of the benchmark's own.
const TOOLS: &str = "tools";

/// The file in a task's folder that holds the command that says whether the model did what was asked: run by `bash` in the folder the model worked in, it ends successfully when it did.
const CHECK: &str = "check";

/// The folder git keeps its own files in, which is no part of a task's files.
const GIT: &str = ".git";

/// One editing task.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Task {
    /// The name of the task, which is the name of its folder.
    pub(crate) name: String,
    /// What the model is asked.
    pub(crate) prompt: String,
    /// The folder of the files the model starts from.
    pub(crate) before: PathBuf,
    /// The folder of the files the model should leave, when the task is scored on them; for a task that gives them as a patch, the folder [`Task::settle`] makes.
    pub(crate) after: Option<PathBuf>,
    /// The command that ends successfully when the model did what was asked, when the task is scored by one.
    pub(crate) check: Option<String>,
    /// The tools the model is given for this task, when it names its own.
    pub(crate) tools: Option<Vec<String>>,
    /// The patch that turns the files the model starts from into the ones it should leave, when the task gives them that way.
    pub(crate) patch: Option<PathBuf>,
}

impl Task {
    /// Makes sure the folder of the files the model should leave is there: a task that gives them as a patch gets a copy of the files it starts from under `scratch`, with the patch applied, and is pointed at it.
    ///
    /// # Errors
    ///
    /// Fails when the files cannot be copied or the patch does not apply to them. The patch is given to git as a plain absolute path, since git on Windows cannot open a `\\?\` path, and git is told to keep line endings as they are, since Windows git would otherwise turn them into CRLF.
    pub(crate) fn settle(&mut self, scratch: &Path) -> Result<(), String> {
        let Some(patch) = &self.patch else {
            return Ok(());
        };
        let after = scratch.join(&self.name);
        let _ = std::fs::remove_dir_all(&after);
        copy(&self.before, &after)?;
        init(&after)?;
        let patch = std::path::absolute(patch)
            .map_err(|error| format!("could not find {}: {error}", patch.display()))?;
        let applied = Process::new("git")
            .args(["-c", "core.autocrlf=false", "apply", "--whitespace=nowarn"])
            .arg(&patch)
            .current_dir(&after)
            .output()
            .map_err(|error| format!("could not run git apply: {error}"))?;
        if !applied.status.success() {
            return Err(format!(
                "the patch of task {} does not apply: {}",
                self.name,
                String::from_utf8_lossy(&applied.stderr).trim()
            ));
        }
        self.after = Some(after);
        Ok(())
    }

    /// The tools the model is given for this task: its own when it names them, else `usual`.
    pub(crate) fn tools<'a>(&'a self, usual: &'a [String]) -> &'a [String] {
        self.tools.as_deref().unwrap_or(usual)
    }
}

/// Makes `folder` a git repository of its own, so that what git and a harness take for the project ends there.
///
/// # Errors
///
/// Fails when `git init` cannot be run there.
pub(crate) fn init(folder: &Path) -> Result<(), String> {
    git(folder, &["init", "--quiet"])
}

/// Commits everything in the git repository `folder` as its first commit, so that a harness finds a clean working tree and a check can tell what the model changed.
///
/// # Errors
///
/// Fails when git cannot stage or commit the files.
pub(crate) fn commit(folder: &Path) -> Result<(), String> {
    git(folder, &["add", "--all"])?;
    git(
        folder,
        &[
            "-c",
            "user.name=crab-bench",
            "-c",
            "user.email=crab-bench@localhost",
            "commit",
            "--quiet",
            "--allow-empty",
            "--message",
            "Start",
        ],
    )
}

/// Runs git in `folder` with `arguments`, showing nothing of what it prints.
///
/// # Errors
///
/// Fails when git cannot be started or ends unsuccessfully.
fn git(folder: &Path, arguments: &[&str]) -> Result<(), String> {
    let asked = arguments.join(" ");
    let ran = Process::new("git")
        .args(arguments)
        .current_dir(folder)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("could not run git {asked}: {error}"))?;
    if ran.success() {
        Ok(())
    } else {
        Err(format!("git {asked} failed in {}", folder.display()))
    }
}

/// The tasks under `folder`, by name, or only those named in `only` when it names any.
///
/// # Errors
///
/// Fails when `folder` cannot be read, a task lacks its prompt or one of its two folders, or `only` names a task that is not there.
pub(crate) fn load(folder: &Path, only: &[String]) -> Result<Vec<Task>, String> {
    let entries = std::fs::read_dir(folder)
        .map_err(|error| format!("could not read {}: {error}", folder.display()))?;
    let mut tasks = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if !path.is_dir() || (!only.is_empty() && !only.contains(&name)) {
            continue;
        }
        tasks.push(read(name, &path, &folder.join("..").join(FIXTURES))?);
    }
    if let Some(missing) = only
        .iter()
        .find(|name| !tasks.iter().any(|task| &task.name == *name))
    {
        return Err(format!("no task is called {missing}"));
    }
    tasks.sort_by(|one, other| one.name.cmp(&other.name));
    Ok(tasks)
}

/// The task called `name` that the folder `path` holds, its shared fixture looked for under `fixtures` when it names one.
///
/// The files the model starts from are the task's own `before` folder, or the fixture its `fixture` file names. The task is scored on the files it should leave, which are its `after` folder or what its `change.patch` makes of the files it starts from, on the command in its `check` file, or on both. Its `tools` file names the tools the model gets, when it needs other than the usual ones.
///
/// # Errors
///
/// Fails when the prompt cannot be read or is empty, the files to start from are missing, or the task has nothing to be scored on.
fn read(name: String, path: &Path, fixtures: &Path) -> Result<Task, String> {
    let prompt = std::fs::read_to_string(path.join(PROMPT))
        .map_err(|error| format!("task {name} has no readable {PROMPT}: {error}"))?
        .trim()
        .to_owned();
    if prompt.is_empty() {
        return Err(format!("task {name} has an empty {PROMPT}"));
    }
    let before = match std::fs::read_to_string(path.join(FIXTURE)) {
        Ok(fixture) => fixtures.join(fixture.trim()),
        Err(_) => path.join(BEFORE),
    };
    if !before.is_dir() {
        return Err(format!("task {name} has no folder {}", before.display()));
    }
    let (after, patch) = (path.join(AFTER), path.join(PATCH));
    let patch = patch.is_file().then_some(patch);
    let after = after.is_dir().then_some(after);
    let check = written(&path.join(CHECK));
    if patch.is_none() && after.is_none() && check.is_none() {
        return Err(format!(
            "task {name} has none of {AFTER}, {PATCH} and {CHECK}"
        ));
    }
    let tools = written(&path.join(TOOLS)).map(|tools| {
        tools
            .split([',', '\n'])
            .map(str::trim)
            .filter(|tool| !tool.is_empty())
            .map(str::to_owned)
            .collect()
    });
    Ok(Task {
        name,
        prompt,
        before,
        after,
        check,
        tools,
        patch,
    })
}

/// What the file `path` says, without the white space around it, or nothing when it is missing, unreadable or empty.
fn written(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The files under `root` and what each holds, by path from `root` written with `/`, git's own folder left out.
///
/// # Errors
///
/// Fails when a folder or a file under `root` cannot be read.
pub(crate) fn files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>, String> {
    let mut found = BTreeMap::new();
    let mut folders = vec![(root.to_path_buf(), String::new())];
    while let Some((folder, prefix)) = folders.pop() {
        let entries = std::fs::read_dir(&folder)
            .map_err(|error| format!("could not read {}: {error}", folder.display()))?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if path.is_dir() {
                if name != GIT {
                    folders.push((path, format!("{prefix}{name}/")));
                }
            } else {
                let content = std::fs::read(&path)
                    .map_err(|error| format!("could not read {}: {error}", path.display()))?;
                found.insert(format!("{prefix}{name}"), content);
            }
        }
    }
    Ok(found)
}

/// Copies the files under `from` to the folder `to`, which is made.
///
/// # Errors
///
/// Fails when a file cannot be read or written.
pub(crate) fn copy(from: &Path, to: &Path) -> Result<(), String> {
    for (name, content) in files(from)? {
        let target = to.join(&name);
        let parent = target.parent().unwrap_or(to);
        std::fs::create_dir_all(parent)
            .and_then(|()| std::fs::write(&target, content))
            .map_err(|error| format!("could not write {}: {error}", target.display()))?;
    }
    std::fs::create_dir_all(to).map_err(|error| format!("could not make {}: {error}", to.display()))
}

/// The files that are not the same under `expected` and under `actual`: missing from one of them, or holding something else, by path.
///
/// Two files are the same when their lines are, white space at the end of a line and blank lines left out, so that an edit is not failed over spacing the task did not ask about.
///
/// # Errors
///
/// Fails when either folder cannot be read.
pub(crate) fn differing(expected: &Path, actual: &Path) -> Result<Vec<String>, String> {
    let (expected, actual) = (files(expected)?, files(actual)?);
    let mut differing: Vec<String> = expected
        .iter()
        .filter(|(name, content)| actual.get(*name).is_none_or(|found| !same(content, found)))
        .map(|(name, _)| name.clone())
        .collect();
    differing.extend(
        actual
            .keys()
            .filter(|name| !expected.contains_key(*name))
            .cloned(),
    );
    differing.sort();
    Ok(differing)
}

/// Whether the folders `expected` and `actual` hold the same files with the same bytes, git's own folder left out; they do not when either cannot be read.
pub(crate) fn identical(expected: &Path, actual: &Path) -> bool {
    matches!((files(expected), files(actual)), (Ok(expected), Ok(actual)) if expected == actual)
}

/// Whether the files holding `one` and `other` are the same: byte for byte when either is not text, else line for line as [`essence`] gives them.
fn same(one: &[u8], other: &[u8]) -> bool {
    match (std::str::from_utf8(one), std::str::from_utf8(other)) {
        (Ok(one), Ok(other)) => essence(one) == essence(other),
        _ => one == other,
    }
}

/// The lines of `text` that say something, each without the white space at its end.
fn essence(text: &str) -> Vec<&str> {
    text.lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect()
}

#[cfg(test)]
/// Tests for the tasks and for comparing what a run left.
mod tests {
    use super::*;

    /// A fresh folder for the test called `name`, under the workspace's build folder.
    fn scratch(name: &str) -> PathBuf {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-bench")
            .join(name);
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    /// Writes `content` to the file `name` under `folder`, making the folders above it.
    fn put(folder: &Path, name: &str, content: &str) {
        let path = folder.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// Spacing at the end of lines and blank lines do not make two files differ; anything else does.
    #[test]
    fn files_are_compared_by_what_their_lines_say() {
        assert!(same(b"a\n\n  b  \n", b"a\n  b\n\n\n"));
        assert!(same(b"a\r\nb\r\n", b"a\nb"));
        assert!(!same(b"a\n  b\n", b"a\nb\n"));
        assert!(!same(b"a\nb\n", b"b\na\n"));
        assert!(same(&[0xff, 0x00], &[0xff, 0x00]));
        assert!(!same(&[0xff, 0x00], &[0xff, 0x01]));
    }

    /// A changed file, a missing file and an extra file are each named, a file git keeps is not, and a copy differs in nothing.
    #[test]
    fn differing_files_are_named() {
        let folder = scratch("differing");
        let (expected, actual) = (folder.join("expected"), folder.join("actual"));
        put(&expected, "same.txt", "one\n");
        put(&expected, "src/changed.txt", "one\n");
        put(&expected, "missing.txt", "one\n");
        put(&actual, "same.txt", "one\n\n");
        put(&actual, "src/changed.txt", "two\n");
        put(&actual, "extra.txt", "one\n");
        put(&actual, ".git/HEAD", "ref\n");
        assert_eq!(
            differing(&expected, &actual).unwrap(),
            ["extra.txt", "missing.txt", "src/changed.txt"]
        );
        let copied = folder.join("copied");
        copy(&expected, &copied).unwrap();
        assert!(differing(&expected, &copied).unwrap().is_empty());
        assert!(identical(&expected, &copied));
        put(&copied, "same.txt", "one\n\n");
        assert!(differing(&expected, &copied).unwrap().is_empty());
        assert!(!identical(&expected, &copied));
    }

    /// Tasks are read by name, a filter keeps only those it names, and a name that is not there is refused.
    #[test]
    fn tasks_are_loaded_by_name() {
        let folder = scratch("loading");
        for name in ["beta", "alpha"] {
            put(&folder, &format!("{name}/prompt.md"), "Do it.\n");
            put(&folder, &format!("{name}/before/a.txt"), "one\n");
            put(&folder, &format!("{name}/after/a.txt"), "two\n");
        }
        let all = load(&folder, &[]).unwrap();
        assert_eq!(
            all.iter()
                .map(|task| task.name.as_str())
                .collect::<Vec<_>>(),
            ["alpha", "beta"]
        );
        assert_eq!(all[0].prompt, "Do it.");
        assert_eq!(load(&folder, &["beta".to_owned()]).unwrap().len(), 1);
        assert!(load(&folder, &["gamma".to_owned()]).is_err());
    }

    /// A task may start from a shared fixture and give what it should leave as a patch, which is applied to a copy.
    #[test]
    fn a_patch_makes_the_files_to_leave() {
        let folder = scratch("patched");
        put(&folder, "fixtures/base/a.txt", "one\ntwo\n");
        put(&folder, "tasks/swap/prompt.md", "Do it.\n");
        put(&folder, "tasks/swap/fixture", "base\n");
        put(
            &folder,
            "tasks/swap/change.patch",
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n one\n-two\n+three\n",
        );
        let mut tasks = load(&folder.join("tasks"), &[]).unwrap();
        tasks[0].settle(&folder.join("expected")).unwrap();
        let after = tasks[0].after.clone().unwrap();
        assert_eq!(
            std::fs::read_to_string(after.join("a.txt")).unwrap(),
            "one\nthree\n"
        );
        assert_eq!(differing(&after, &tasks[0].before).unwrap(), ["a.txt"]);
        put(
            &folder,
            "tasks/swap/change.patch",
            "--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n nine\n-two\n+three\n",
        );
        let mut broken = load(&folder.join("tasks"), &[]).unwrap();
        assert!(broken[0].settle(&folder.join("expected")).is_err());
    }

    /// A task may be scored by a command alone and name its own tools, and one with nothing to be scored on is refused.
    #[test]
    fn a_task_may_be_scored_by_a_command() {
        let folder = scratch("checked");
        put(&folder, "run/prompt.md", "Do it.\n");
        put(&folder, "run/before/a.txt", "one\n");
        assert!(load(&folder, &[]).is_err());
        put(&folder, "run/check", "test -f done.txt\n");
        put(&folder, "run/tools", "read, edit\nbash\n");
        let tasks = load(&folder, &[]).unwrap();
        assert_eq!(tasks[0].after, None);
        assert_eq!(tasks[0].check.as_deref(), Some("test -f done.txt"));
        let usual = ["read".to_owned()];
        assert_eq!(tasks[0].tools(&usual), ["read", "edit", "bash"]);
    }

    /// Every bundled task asks for a change: one scored on files has the same files in both folders, at least one of them different, and one scored by a command alone has a command.
    #[test]
    fn bundled_tasks_ask_for_a_change() {
        let mut tasks = load(&crate::bundled_tasks(), &[]).unwrap();
        assert!(!tasks.is_empty());
        let expected = scratch("bundled");
        for task in &mut tasks {
            task.settle(&expected).unwrap();
            let Some(after) = &task.after else {
                assert!(task.check.is_some(), "task {} is not scored", task.name);
                continue;
            };
            let (before, after_files) = (files(&task.before).unwrap(), files(after).unwrap());
            assert_eq!(
                before.keys().collect::<Vec<_>>(),
                after_files.keys().collect::<Vec<_>>(),
                "task {} adds or removes files",
                task.name
            );
            assert!(
                !differing(after, &task.before).unwrap().is_empty(),
                "task {} changes nothing",
                task.name
            );
        }
    }
}
