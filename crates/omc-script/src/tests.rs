//! Tests for the `script` tool and its engine.

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::time::Instant;

use super::*;

/// Creates an empty scratch directory under the workspace's `build/`.
fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-script")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Counts how many times it was dropped.
struct Dropped(Arc<AtomicUsize>);

impl Drop for Dropped {
    /// Counts the drop.
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

/// A caller with a few made-up tools: `echo` returns its `text`, `data` returns `{ n }` as data, `fail` fails, `slow` waits for `seconds`; it records every call it is asked to make.
#[derive(Default)]
struct Fake {
    /// The calls asked for, by name, in order.
    calls: Mutex<Vec<String>>,
    /// How many `slow` calls were dropped before they ended.
    abandoned: Arc<AtomicUsize>,
    /// What [`Caller::waited`] reports.
    waited: Duration,
}

#[async_trait]
impl Caller for Fake {
    /// Has the four made-up tools.
    fn has(&self, name: &str) -> bool {
        ["echo", "data", "fail", "slow"].contains(&name)
    }

    /// Runs the made-up tool `name`.
    async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, String> {
        self.calls.lock().unwrap().push(name.to_owned());
        match name {
            "echo" => Ok(ToolOutput::new(args["text"].as_str().unwrap_or_default())),
            "data" => Ok(ToolOutput::new("n").with_data(json!({ "n": args["n"] }))),
            "fail" => Err("it broke".to_owned()),
            "slow" => {
                let guard = Dropped(self.abandoned.clone());
                let seconds = args["seconds"].as_f64().unwrap_or(10.0);
                tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                std::mem::forget(guard);
                Ok(ToolOutput::new("slept"))
            }
            other => Err(format!("unknown tool: {other}")),
        }
    }

    /// Reports the time set up front.
    fn waited(&self) -> Duration {
        self.waited
    }
}

/// A script tool that may call the made-up tools of [`Fake`].
fn tool() -> Script {
    Script {
        spill: Spill::disabled(),
        exposed: ["data", "echo", "fail", "slow"].map(str::to_owned).to_vec(),
        read_only: true,
        description: String::new(),
    }
}

/// Runs `code` with a timeout of `seconds` through `caller`.
async fn run_with(caller: &Fake, code: &str, seconds: u64) -> Result<ToolOutput, ToolError> {
    tool()
        .call_through("s1", json!({ "code": code, "timeout": seconds }), caller)
        .await
}

/// Runs `code` with a fresh [`Fake`] and returns its text, or panics with its error.
async fn text_of(code: &str) -> String {
    run_with(&Fake::default(), code, 10)
        .await
        .unwrap_or_else(|error| panic!("{error}"))
        .text
}

/// Runs `code` with a fresh [`Fake`] and returns its error, or panics when it succeeds.
async fn error_of(code: &str, seconds: u64) -> String {
    match run_with(&Fake::default(), code, seconds).await {
        Ok(output) => panic!("expected a failure, got {}", output.text),
        Err(error) => error.to_string(),
    }
}

/// Printed lines and the returned value both come back, the value as indented JSON.
#[tokio::test]
async fn prints_and_returns() {
    let text = text_of("print('hi', 2, [1]); console.log({a: 1}); return { ok: true };").await;
    assert_eq!(
        text,
        "hi 2 [\n  1\n]\n{\n  \"a\": 1\n}\n\nReturned:\n{\n  \"ok\": true\n}"
    );
    assert_eq!(text_of("return 'plain'").await, "plain");
    assert_eq!(
        text_of("return { m: new Map([['a', 1]]), s: new Set([2]) }").await,
        "{\n  \"m\": {\n    \"a\": 1\n  },\n  \"s\": [\n    2\n  ]\n}"
    );
    assert_eq!(
        text_of("1 + 1;").await,
        "The script finished without printing or returning anything."
    );
}

/// A script reaches nothing but its tools: no module loader, network, process, timers or engine extras.
#[tokio::test]
async fn has_no_ambient_capabilities() {
    let text = text_of(
        "const names = ['require', 'fetch', 'process', 'setTimeout', 'setInterval', 'std', 'os', 'Deno', 'Bun', 'XMLHttpRequest', 'WebSocket', 'WebAssembly', 'module', 'exports', '__host', 'host'];
         const present = names.filter((name) => typeof globalThis[name] !== 'undefined');
         let imported;
         try { await import('fs'); imported = 'imported'; } catch (error) { imported = 'refused'; }
         return { present, imported };",
    )
    .await;
    assert_eq!(
        text,
        "{\n  \"present\": [],\n  \"imported\": \"refused\"\n}"
    );
}

/// The tools and printing cannot be replaced by the script.
#[tokio::test]
async fn the_tools_cannot_be_replaced() {
    let text = text_of(
        "try { tools = null; } catch {} try { tools.echo = null; } catch {}
         return await tools.echo({ text: 'still here' });",
    )
    .await;
    assert_eq!(text, "still here");
}

/// A tool's text comes back as a string, its data as a value, and several calls can run at once.
#[tokio::test]
async fn tool_results_come_back_as_text_or_data() {
    let caller = Fake::default();
    let output = run_with(
        &caller,
        "const [a, b] = await Promise.all([tools.echo({ text: 'x' }), tools.data({ n: 3 })]);
         return [typeof a, a, b.n];",
        10,
    )
    .await
    .unwrap();
    assert_eq!(output.text, "[\n  \"string\",\n  \"x\",\n  3\n]");
    assert_eq!(*caller.calls.lock().unwrap(), ["echo", "data"]);
}

/// A tool that fails rejects with a `ToolError` the script can catch, naming the tool.
#[tokio::test]
async fn failing_tools_throw_catchable_errors() {
    let text = text_of(
        "try { await tools.fail({}); } catch (error) {
           return [error instanceof ToolError, error.name, error.tool, error.message];
         }",
    )
    .await;
    assert_eq!(
        text,
        "[\n  true,\n  \"ToolError\",\n  \"fail\",\n  \"it broke\"\n]"
    );
}

/// An uncaught failure fails the script, with what it printed before.
#[tokio::test]
async fn uncaught_failures_fail_the_script() {
    let error = error_of("print('before'); await tools.fail({});", 10).await;
    assert!(
        error.starts_with("the script failed after 1 tool call: ToolError: it broke"),
        "{error}"
    );
    assert!(
        error.ends_with("Printed before it failed:\nbefore"),
        "{error}"
    );
    let error = error_of("const x = ;", 10).await;
    assert!(error.contains("SyntaxError"), "{error}");
    let error = error_of("null.x;", 10).await;
    assert!(error.contains("TypeError"), "{error}");
}

/// A tool scripts do not have rejects, listing the tools they do have, and arguments that are not an object are refused before the call.
#[tokio::test]
async fn unknown_tools_and_bad_arguments_are_refused() {
    let text = text_of(
        "const seen = [];
         try { await tools.bash({ command: 'ls' }); } catch (error) { seen.push(error.message); }
         try { await tools.echo('text'); } catch (error) { seen.push(error.message); }
         return seen.join('\\n');",
    )
    .await;
    assert_eq!(
        text,
        "there is no tool called bash in scripts; the tools are: data, echo, fail, slow\ntools.echo takes one object of arguments"
    );
}

/// The tools a caller lacks are not offered to the script, even when the tool was fitted to them.
#[tokio::test]
async fn only_tools_the_caller_has_are_offered() {
    let mut script = tool();
    script.exposed.push("write".to_owned());
    let output = script
        .call_through(
            "s1",
            json!({ "code": "return Object.keys(tools).join(',')" }),
            &Fake::default(),
        )
        .await
        .unwrap();
    assert_eq!(output.text, "data,echo,fail,slow");
}

/// A loop that never yields is stopped at the time limit, and the time limit cannot be caught.
#[tokio::test]
async fn busy_loops_hit_the_time_limit() {
    let started = Instant::now();
    let error = error_of("while (true) { try { for (;;) {} } catch {} }", 1).await;
    assert!(error.contains("time limit of 1 s"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// A script waiting on a tool call is stopped at the time limit, and the call is dropped.
#[tokio::test]
async fn waiting_scripts_hit_the_time_limit() {
    let caller = Fake::default();
    let started = Instant::now();
    let error = run_with(&caller, "await tools.slow({ seconds: 30 });", 1)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("time limit of 1 s"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(caller.abandoned.load(Ordering::SeqCst), 1);
}

/// Time spent waiting for the reader to approve calls is left out of the time limit.
#[tokio::test]
async fn time_waiting_for_the_reader_is_not_counted() {
    let caller = Fake {
        waited: Duration::from_secs(10),
        ..Fake::default()
    };
    let output = run_with(&caller, "return await tools.slow({ seconds: 1.5 });", 1)
        .await
        .unwrap();
    assert_eq!(output.text, "slept");
}

/// A script that allocates without end runs out of memory instead of taking the machine's.
#[tokio::test]
async fn memory_is_limited() {
    let error = error_of(
        "const hoard = []; while (true) hoard.push(new Array(1 << 20).fill(1.5));",
        30,
    )
    .await;
    assert!(error.contains("out of memory"), "{error}");
}

/// Deep recursion fails with a stack overflow instead of crashing the process.
#[tokio::test]
async fn deep_recursion_is_caught() {
    let error = error_of("function down(n) { return down(n + 1) + 1; } down(0);", 10).await;
    assert!(
        error.contains("stack overflow") || error.contains("call stack"),
        "{error}"
    );
}

/// A script may make at most the limit of tool calls.
#[tokio::test]
async fn tool_calls_are_limited() {
    let caller = Fake::default();
    let error = run_with(
        &caller,
        "for (let i = 0; i < 1000; i++) await tools.echo({ text: 'x' });",
        30,
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains(&format!("limit of {MAX_CALLS} tool calls")),
        "{error}"
    );
    assert_eq!(caller.calls.lock().unwrap().len(), MAX_CALLS);
}

/// A script waiting on a promise nothing will settle fails at once rather than at the time limit.
#[tokio::test]
async fn promises_that_never_settle_fail_at_once() {
    let started = Instant::now();
    let error = error_of("await new Promise(() => {});", 30).await;
    assert!(error.contains("nothing will settle"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

/// Dropping a running script, as cancelling the turn does, drops the calls it is waiting on.
#[tokio::test]
async fn dropping_the_call_drops_its_tool_calls() {
    let caller = Fake::default();
    let cut = tokio::time::timeout(
        Duration::from_millis(300),
        run_with(&caller, "await tools.slow({ seconds: 30 });", 60),
    )
    .await;
    assert!(cut.is_err());
    assert_eq!(caller.abandoned.load(Ordering::SeqCst), 1);
}

/// An engine told to stop leaves even a loop that never yields.
#[test]
fn a_cancelled_engine_stops_in_a_busy_loop() {
    let (requests, _incoming) = tokio::sync::mpsc::unbounded_channel();
    let (_answers, replies) = std::sync::mpsc::channel();
    let cancelled = Arc::new(AtomicBool::new(false));
    let link = Link {
        requests,
        replies,
        cancelled: cancelled.clone(),
        waited: Arc::new(AtomicU64::new(0)),
    };
    let limits = Limits {
        time: Duration::from_secs(300),
        memory: 16 << 20,
        calls: 1,
        printed: 1024,
    };
    let engine = std::thread::Builder::new()
        .stack_size(THREAD_STACK)
        .spawn(move || engine::run("for (;;) {}", &[], limits, link))
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let _abandon = Abandon(cancelled);
    drop(_abandon);
    let finished = engine.join().unwrap();
    assert_eq!(
        finished.error.as_deref(),
        Some("the script was stopped: the turn was cancelled")
    );
}

/// Printing past the kept amount drops the rest and says so, and a long result is cut and saved whole.
#[tokio::test]
async fn long_output_is_bounded_and_saved() {
    let text =
        text_of("const line = 'x'.repeat(1000); for (let i = 0; i < 2000; i++) print(line);").await;
    assert!(
        text.contains("bytes of printed output were dropped"),
        "{}",
        &text[text.len() - 300..]
    );
    assert!(text.len() < OUTPUT_LIMIT + 1024);
    let dir = scratch("spill");
    let script = Script {
        spill: Spill::new(&dir),
        ..tool()
    };
    let output = script
        .call_through(
            "s1",
            json!({ "code": "return 'y'.repeat(100000);" }),
            &Fake::default(),
        )
        .await
        .unwrap();
    assert!(output.text.contains("output truncated"), "{}", output.text);
    let saved: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
    assert_eq!(saved.len(), 1);
    assert_eq!(std::fs::read(saved[0].path()).unwrap().len(), 100000);
}

/// A script tool made for a registry offers its tools but none that are withheld, lists them with their arguments, and only looks when they all do.
#[test]
fn fitting_lists_the_tools_and_withholds_some() {
    let dir = scratch("fitted");
    let mut tools = Registry::builtin(&dir).unwrap();
    tools.register(Script::new(Spill::disabled()));
    let fitted = tools.fitted();
    let script = fitted.get(NAME).unwrap();
    assert!(!script.read_only());
    assert!(script.description().contains(
        "tools.glob(args: { limit?: number; path?: string; pattern: string }): Promise<{ paths: string[]; truncated: boolean }>"
    ));
    assert!(script.description().contains("tools.bash("));
    assert!(!script.description().contains("tools.script("));
    let looking = tools.read_only().fitted();
    let script = looking.get(NAME).unwrap();
    assert!(script.read_only());
    assert!(!script.description().contains("tools.bash("));
    assert!(!script.description().contains("tools.eval("));
    assert_eq!(
        Script::with_tools(Spill::disabled(), &tools.read_only()).exposed(),
        ["ast_grep", "glob", "grep", "lsp", "read", "web_search"]
    );
}

/// A call is shown by its opening comment, or its first line.
#[test]
fn calls_are_shown_by_their_first_line() {
    assert_eq!(
        headline("// Count things\nconst a = 1;\nreturn a;"),
        "Count things (3 lines)"
    );
    assert_eq!(headline("return 1"), "return 1");
    assert_eq!(headline("  \n"), "empty script");
}

/// Without a caller, as outside the agent loop, a script has no tools.
#[tokio::test]
async fn outside_the_loop_there_are_no_tools() {
    let output = tool()
        .call(json!({ "code": "return Object.keys(tools).length" }))
        .await
        .unwrap();
    assert_eq!(output.text, "0");
}
