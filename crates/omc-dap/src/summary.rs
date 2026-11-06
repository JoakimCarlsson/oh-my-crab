//! Short text for the model from what a debugger answers: places, frames, variables, threads and output.

use std::path::Path;

use omc_tools::Sandbox;
use serde_json::Value;

use crate::state::{Stop, Unread};

/// The longest value shown, in characters; longer ones are cut.
const VALUE_LIMIT: usize = 300;

/// How many lines of source are shown on each side of the line a program stopped on.
const CONTEXT: i64 = 2;

/// The largest source file read to show lines from, in bytes.
const SOURCE_LIMIT: u64 = 8 * 1024 * 1024;

/// `text` cut to `limit` characters, with `…` marking the cut, and on one line.
pub(crate) fn clip(text: &str, limit: usize) -> String {
    let flat = text.replace('\n', "\\n");
    if flat.chars().count() <= limit {
        return flat;
    }
    let mut cut: String = flat.chars().take(limit).collect();
    cut.push('…');
    cut
}

/// The path of `frame`'s source, when it has one on disk.
pub(crate) fn frame_path(frame: &Value) -> Option<&str> {
    frame["source"]["path"]
        .as_str()
        .filter(|path| !path.is_empty())
}

/// Where `frame` is, as `name at file:line`, the file shown relative to the working directory when it is inside it.
pub(crate) fn place(frame: &Value, sandbox: &Sandbox) -> String {
    let name = frame["name"].as_str().unwrap_or("??");
    let line = frame["line"].as_i64().unwrap_or(0);
    match frame_path(frame).or_else(|| frame["source"]["name"].as_str()) {
        Some(path) if line > 0 => {
            format!("{name} at {}:{line}", sandbox.display(Path::new(path)))
        }
        Some(path) => format!("{name} in {}", sandbox.display(Path::new(path))),
        None => match frame["instructionPointerReference"].as_str() {
            Some(address) => format!("{name} at {address} (no source)"),
            None => format!("{name} (no source)"),
        },
    }
}

/// The frames `frames`, one numbered line each, `#0` the innermost.
pub(crate) fn frames(frames: &[Value], sandbox: &Sandbox) -> String {
    frames
        .iter()
        .enumerate()
        .map(|(index, frame)| format!("  #{index} {}", place(frame, sandbox)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The lines of the file at `path` around `line`, the line itself marked with `>`; nothing when the file is outside the working directory, cannot be read, or has no such line.
pub(crate) fn excerpt(path: &str, line: i64, sandbox: &Sandbox) -> Option<String> {
    let resolved = sandbox.resolve(path).ok()?;
    if std::fs::metadata(&resolved).ok()?.len() > SOURCE_LIMIT {
        return None;
    }
    let text = std::fs::read_to_string(resolved).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let count = i64::try_from(lines.len()).ok()?;
    if line < 1 || line > count {
        return None;
    }
    let first = (line - CONTEXT).max(1);
    let last = (line + CONTEXT).min(count);
    let width = last.to_string().len();
    let shown: Vec<String> = (first..=last)
        .map(|number| {
            let marker = if number == line { ">" } else { " " };
            let index = usize::try_from(number - 1).unwrap_or_default();
            format!("  {marker} {number:>width$} | {}", lines[index].trim_end())
        })
        .collect();
    Some(shown.join("\n"))
}

/// The first line of a report on `stop`, naming the thread as `thread`.
pub(crate) fn stop_heading(stop: &Stop, thread: &str) -> String {
    let mut heading = format!("Stopped: {}", stop.reason);
    if !stop.breakpoints.is_empty() {
        let ids: Vec<String> = stop.breakpoints.iter().map(i64::to_string).collect();
        heading.push_str(&format!(" {}", ids.join(", ")));
    }
    let detail = [&stop.description, &stop.text]
        .into_iter()
        .flatten()
        .filter(|detail| !detail.eq_ignore_ascii_case(&stop.reason))
        .filter(|detail| !detail.starts_with("breakpoint "))
        .map(String::as_str)
        .collect::<Vec<_>>();
    if let Some(first) = detail.first() {
        heading.push_str(&format!(" ({})", clip(first, VALUE_LIMIT)));
    }
    heading.push_str(&format!(" in thread {thread}"));
    heading
}

/// One variable or evaluation result as a line, `indent` levels in: its name, type and value, and the reference that expands it when it has children.
pub(crate) fn variable_line(variable: &Value, indent: usize) -> String {
    let name = variable["name"].as_str().unwrap_or("?");
    let value = variable["value"]
        .as_str()
        .or_else(|| variable["result"].as_str())
        .unwrap_or("");
    let mut line = format!("{}{name}", "  ".repeat(indent));
    if let Some(kind) = variable["type"].as_str().filter(|kind| !kind.is_empty()) {
        line.push_str(&format!(": {kind}"));
    }
    line.push_str(&format!(" = {}", clip(value, VALUE_LIMIT)));
    if let Some(reference) = reference_of(variable) {
        line.push_str(&format!(" [ref {reference}]"));
    }
    line
}

/// The reference that lists `variable`'s children, when it has some.
pub(crate) fn reference_of(variable: &Value) -> Option<i64> {
    variable["variablesReference"]
        .as_i64()
        .filter(|reference| *reference > 0)
}

/// The threads `threads` as lines of id and name, the one `stopped` marked.
pub(crate) fn threads(threads: &[Value], stopped: Option<i64>) -> String {
    if threads.is_empty() {
        return "No threads.".to_owned();
    }
    threads
        .iter()
        .map(|thread| {
            let id = thread["id"].as_i64().unwrap_or_default();
            let marker = if Some(id) == stopped { "*" } else { " " };
            format!(
                "{marker} {id} {}",
                thread["name"].as_str().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The name of the thread `id` among `threads`, with its id.
pub(crate) fn thread_name(threads: &[Value], id: i64) -> String {
    threads
        .iter()
        .find(|thread| thread["id"].as_i64() == Some(id))
        .and_then(|thread| thread["name"].as_str())
        .filter(|name| !name.is_empty())
        .map_or_else(|| id.to_string(), |name| format!("{id} ({name})"))
}

/// Output the model has not seen, under `title`, at most `lines` of its last lines with a note of how many were left out; nothing when there is none.
pub(crate) fn output_block(title: &str, unread: &Unread, lines: usize) -> Option<String> {
    if unread.text.is_empty() && unread.dropped == 0 {
        return None;
    }
    let all: Vec<&str> = unread.text.lines().collect();
    let skipped = all.len().saturating_sub(lines);
    let mut block = format!("{title}:");
    if unread.dropped > 0 {
        block.push_str(&format!(
            "\n  [{} bytes written before this were dropped]",
            unread.dropped
        ));
    }
    if skipped > 0 {
        block.push_str(&format!(
            "\n  [{skipped} earlier lines not shown; action output with all: true shows what is kept]"
        ));
    }
    for line in &all[skipped..] {
        block.push_str(&format!("\n  {}", clip(line, 500)));
    }
    Some(block)
}

#[cfg(test)]
/// Tests for the summaries.
mod tests {
    use serde_json::json;

    use super::*;

    /// A scratch folder under the workspace's git-ignored `build/`.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-dap")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    /// A frame is shown as its function and its file and line, relative to the working directory.
    #[test]
    fn places_are_relative() {
        let dir = scratch("summary-place");
        let sandbox = Sandbox::new(&dir).unwrap();
        let frame = json!({
            "name": "sum",
            "line": 5,
            "source": {"path": dir.join("src/bug.c").display().to_string()},
        });
        assert_eq!(place(&frame, &sandbox), "sum at src/bug.c:5");
        let bare = json!({"name": "??", "instructionPointerReference": "0x10"});
        assert_eq!(place(&bare, &sandbox), "?? at 0x10 (no source)");
        assert_eq!(
            frames(&[frame, bare], &sandbox),
            "  #0 sum at src/bug.c:5\n  #1 ?? at 0x10 (no source)"
        );
    }

    /// The source around a line is shown with the line marked, and a file outside the working directory is not read.
    #[test]
    fn excerpts_mark_the_line_and_stay_inside() {
        let dir = scratch("summary-excerpt");
        std::fs::write(dir.join("a.c"), "one\ntwo\nthree\nfour\nfive\nsix\n").unwrap();
        let sandbox = Sandbox::new(&dir).unwrap();
        let shown = excerpt(&dir.join("a.c").display().to_string(), 2, &sandbox).unwrap();
        assert_eq!(
            shown,
            "    1 | one\n  > 2 | two\n    3 | three\n    4 | four"
        );
        assert!(excerpt("/etc/hostname", 1, &sandbox).is_none());
        assert!(excerpt(&dir.join("a.c").display().to_string(), 99, &sandbox).is_none());
    }

    /// A stop's heading names its reason, breakpoints, detail and thread, without repeating itself.
    #[test]
    fn stop_headings() {
        let stop = Stop {
            reason: "breakpoint".into(),
            description: Some("breakpoint 1.1".into()),
            breakpoints: vec![1],
            thread: Some(9),
            ..Stop::default()
        };
        assert_eq!(
            stop_heading(&stop, "9 (bug)"),
            "Stopped: breakpoint 1 in thread 9 (bug)"
        );
        let crash = Stop {
            reason: "exception".into(),
            description: Some("signal SIGSEGV: invalid address".into()),
            ..Stop::default()
        };
        assert_eq!(
            stop_heading(&crash, "1"),
            "Stopped: exception (signal SIGSEGV: invalid address) in thread 1"
        );
    }

    /// Variables show their type, a cut value and the reference that expands them.
    #[test]
    fn variable_lines() {
        let variable =
            json!({"name": "xs", "type": "int *", "value": "0x10", "variablesReference": 4});
        assert_eq!(variable_line(&variable, 1), "  xs: int * = 0x10 [ref 4]");
        let long = json!({"name": "s", "value": "x".repeat(400), "variablesReference": 0});
        assert!(variable_line(&long, 0).ends_with('…'));
        assert_eq!(clip("a\nb", 10), "a\\nb");
    }

    /// Threads are listed with the stopped one marked, and named by id and name.
    #[test]
    fn thread_lines() {
        let list = [
            json!({"id": 1, "name": "main"}),
            json!({"id": 2, "name": "worker"}),
        ];
        assert_eq!(threads(&list, Some(2)), "  1 main\n* 2 worker");
        assert_eq!(thread_name(&list, 2), "2 (worker)");
        assert_eq!(thread_name(&list, 3), "3");
    }

    /// Output shows its last lines with a note of what was left out, and nothing when there is none.
    #[test]
    fn output_blocks() {
        let unread = Unread {
            text: "a\nb\nc\n".into(),
            dropped: 0,
        };
        let block = output_block("Program output", &unread, 2).unwrap();
        assert!(block.contains("[1 earlier lines not shown"));
        assert!(block.ends_with("\n  b\n  c"));
        assert!(output_block("x", &Unread::default(), 5).is_none());
    }
}
