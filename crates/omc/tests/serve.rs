//! `crab serve` ends when the client says so, not only when it closes standard input.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long the process may take to end after `Close`.
const DEADLINE: Duration = Duration::from_secs(10);

/// A `Close` sent while standard input stays open ends the process, successfully and in good time.
#[test]
fn close_ends_the_process_while_input_is_open() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_crab"))
        .arg("serve")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "\"Close\"").unwrap();
    input.flush().unwrap();
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if started.elapsed() > DEADLINE {
            child.kill().unwrap();
            panic!("crab serve was still running {DEADLINE:?} after Close");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    drop(input);
    assert!(status.success());
}
