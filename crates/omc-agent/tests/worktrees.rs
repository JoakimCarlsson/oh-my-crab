//! Tests of subagents working in copies of their own, git worktrees checked out or copy-on-write clones, against real repositories and folders.
//!
//! They run in a test program of their own: the git processes they start would otherwise briefly hold the file locks of the conversation files other tests open and reopen. The tests of clones pass on a host whose file system cannot clone only by skipping, which they say they do.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_agent::{
    AgentEvent, ApprovalRequest, Approvals, Backend, Decision, EventSink, Isolation, LOCK_REASON,
    Merge, Prompter, Task, TaskSettings, prune_worktrees,
};
use omc_context::{AgentDefinition, Origin, builtin_agents};
use omc_providers::{ChatModel, ProviderError, Reply, Request, Sink};
use omc_tools::{Jobs, Registry, Spill, Tool, ToolError, ToolOutput};
use serde_json::{Value, json};
use testkit::{DenyAll, RequestSeen, Router, say, scratch};

#[allow(dead_code)]
#[path = "../src/testkit.rs"]
mod testkit;

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// Runs git in `directory` with `arguments` and returns what it printed, failing the test when git fails.
fn git(directory: &Path, arguments: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args([
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@localhost",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.autocrlf=false",
        ])
        .args(arguments)
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {arguments:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Folders of a test: the repository's own checkout, a linked worktree inside it where a session of an editor would run the agent, and the folders worktrees and patches go in, outside both.
struct Bench {
    /// The repository's own checkout.
    main: PathBuf,
    /// The linked worktree the main agent works in.
    linked: PathBuf,
    /// The folder the conversation's worktrees are made in.
    store: PathBuf,
    /// Where patches that were not applied are saved.
    patches: PathBuf,
}

/// Whether the file system of the scratch folders can make copy-on-write clones, tried with a file in a scratch folder called `name`; says on standard error that the test calling it is skipped when it cannot.
fn clones_here(name: &str) -> bool {
    let root = scratch(name);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("probe"), "probe\n").unwrap();
    let cloned = if cfg!(target_os = "macos") {
        Command::new("cp")
            .args(["-c", "probe", "probe-clone"])
            .current_dir(&root)
            .status()
            .is_ok_and(|status| status.success())
    } else if cfg!(target_os = "linux") {
        Command::new("cp")
            .args(["--reflink=always", "probe", "probe-clone"])
            .current_dir(&root)
            .status()
            .is_ok_and(|status| status.success())
    } else {
        false
    };
    if !cloned {
        eprintln!(
            "skipped: the file system of {} cannot make copy-on-write clones",
            root.display()
        );
    }
    cloned
}

/// A repository in a scratch folder called `name`, holding `a.txt` committed, with a linked worktree on a detached head under `.pandemonium/worktrees` in which `a.txt` has an uncommitted change and `c.txt` is untracked, and `ignored.log` is ignored.
fn bench(name: &str) -> Bench {
    let root = scratch(name);
    let main = root.join("main");
    std::fs::create_dir_all(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    std::fs::write(main.join("a.txt"), "alpha\n").unwrap();
    std::fs::write(main.join(".gitignore"), "*.log\n").unwrap();
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "start"]);
    std::fs::write(main.join(".git/info/exclude"), ".pandemonium/\n").unwrap();
    let linked = main.join(".pandemonium/worktrees/session");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            &linked.to_string_lossy(),
        ],
    );
    std::fs::write(linked.join("a.txt"), "alpha\nlocal\n").unwrap();
    std::fs::write(linked.join("c.txt"), "untracked words\n").unwrap();
    std::fs::write(linked.join("ignored.log"), "noise\n").unwrap();
    Bench {
        main,
        linked: linked.canonicalize().unwrap(),
        store: root.join("cache/worktrees/conversation"),
        patches: root.join("cache/output/conversation"),
    }
}

/// How subagents of a main agent working in `directory` get copies in `bench`'s store, made as `backend` says.
fn isolation(directory: &Path, bench: &Bench, backend: Backend) -> Arc<Isolation> {
    Arc::new(Isolation {
        directory: directory.to_path_buf(),
        store: bench.store.clone(),
        patches: Spill::new(&bench.patches),
        tools: Arc::new(|folder: &Path| Registry::builtin(folder)),
        backend,
    })
}

/// A `task` tool whose subagents, of the `agents` kinds, ask `model`, may use every built-in tool in `directory` without asking, get worktrees as `isolation` says, and have a prompt naming the folder they work in apart, when they do; with the log of its events.
fn task_tool(
    directory: &Path,
    model: Arc<dyn ChatModel>,
    agents: Vec<AgentDefinition>,
    isolation: Option<Arc<Isolation>>,
) -> (Task, Log) {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let task = Task::new(TaskSettings {
        models: omc_agent::Models::default(),
        nesting: omc_agent::Nesting::default(),
        model,
        registry: Arc::new(Registry::builtin(directory).unwrap()),
        approvals: Arc::new(Approvals::new(Arc::new(DenyAll), true)),
        events,
        agents,
        effort: None,
        prompt: Arc::new(|agent, _, apart, _| match apart {
            Some(folder) => format!("SUB:{} in {}", agent.name, folder.display()),
            None => format!("SUB:{}", agent.name),
        }),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation,
        hooks: None,
    });
    (task, log)
}

/// A `task` tool like [`task_tool`] that starts background runs in `jobs`.
fn task_tool_with(
    directory: &Path,
    model: Arc<dyn ChatModel>,
    isolation: Arc<Isolation>,
    jobs: Arc<Jobs>,
) -> Task {
    Task::new(TaskSettings {
        models: omc_agent::Models::default(),
        nesting: omc_agent::Nesting::default(),
        model,
        registry: Arc::new(Registry::builtin(directory).unwrap()),
        approvals: Arc::new(Approvals::new(Arc::new(DenyAll), true)),
        events: Arc::new(|_| {}),
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        hooks: None,
        jobs,
        isolation: Some(isolation),
    })
}

/// A reply calling each of `calls`, a tool name and its arguments, under ids of its own.
fn calls(calls: &[(&str, Value)]) -> omc_providers::Reply {
    omc_providers::Reply {
        tool_calls: calls
            .iter()
            .enumerate()
            .map(|(at, (name, arguments))| omc_providers::ToolCall {
                id: format!("s{at}"),
                name: (*name).to_owned(),
                arguments: arguments.clone(),
            })
            .collect(),
        ..omc_providers::Reply::default()
    }
}

/// The worktree events the log holds, wherever they are filed, as their id, folder and merge.
fn worktree_events(log: &Log) -> Vec<(String, PathBuf, Option<Merge>)> {
    /// The worktree event `event` is or holds, when it is or holds one.
    fn found(event: &AgentEvent) -> Option<(String, PathBuf, Option<Merge>)> {
        match event {
            AgentEvent::Worktree { id, path, merge } => {
                Some((id.clone(), path.clone(), merge.clone()))
            }
            AgentEvent::Nested { event, .. } => found(event),
            _ => None,
        }
    }
    log.lock().unwrap().iter().filter_map(found).collect()
}

/// The worktrees git lists for the repository at `main`, as its porcelain listing.
fn listed(main: &Path) -> String {
    git(main, &["worktree", "list", "--porcelain"])
}

/// An isolated subagent started from a linked worktree sees that worktree's uncommitted and untracked files but not its ignored ones, works in a folder outside it, and its edits come back to the linked worktree uncommitted, leaving the repository's own checkout alone and no worktree behind.
#[tokio::test]
async fn an_isolated_subagent_hands_its_edits_back_to_a_linked_worktree() {
    let bench = bench("isolated-linked");
    let model = Router::new(|seen: &RequestSeen| match seen.messages {
        1 => calls(&[
            ("read", json!({"path": "c.txt"})),
            ("glob", json!({"pattern": "*.log"})),
        ]),
        3 => calls(&[
            (
                "write",
                json!({"path": "a.txt", "content": "alpha\nlocal\nsub\n"}),
            ),
            ("write", json!({"path": "new.txt", "content": "made\n"})),
            (
                "write",
                json!({"path": "../escape.txt", "content": "out\n"}),
            ),
        ]),
        _ => say("edited"),
    });
    let (task, log) = task_tool(
        &bench.linked,
        model.clone(),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Worktree)),
    );
    let done = task
        .call_as("t1", json!({"task": "edit a.txt", "isolated": true}))
        .await
        .unwrap();
    assert!(
        done.text.starts_with("edited\n\n# Changes\n"),
        "{}",
        done.text
    );
    assert!(done.text.contains("- a.txt") && done.text.contains("- new.txt"));
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("a.txt")).unwrap(),
        "alpha\nlocal\nsub\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("new.txt")).unwrap(),
        "made\n"
    );
    assert!(!bench.main.join("new.txt").exists());
    assert_eq!(
        std::fs::read_to_string(bench.main.join("a.txt")).unwrap(),
        "alpha\n"
    );
    let status = git(&bench.linked, &["status", "--porcelain"]);
    assert!(status.contains(" M a.txt") && status.contains("?? new.txt"));
    assert!(!git(&bench.linked, &["diff", "--cached", "--name-only"]).contains("a.txt"));
    let events = worktree_events(&log);
    assert_eq!(events.len(), 2);
    let (id, path, merge) = &events[0];
    assert_eq!((id.as_str(), merge), ("t1", &None));
    assert!(path.starts_with(&bench.store) && !path.starts_with(&bench.linked));
    assert_eq!(
        events[1].2,
        Some(Merge::Applied(vec!["a.txt".into(), "new.txt".into()]))
    );
    let seen = model.seen.lock().unwrap();
    assert_eq!(seen[0].system, format!("SUB:task in {}", path.display()));
    assert!(told(&seen[0]).contains("worktree of your own"));
    drop(seen);
    let outputs: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Nested { event, .. } => match event.as_ref() {
                AgentEvent::ToolDone { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert!(outputs[0].contains("untracked words"));
    assert!(!outputs[1].contains("ignored.log"));
    assert!(outputs[4].contains("outside the working directory"));
    assert!(!bench.linked.parent().unwrap().join("escape.txt").exists());
    assert!(!path.exists());
    assert!(!listed(&bench.main).contains(LOCK_REASON));
}

/// The last user message a request carried, or nothing.
fn told(seen: &RequestSeen) -> &str {
    seen.last_user.as_deref().unwrap_or_default()
}

/// Siblings are applied in the order of the list: the first applies, and the second, which changed the same lines, is not applied at all, not even its file that does not conflict; its patch is saved and the result names the file that conflicts and where the patch is.
#[tokio::test]
async fn a_conflicting_sibling_is_kept_as_a_patch() {
    conflicting_siblings(&bench("isolated-conflict"), Backend::Worktree).await;
}

/// The same holds for clones in a repository.
#[tokio::test]
async fn a_conflicting_cloned_sibling_is_kept_as_a_patch() {
    if !clones_here("cloned-conflict-probe") {
        return;
    }
    conflicting_siblings(&bench("cloned-conflict"), Backend::Cow).await;
}

/// The same holds for clones of a folder outside any repository, whose changes are taken and checked file by file.
#[tokio::test]
async fn a_conflicting_sibling_outside_a_repository_is_kept_as_a_patch() {
    if !clones_here("plain-conflict-probe") {
        return;
    }
    conflicting_siblings(&plain_bench("plain-conflict"), Backend::Auto).await;
}

/// A folder in no git repository, in a scratch folder called `name`, holding `a.txt` as the linked worktree of [`bench`] has it, with folders for copies and patches beside it.
fn plain_bench(name: &str) -> Bench {
    let root = scratch(name);
    stop_git_above(root.parent().unwrap());
    let plain = root.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("a.txt"), "alpha\nlocal\n").unwrap();
    std::fs::write(plain.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(plain.join("ignored.log"), "noise\n").unwrap();
    Bench {
        main: plain.clone(),
        linked: plain.canonicalize().unwrap(),
        store: root.join("cache/worktrees/conversation"),
        patches: root.join("cache/output/conversation"),
    }
}

/// Runs two siblings in copies made as `backend` says from `bench`'s linked folder, the second of which conflicts with the first, and checks that only the first is applied.
async fn conflicting_siblings(bench: &Bench, backend: Backend) {
    let model = Router::new(|seen: &RequestSeen| {
        let second = told(seen).contains("second");
        match seen.messages {
            1 if second => calls(&[
                (
                    "write",
                    json!({"path": "a.txt", "content": "alpha\nlocal\nsecond\n"}),
                ),
                ("write", json!({"path": "other.txt", "content": "other\n"})),
            ]),
            1 => calls(&[(
                "write",
                json!({"path": "a.txt", "content": "alpha\nlocal\nfirst\n"}),
            )]),
            _ => say(if second { "second done" } else { "first done" }),
        }
    });
    let (task, log) = task_tool(
        &bench.linked,
        model,
        builtin_agents(),
        Some(isolation(&bench.linked, bench, backend)),
    );
    let done = task
        .call_as(
            "t1",
            json!({"isolated": true, "tasks": [{"task": "first"}, {"task": "second"}]}),
        )
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("a.txt")).unwrap(),
        "alpha\nlocal\nfirst\n"
    );
    assert!(!bench.linked.join("other.txt").exists());
    let merges: Vec<(String, Option<Merge>)> = worktree_events(&log)
        .into_iter()
        .filter(|(_, _, merge)| merge.is_some())
        .map(|(id, _, merge)| (id, merge))
        .collect();
    assert_eq!(
        merges[0],
        ("1".into(), Some(Merge::Applied(vec!["a.txt".into()])))
    );
    let Some(Merge::Conflicted { patch, files }) = &merges[1].1 else {
        panic!("the second sibling did not conflict: {merges:?}");
    };
    assert_eq!(merges[1].0, "2");
    assert_eq!(files, &vec!["a.txt".to_owned()]);
    assert!(patch.starts_with(&bench.patches));
    let saved = std::fs::read_to_string(patch).unwrap();
    assert!(saved.contains("+second") && saved.contains("other.txt"));
    assert!(done.text.contains("did NOT apply cleanly"));
    assert!(done.text.contains(&patch.display().to_string()));
    assert!(done.text.contains("first done") && done.text.contains("second done"));
    if bench.main.join(".git").exists() {
        assert!(!listed(&bench.main).contains(LOCK_REASON));
    }
}

/// Has git, run by this process, stop looking for a repository when it reaches `ceiling`, so that a scratch folder inside this project's own checkout counts as being in none; repositories inside `ceiling` are found as before.
fn stop_git_above(ceiling: &Path) {
    unsafe { std::env::set_var("GIT_CEILING_DIRECTORIES", ceiling) };
}

/// Asking for isolation outside a git repository starts nothing and says why, and so does asking where isolation is not available; without asking, a kind whose definition works isolated is refused too, unless the call says otherwise.
#[tokio::test]
async fn isolation_outside_a_repository_is_refused() {
    let plain = scratch("isolated-plain");
    stop_git_above(plain.parent().unwrap());
    let bench = Bench {
        main: plain.clone(),
        linked: plain.clone(),
        store: plain.join("../isolated-plain-store/conversation"),
        patches: plain.join("../isolated-plain-store/output"),
    };
    let model = Router::new(|_: &RequestSeen| say("worked"));
    let mut agents = builtin_agents();
    agents.push(AgentDefinition {
        name: "fixer".into(),
        description: "fixes".into(),
        tools: None,
        prompt: "Fix.".into(),
        output_schema: None,
        isolated: true,
        model: None,
        path: None,
        origin: Origin::User,
    });
    let (task, log) = task_tool(
        &plain,
        model.clone(),
        agents.clone(),
        Some(isolation(&plain, &bench, Backend::Worktree)),
    );
    let refused = task
        .call_as("t1", json!({"task": "x", "isolated": true}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("is not in a git working tree"),
        "{refused}"
    );
    let by_definition = task
        .call_as("t2", json!({"task": "x", "agent": "fixer"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(by_definition.contains("is not in a git working tree"));
    assert!(model.seen.lock().unwrap().is_empty());
    assert!(log.lock().unwrap().is_empty());
    let shared = task
        .call_as(
            "t3",
            json!({"task": "x", "agent": "fixer", "isolated": false}),
        )
        .await
        .unwrap();
    assert!(shared.text.starts_with("worked\n\n(Subagent kept as "));
    let (unavailable, _) = task_tool(&plain, model, agents, None);
    let refused = unavailable
        .call_as("t4", json!({"task": "x", "isolated": true}))
        .await
        .unwrap_err()
        .to_string();
    assert!(refused.contains("cannot work in worktrees of their own here"));
}

/// A model whose subagents never answer, so that only stopping them ends them.
struct Stuck;

#[async_trait]
impl ChatModel for Stuck {
    /// Never answers.
    async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
        std::future::pending().await
    }
}

/// Starts an isolated subagent of `task`, which never ends, and returns the call once the worktree it works in is made, with that worktree's folder.
async fn started(
    task: &Task,
    log: &Log,
) -> (
    std::pin::Pin<Box<impl Future<Output = Result<ToolOutput, ToolError>>>>,
    PathBuf,
) {
    let mut call = Box::pin(task.call_as("t1", json!({"task": "x", "isolated": true})));
    let made = async {
        loop {
            if let Some((_, path, _)) = worktree_events(log).pop() {
                return path;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        _ = &mut call => panic!("the stuck subagent ended"),
        path = made => (call, path),
    }
}

/// A worktree left behind by a run that crashed is removed, and forgotten by git, when its conversation opens again, and only then.
#[tokio::test]
async fn worktrees_left_behind_are_pruned() {
    let bench = bench("isolated-prune");
    let (task, log) = task_tool(
        &bench.linked,
        Arc::new(Stuck),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Worktree)),
    );
    let (call, folder) = started(&task, &log).await;
    std::mem::forget(call);
    assert!(folder.join("a.txt").exists());
    assert!(listed(&bench.main).contains(LOCK_REASON));
    let root = bench.store.parent().unwrap();
    prune_worktrees(root, Some("other"), Duration::from_secs(3600));
    assert!(folder.exists());
    prune_worktrees(root, Some("conversation"), Duration::from_secs(3600));
    assert!(!folder.exists());
    assert!(!bench.store.exists());
    assert!(!listed(&bench.main).contains(LOCK_REASON));
}

/// A subagent stopped while it works has its worktree removed.
#[tokio::test]
async fn a_stopped_subagent_has_its_worktree_removed() {
    let bench = bench("isolated-stopped");
    let (task, log) = task_tool(
        &bench.linked,
        Arc::new(Stuck),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Worktree)),
    );
    let (call, folder) = started(&task, &log).await;
    drop(call);
    for _ in 0..400 {
        if !folder.exists() && !listed(&bench.main).contains(LOCK_REASON) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the worktree at {} was not removed", folder.display());
}

/// A subagent that fails has its changes saved but not applied.
#[tokio::test]
async fn a_failed_subagent_has_its_changes_held() {
    let bench = bench("isolated-failed");
    let model = Router::new(|seen: &RequestSeen| match seen.messages {
        1 => calls(&[("write", json!({"path": "new.txt", "content": "half\n"}))]),
        _ => say("gave up"),
    });
    let (task, log) = task_tool(
        &bench.linked,
        model,
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Worktree)),
    );
    let schema = json!({"type": "object", "required": ["n"]});
    let failed = task
        .call_as(
            "t1",
            json!({"task": "x", "isolated": true, "output_schema": schema}),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(failed.contains("Its changes were not applied: the subagent did not finish"));
    assert!(!bench.linked.join("new.txt").exists());
    let merge = worktree_events(&log).pop().and_then(|(_, _, merge)| merge);
    let Some(Merge::Held {
        patch: Some(patch), ..
    }) = merge
    else {
        panic!("not held: {merge:?}");
    };
    assert!(std::fs::read_to_string(patch).unwrap().contains("+half"));
}

/// A model whose subagents write `new.txt` and then never answer again, so that only stopping them ends them.
struct WritesThenHangs;

#[async_trait]
impl ChatModel for WritesThenHangs {
    /// Writes `new.txt` on the first request and never answers the ones after it.
    async fn complete(
        &self,
        request: Request<'_>,
        _: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        if request.messages.len() == 1 {
            return Ok(calls(&[(
                "write",
                json!({"path": "new.txt", "content": "half\n"}),
            )]));
        }
        std::future::pending().await
    }
}

/// An isolated subagent the reader stops has its changes held, not applied, and its result says the reader stopped it.
#[tokio::test]
async fn a_subagent_the_reader_stops_has_its_changes_held() {
    let bench = bench("isolated-reader-stopped");
    let roster = Arc::new(omc_agent::Roster::default());
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let task = Task::new(TaskSettings {
        models: omc_agent::Models::default(),
        nesting: omc_agent::Nesting::default().with_roster(roster.clone()),
        model: Arc::new(WritesThenHangs),
        registry: Arc::new(Registry::builtin(&bench.linked).unwrap()),
        approvals: Arc::new(Approvals::new(Arc::new(DenyAll), true)),
        events: Arc::new(move |event| sink.lock().unwrap().push(event)),
        agents: builtin_agents(),
        effort: None,
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: Some(isolation(&bench.linked, &bench, Backend::Worktree)),
        hooks: None,
    });
    let call = task.call_as("t1", json!({"task": "x", "isolated": true}));
    let stop = async {
        loop {
            if let Some((_, path, _)) = worktree_events(&log).pop()
                && path.join("new.txt").exists()
            {
                assert!(roster.stop("t1"));
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let (outcome, ()) =
        tokio::time::timeout(Duration::from_secs(30), async { tokio::join!(call, stop) })
            .await
            .unwrap();
    let stopped = outcome.unwrap_err().to_string();
    assert!(stopped.contains("stopped by the reader"));
    assert!(stopped.contains("Its changes were not applied"));
    assert!(!bench.linked.join("new.txt").exists());
    let merge = worktree_events(&log).pop().and_then(|(_, _, merge)| merge);
    assert!(
        matches!(merge, Some(Merge::Held { .. })),
        "not held: {merge:?}"
    );
}

/// An isolated subagent started in the background, asked for a structured result, hands back its checked JSON through `wait`, followed by the files it changed, which are applied by then.
#[tokio::test]
async fn a_background_subagent_hands_back_typed_results_and_edits() {
    let bench = bench("isolated-typed");
    let model = Router::new(|seen: &RequestSeen| match seen.messages {
        1 => calls(&[("write", json!({"path": "new.txt", "content": "made\n"}))]),
        3 => calls(&[("submit_result", json!({"result": {"count": 1}}))]),
        _ => say("submitted"),
    });
    let jobs = Arc::new(Jobs::new(|_| {}));
    let task = task_tool_with(
        &bench.linked,
        model,
        isolation(&bench.linked, &bench, Backend::Worktree),
        jobs.clone(),
    );
    let schema = json!({
        "type": "object",
        "properties": { "count": { "type": "integer" } },
        "required": ["count"]
    });
    let started = task
        .call_as(
            "t1",
            json!({"task": "x", "isolated": true, "background": true, "output_schema": schema}),
        )
        .await
        .unwrap();
    assert!(started.text.contains("bg-1"));
    let finished = jobs.wait().await.unwrap();
    assert!(finished.ok, "{}", finished.text);
    let (typed, changes) = finished.text.split_once("\n\n# Changes\n").unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(typed).unwrap(),
        json!({"count": 1})
    );
    assert!(changes.contains("- new.txt"));
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("new.txt")).unwrap(),
        "made\n"
    );
    assert!(!listed(&bench.main).contains(LOCK_REASON));
}

/// An isolated subagent's own isolated subagent starts from its parent's worktree, sees what the parent changed there, and hands its edits back to that worktree; the parent then hands both its own edits and its child's back to the main agent's working tree.
#[tokio::test]
async fn a_nested_isolated_subagent_merges_into_its_parents_worktree() {
    nested("isolated-nested", Backend::Worktree).await;
}

/// The same holds for clones: the child's clone is made from its parent's clone.
#[tokio::test]
async fn a_nested_cloned_subagent_merges_into_its_parents_clone() {
    if !clones_here("cloned-nested-probe") {
        return;
    }
    nested("cloned-nested", Backend::Cow).await;
}

/// A subagent in a copy made as `backend` says, in a bench called `name`, starts an isolated subagent, whose change lands in its parent's copy and then, with the parent's own, in the main agent's working tree.
async fn nested(name: &str, backend: Backend) {
    let bench = bench(name);
    let model = Router::new(|seen: &RequestSeen| {
        let child = told(seen).contains("child work");
        match (child, seen.messages) {
            (false, 1) => calls(&[("write", json!({"path": "parent.txt", "content": "p\n"}))]),
            (false, 3) => calls(&[("task", json!({"task": "child work", "isolated": true}))]),
            (false, _) => say("parent done"),
            (true, 1) => calls(&[
                ("read", json!({"path": "parent.txt"})),
                ("write", json!({"path": "child.txt", "content": "c\n"})),
            ]),
            (true, _) => say("child done"),
        }
    });
    let (task, log) = task_tool(
        &bench.linked,
        model.clone(),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, backend)),
    );
    let done = task
        .call_as("t1", json!({"task": "parent job", "isolated": true}))
        .await
        .unwrap();
    assert!(done.text.starts_with("parent done"), "{}", done.text);
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("parent.txt")).unwrap(),
        "p\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("child.txt")).unwrap(),
        "c\n"
    );
    assert!(!bench.main.join("child.txt").exists());
    let events = worktree_events(&log);
    assert_eq!(events.len(), 4, "{events:?}");
    let parent_path = &events[0].1;
    let child_path = &events[1].1;
    assert!(child_path.starts_with(&bench.store) && child_path != parent_path);
    assert_eq!(events[2].2, Some(Merge::Applied(vec!["child.txt".into()])));
    assert_eq!(
        events[3].2,
        Some(Merge::Applied(vec![
            "child.txt".into(),
            "parent.txt".into()
        ]))
    );
    let read_by_child = log
        .lock()
        .unwrap()
        .iter()
        .find_map(|event| match event {
            AgentEvent::Nested { event, .. } => match event.as_ref() {
                AgentEvent::Nested { event, .. } => match event.as_ref() {
                    AgentEvent::ToolDone { name, text, .. } if name == "read" => Some(text.clone()),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .unwrap();
    assert!(read_by_child.contains('p'), "{read_by_child}");
    assert!(!listed(&bench.main).contains(LOCK_REASON));
}

/// The text of every tool call the log says a subagent's tools answered, in order.
fn tool_outputs(log: &Log) -> Vec<String> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Nested { event, .. } => match event.as_ref() {
                AgentEvent::ToolDone { text, .. } => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// A subagent in a clone of a linked worktree sees its uncommitted, untracked and ignored files, works outside it, and its edits come back uncommitted, while what it changed in an ignored file does not; the clone is gone afterwards and git lists no worktree of it.
#[tokio::test]
async fn a_cloned_subagent_sees_ignored_files_and_hands_its_edits_back() {
    if !clones_here("cloned-linked-probe") {
        return;
    }
    let bench = bench("cloned-linked");
    let model = Router::new(|seen: &RequestSeen| match seen.messages {
        1 => calls(&[
            ("read", json!({"path": "c.txt"})),
            ("read", json!({"path": "ignored.log"})),
        ]),
        3 => calls(&[
            (
                "write",
                json!({"path": "a.txt", "content": "alpha\nlocal\nsub\n"}),
            ),
            ("write", json!({"path": "new.txt", "content": "made\n"})),
            (
                "write",
                json!({"path": "ignored.log", "content": "changed\n"}),
            ),
        ]),
        _ => say("edited"),
    });
    let (task, log) = task_tool(
        &bench.linked,
        model.clone(),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Cow)),
    );
    let done = task
        .call_as("t1", json!({"task": "edit a.txt", "isolated": true}))
        .await
        .unwrap();
    assert!(
        done.text.starts_with("edited\n\n# Changes\n"),
        "{}",
        done.text
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("a.txt")).unwrap(),
        "alpha\nlocal\nsub\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("new.txt")).unwrap(),
        "made\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("ignored.log")).unwrap(),
        "noise\n"
    );
    assert!(!bench.main.join("new.txt").exists());
    let status = git(&bench.linked, &["status", "--porcelain"]);
    assert!(status.contains(" M a.txt") && status.contains("?? new.txt"));
    let events = worktree_events(&log);
    assert_eq!(events.len(), 2);
    let path = &events[0].1;
    assert!(path.starts_with(&bench.store) && !path.starts_with(&bench.linked));
    assert_eq!(
        events[1].2,
        Some(Merge::Applied(vec!["a.txt".into(), "new.txt".into()]))
    );
    let seen = model.seen.lock().unwrap();
    assert!(told(&seen[0]).contains("copy-on-write clone"));
    drop(seen);
    let outputs = tool_outputs(&log);
    let read = |words: &str| outputs.iter().any(|output| output.contains(words));
    assert!(read("untracked words"), "{outputs:?}");
    assert!(read("noise"), "{outputs:?}");
    assert!(!path.exists());
    assert!(!listed(&bench.main).contains(LOCK_REASON));
}

/// A subagent in a clone of a folder outside any repository has its changed, added and removed files applied to the folder, its ignored ones left alone.
#[tokio::test]
async fn a_cloned_subagent_outside_a_repository_hands_its_edits_back() {
    if !clones_here("plain-linked-probe") {
        return;
    }
    let bench = plain_bench("plain-linked");
    std::fs::write(bench.linked.join("gone.txt"), "bye\n").unwrap();
    let model = Router::new(|seen: &RequestSeen| match seen.messages {
        1 => calls(&[
            ("read", json!({"path": "ignored.log"})),
            (
                "write",
                json!({"path": "a.txt", "content": "alpha\nlocal\nsub\n"}),
            ),
            (
                "write",
                json!({"path": "deep/new.txt", "content": "made\n"}),
            ),
            (
                "write",
                json!({"path": "ignored.log", "content": "changed\n"}),
            ),
            ("bash", json!({"command": "rm gone.txt"})),
        ]),
        _ => say("edited"),
    });
    let (task, log) = task_tool(
        &bench.linked,
        model.clone(),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Auto)),
    );
    let done = task
        .call_as("t1", json!({"task": "edit a.txt", "isolated": true}))
        .await
        .unwrap();
    assert!(done.text.contains("Files changed"), "{}", done.text);
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("a.txt")).unwrap(),
        "alpha\nlocal\nsub\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("deep/new.txt")).unwrap(),
        "made\n"
    );
    assert_eq!(
        std::fs::read_to_string(bench.linked.join("ignored.log")).unwrap(),
        "noise\n"
    );
    assert!(!bench.linked.join("gone.txt").exists());
    let events = worktree_events(&log);
    assert_eq!(
        events[1].2,
        Some(Merge::Applied(vec![
            "a.txt".into(),
            "deep/new.txt".into(),
            "gone.txt".into()
        ]))
    );
    assert!(!events[0].1.exists());
    assert!(tool_outputs(&log)[0].contains("noise"));
}

/// Outside a repository, `worktree` refuses isolation as before, while `cow` clones.
#[tokio::test]
async fn outside_a_repository_only_clones_isolate() {
    if !clones_here("plain-backends-probe") {
        return;
    }
    let bench = plain_bench("plain-backends");
    let model = Router::new(|_: &RequestSeen| say("worked"));
    let (task, _) = task_tool(
        &bench.linked,
        model.clone(),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Worktree)),
    );
    let refused = task
        .call_as("t1", json!({"task": "x", "isolated": true}))
        .await
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("is not in a git working tree"),
        "{refused}"
    );
    let (task, _) = task_tool(
        &bench.linked,
        model,
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Cow)),
    );
    let done = task
        .call_as("t2", json!({"task": "x", "isolated": true}))
        .await
        .unwrap();
    assert!(done.text.contains("It changed no files."), "{}", done.text);
}

/// A clone left behind by a run that crashed is removed, and forgotten by git, when its conversation opens again; one whose subagent is stopped is removed at once.
#[tokio::test]
async fn clones_left_behind_are_pruned_and_stopped_ones_removed() {
    if !clones_here("cloned-prune-probe") {
        return;
    }
    let bench = bench("cloned-prune");
    let (task, log) = task_tool(
        &bench.linked,
        Arc::new(Stuck),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Cow)),
    );
    let (call, folder) = started(&task, &log).await;
    std::mem::forget(call);
    assert!(folder.join("ignored.log").exists());
    assert!(listed(&bench.main).contains(LOCK_REASON));
    let root = bench.store.parent().unwrap();
    prune_worktrees(root, Some("conversation"), Duration::from_secs(3600));
    assert!(!folder.exists());
    assert!(!listed(&bench.main).contains(LOCK_REASON));

    let (task, log) = task_tool(
        &bench.linked,
        Arc::new(Stuck),
        builtin_agents(),
        Some(isolation(&bench.linked, &bench, Backend::Cow)),
    );
    let (call, folder) = started(&task, &log).await;
    drop(call);
    for _ in 0..400 {
        if !folder.exists() && !listed(&bench.main).contains(LOCK_REASON) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the clone at {} was not removed", folder.display());
}
