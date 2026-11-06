//! Runs against real debug adapters: a small C program that crashes on a null pointer is debugged with `lldb-dap` and with `gdb -i dap`. Each run is skipped when its adapter or `gcc` is not installed.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use omc_dap::{DebugTool, Sessions};
use omc_tools::{Sandbox, Tool};
use serde_json::{Value, json};

/// The program: it walks a list of three nodes four times, so the fourth step reads through a null pointer on line 7.
const PROGRAM: &str = "#include <stdio.h>\n#include <stddef.h>\nstruct node { int value; struct node *next; };\nstatic int total(struct node *n, int count) {\n    int sum = 0;\n    for (int i = 0; i <= count; i++) {\n        sum += n->value;\n        n = n->next;\n    }\n    return sum;\n}\nint main(void) {\n    struct node c = {3, NULL};\n    struct node b = {2, &c};\n    struct node a = {1, &b};\n    printf(\"start\\n\");\n    fflush(stdout);\n    printf(\"%d\\n\", total(&a, 3));\n    return 0;\n}\n";

/// Whether `program` is found in a folder of the `PATH`.
fn installed(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|folders| {
        std::env::split_paths(&folders).any(|folder| folder.join(program).is_file())
    })
}

/// A program that never ends: it spins in a loop on line 4.
const SPINNER: &str =
    "int main(void) {\n    volatile unsigned long n = 0;\n    for (;;) {\n        n++;\n    }\n}\n";

/// Builds the program with debug information in a folder of its own under the workspace's git-ignored `build/`, and returns the folder; nothing when it does not build.
fn build(name: &str) -> Option<PathBuf> {
    build_source(name, PROGRAM)
}

/// Builds `source` as `crash.c` with debug information in a folder of its own under the workspace's git-ignored `build/`, and returns the folder; nothing when it does not build.
fn build_source(name: &str, source: &str) -> Option<PathBuf> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-dap")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).ok()?;
    let dir = dir.canonicalize().ok()?;
    std::fs::write(dir.join("crash.c"), source).ok()?;
    let built = std::process::Command::new("gcc")
        .args(["-g", "-O0", "-o", "crash", "crash.c"])
        .current_dir(&dir)
        .status()
        .ok()?;
    built.success().then_some(dir)
}

/// Calls `tool` with `args` and returns its text, failing the test with the reason when the call fails.
async fn call(tool: &DebugTool, args: Value) -> String {
    match tool.call(args.clone()).await {
        Ok(output) => output.text,
        Err(error) => panic!("{args} failed: {error}"),
    }
}

/// Debugs the program in `dir` with `adapter`: stops at a breakpoint, reads variables and an expression, runs into the crash and reads where it happened, then ends the session.
async fn debug_the_crash(adapter: &str, dir: &Path) {
    let sessions = Arc::new(Sessions::new());
    let tool = DebugTool::new(Sandbox::new(dir).unwrap(), sessions.clone());
    let started = call(
        &tool,
        json!({"action": "launch", "program": "crash", "adapter": adapter, "breakpoints": ["crash.c:7"], "timeout": 30}),
    )
    .await;
    assert!(started.contains("Stopped: breakpoint"), "{started}");
    assert!(started.contains("total at crash.c:7"), "{started}");
    assert!(
        started.contains(">  7 |") || started.contains("> 7 |"),
        "{started}"
    );
    let variables = call(&tool, json!({"action": "variables", "depth": 1})).await;
    assert!(variables.contains("count"), "{variables}");
    assert!(variables.contains("sum"), "{variables}");
    let value = call(
        &tool,
        json!({"action": "evaluate", "expression": "n->value"}),
    )
    .await;
    assert!(value.contains("= 1"), "{value}");
    let cleared = call(&tool, json!({"action": "clear", "all": true})).await;
    assert!(!cleared.contains("crash.c:7"), "{cleared}");
    let crashed = call(&tool, json!({"action": "continue", "timeout": 30})).await;
    assert!(crashed.contains("SIGSEGV"), "{crashed}");
    assert!(crashed.contains("crash.c:7"), "{crashed}");
    let index = call(&tool, json!({"action": "evaluate", "expression": "i"})).await;
    assert!(index.contains("= 3"), "{index}");
    let output = call(&tool, json!({"action": "output", "all": true})).await;
    assert!(output.contains("start"), "{output}");
    let ended = call(&tool, json!({"action": "disconnect"})).await;
    assert!(ended.contains("ended"), "{ended}");
    assert!(sessions.is_empty());
}

/// `lldb-dap` finds the null pointer.
#[tokio::test(flavor = "multi_thread")]
async fn lldb_dap_finds_the_crash() {
    if !installed("lldb-dap") || !installed("gcc") {
        eprintln!("skipped: lldb-dap or gcc is not installed");
        return;
    }
    let dir = build("real-lldb").expect("the program builds");
    debug_the_crash("lldb-dap", &dir).await;
}

/// `gdb -i dap` finds the null pointer.
#[tokio::test(flavor = "multi_thread")]
async fn gdb_finds_the_crash() {
    if !installed("gdb") || !installed("gcc") {
        eprintln!("skipped: gdb or gcc is not installed");
        return;
    }
    let dir = build("real-gdb").expect("the program builds");
    debug_the_crash("gdb", &dir).await;
}

/// A program that never stops is reported as still running instead of holding the call, and pausing it stops it in its loop.
#[tokio::test(flavor = "multi_thread")]
async fn a_running_program_times_out_and_pauses() {
    if !installed("lldb-dap") || !installed("gcc") {
        eprintln!("skipped: lldb-dap or gcc is not installed");
        return;
    }
    let dir = build_source("real-spin", SPINNER).expect("the program builds");
    let tool = DebugTool::new(Sandbox::new(&dir).unwrap(), Arc::new(Sessions::new()));
    let started = call(
        &tool,
        json!({"action": "launch", "program": "crash", "timeout": 1}),
    )
    .await;
    assert!(started.contains("Still running after 1s"), "{started}");
    let waited = call(&tool, json!({"action": "wait", "timeout": 0.5})).await;
    assert!(waited.contains("Still running"), "{waited}");
    let paused = call(&tool, json!({"action": "pause", "timeout": 10})).await;
    assert!(paused.contains("Stopped:"), "{paused}");
    assert!(paused.contains("main at crash.c:"), "{paused}");
    let threads = call(&tool, json!({"action": "threads"})).await;
    assert!(threads.contains("* "), "{threads}");
    let ended = call(&tool, json!({"action": "disconnect"})).await;
    assert!(ended.contains("the program was stopped"), "{ended}");
}

/// Whether a process running the program at `program` is alive, as `/proc` shows.
#[cfg(target_os = "linux")]
fn running(program: &Path) -> bool {
    std::fs::read_dir("/proc").is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            std::fs::read_link(entry.path().join("exe")).is_ok_and(|exe| exe == program)
        })
    })
}

/// Stopping every session of a conversation kills the programs it launched, even ones that were never disconnected.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread")]
async fn stopping_the_sessions_kills_what_they_launched() {
    if !installed("lldb-dap") || !installed("gcc") {
        eprintln!("skipped: lldb-dap or gcc is not installed");
        return;
    }
    let dir = build_source("real-stop", SPINNER).expect("the program builds");
    let sessions = Arc::new(Sessions::new());
    let tool = DebugTool::new(Sandbox::new(&dir).unwrap(), sessions.clone());
    call(
        &tool,
        json!({"action": "launch", "program": "crash", "timeout": 0.5}),
    )
    .await;
    let program = dir.join("crash");
    assert!(running(&program));
    sessions.stop_all();
    let mut gone = false;
    for _ in 0..50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if !running(&program) {
            gone = true;
            break;
        }
    }
    assert!(gone, "the program outlived its session");
}
