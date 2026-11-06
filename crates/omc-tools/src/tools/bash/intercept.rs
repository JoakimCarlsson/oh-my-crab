//! Steering the model from shell commands to the dedicated tool that does the same job better.
//!
//! A command is turned away only when a dedicated tool gives exactly the same result: `cat`, `less` or `more` of a few text files, `head` or `tail` of a number of lines of one text file, and `echo`, `printf` or a `cat` here-document whose text the shell does not expand, written over one file. Every word must be literal, every path must lie inside the working directory, every option must be one the dedicated tool reproduces, and the line must be that one command alone, with no control operator and not sent to the background. Anything else runs. Searches with `grep` or `rg`, `find` by name, `ls -R`, `sed -i` and appending to a file run too, since the dedicated tools differ from them in what they see or do, and such a command, like a command that would be turned away when it stands inside a pipeline or a list, runs with a one-line tip naming the tool.

use std::io::Read as _;

use serde_json::{Value, json};

use super::shell::{self, CommandLine, Simple, Word, program_name};
use crate::Sandbox;

/// Most files a `cat`, `less` or `more` may name and still be turned away, one `read` per file.
const MAX_FILES: usize = 4;

/// Lines `head` and `tail` show when given no count.
const DEFAULT_LINES: &str = "10";

/// Bytes from the start of a file looked at for a NUL byte, which marks it as binary rather than text.
const TEXT_PROBE_BYTES: u64 = 8 * 1024;

/// The tip for a search of file contents.
const GREP_TIP: &str = "[tip: the `grep` tool searches file contents too, skipping ignored and binary files, and gives the snapshot tags `edit` needs]";

/// The tip for finding or listing files by name.
const GLOB_TIP: &str = "[tip: the `glob` tool finds files by name, skipping ignored files and listing the newest first]";

/// The tip for editing a file in place.
const SED_TIP: &str = "[tip: for a targeted change, the `edit` tool checks the lines against a `read` snapshot and shows the change]";

/// The tip for appending to a file that exists.
const APPEND_TIP: &str = "[tip: to add to an existing file, `edit` with `insert_after` on its last line shows the change]";

/// The tip for appending to a file that does not exist yet.
const CREATE_TIP: &str =
    "[tip: the `write` tool creates a file from its whole text, with no shell quoting in the way]";

/// What to do with a command line.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Run it as it is.
    Run,
    /// Run it, and add this tip to its result.
    Hint(String),
    /// Do not run it; tell the model this instead.
    Refuse(String),
}

/// A dedicated tool that does exactly what a command does.
struct Redirection {
    /// The tool's name.
    tool: &'static str,
    /// Why the tool is the better choice, as a sentence.
    reason: &'static str,
    /// What to call instead, as a sentence.
    advice: String,
}

/// Decides whether `command` runs, runs with a tip, or is turned away in favour of a dedicated tool working inside `sandbox`.
pub(super) fn check(command: &str, sandbox: &Sandbox) -> Verdict {
    let line = shell::parse(command);
    if let Some(alone) = only_command(&line)
        && let Some(redirection) = equivalent(alone, sandbox)
    {
        return Verdict::Refuse(format!(
            "Not run: {} {} Anything the tool does differently, such as other options, pipelines or commands sent to the background, still runs with bash.",
            redirection.reason, redirection.advice
        ));
    }
    if line.unusual {
        return Verdict::Run;
    }
    line.pipelines
        .iter()
        .filter_map(|pipeline| pipeline.stages.first())
        .find_map(|first| tip(first, sandbox))
        .map_or(Verdict::Run, Verdict::Hint)
}

/// The one command `line` consists of, when it is a single plain command with no control operator and is not sent to the background.
fn only_command(line: &CommandLine) -> Option<&Simple> {
    match line.pipelines.as_slice() {
        [pipeline]
            if !line.unusual
                && line.operators.is_empty()
                && !pipeline.background
                && pipeline.stages.len() == 1 =>
        {
            pipeline.stages.first()
        }
        _ => None,
    }
}

/// The one-line tip for `simple`, the first command of a pipeline, when a dedicated tool does the same job or a similar one; the program is found past assignments and wrappers such as `sudo`.
fn tip(simple: &Simple, sandbox: &Sandbox) -> Option<String> {
    if let Some(redirection) = equivalent(simple, sandbox) {
        return Some(format!(
            "[tip: the `{}` tool does what `{}` did here, without the shell]",
            redirection.tool, simple.words[0].text
        ));
    }
    let (program, arguments) = simple.invocation().split_first()?;
    let arguments: Vec<&str> = arguments.iter().map(|word| word.text.as_str()).collect();
    let tip = match program_name(program) {
        "grep" | "egrep" | "fgrep" | "rg" if !reads_stdin_redirect(simple) => GREP_TIP,
        "find" if finds_by_name(&arguments) => GLOB_TIP,
        "ls" if lists_recursively(&arguments) => GLOB_TIP,
        "sed" if edits_in_place(&arguments) => SED_TIP,
        "echo" | "printf" | "cat" => appended(simple, &Paths(sandbox))?,
        _ => return None,
    };
    Some(tip.to_owned())
}

/// Whether `simple` reads its standard input from a redirection, so that it searches what it is fed rather than files.
fn reads_stdin_redirect(simple: &Simple) -> bool {
    simple.redirects.iter().any(|redirect| {
        matches!(
            redirect.operator.as_str(),
            "<" | "0<" | "<<" | "<<-" | "<<<" | "0<<" | "0<<-" | "0<<<"
        )
    })
}

/// Whether a `find` with `arguments` matches files by name or path.
fn finds_by_name(arguments: &[&str]) -> bool {
    arguments
        .iter()
        .any(|word| matches!(*word, "-name" | "-iname" | "-path" | "-ipath"))
}

/// Whether an `ls` with `arguments` lists directories recursively.
fn lists_recursively(arguments: &[&str]) -> bool {
    arguments.iter().any(|word| {
        *word == "--recursive"
            || (word.starts_with('-') && !word.starts_with("--") && word.contains('R'))
    })
}

/// Whether a `sed` with `arguments` edits its files in place.
fn edits_in_place(arguments: &[&str]) -> bool {
    arguments
        .iter()
        .any(|word| word.starts_with("-i") || word.starts_with("--in-place"))
}

/// The tip for `simple` appending its output to one file inside the working directory: `edit` when the file exists, `write` when it does not.
fn appended(simple: &Simple, paths: &Paths) -> Option<&'static str> {
    let output = simple
        .redirects
        .iter()
        .find(|redirect| redirect.writes_stdout() && redirect.appends())?;
    let target = output.target.as_ref().filter(|target| target.literal)?;
    let resolved = paths.inside(&target.text)?;
    if target.text.starts_with("/dev/") || resolved.is_dir() {
        return None;
    }
    Some(if resolved.is_file() {
        APPEND_TIP
    } else {
        CREATE_TIP
    })
}

/// The dedicated tool that gives exactly what `simple` gives, when there is one.
fn equivalent(simple: &Simple, sandbox: &Sandbox) -> Option<Redirection> {
    let literal = |word: &Word| word.literal;
    let targets_literal = simple
        .redirects
        .iter()
        .all(|redirect| redirect.target.as_ref().is_some_and(literal));
    if !simple.words.iter().all(literal) || !targets_literal {
        return None;
    }
    let (program, arguments) = simple.words.split_first()?;
    let program = program_name(program);
    let arguments: Vec<&str> = arguments.iter().map(|word| word.text.as_str()).collect();
    let paths = Paths(sandbox);
    if !simple.redirects.is_empty() {
        return match program {
            "echo" | "printf" | "cat" => written(simple, program, &arguments, &paths),
            _ => None,
        };
    }
    match program {
        "cat" | "less" | "more" => shown(program, &arguments, &paths),
        "head" | "tail" => sliced(program, &arguments, &paths),
        _ => None,
    }
}

/// Checks paths against the working directory the dedicated tools are confined to.
struct Paths<'a>(&'a Sandbox);

impl Paths<'_> {
    /// Whether `path` names a text file `read` can open: a file whose first bytes hold no NUL byte.
    fn text_file(&self, path: &str) -> bool {
        if path.starts_with('-') {
            return false;
        }
        let Ok(resolved) = self.0.resolve_readable(path) else {
            return false;
        };
        let Ok(file) = std::fs::File::open(&resolved) else {
            return false;
        };
        let mut start = Vec::new();
        resolved.is_file()
            && file.take(TEXT_PROBE_BYTES).read_to_end(&mut start).is_ok()
            && !start.contains(&0)
    }

    /// Where `path` lies inside the working directory, when it does.
    fn inside(&self, path: &str) -> Option<std::path::PathBuf> {
        (!path.starts_with('-'))
            .then(|| self.0.resolve(path).ok())
            .flatten()
    }
}

/// A sentence telling the model to call `tool` once with each of `calls`.
fn advise(tool: &str, calls: &[Value]) -> String {
    let calls: Vec<String> = calls.iter().map(Value::to_string).collect();
    format!(
        "Call `{tool}` with {} instead.",
        calls.join(", and again with ")
    )
}

/// `cat`, with `-n` at most, or `less` or `more`, with no option, named `program`, of one to [`MAX_FILES`] text files, which `read` shows whole when asked for every line.
fn shown(program: &str, arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let cat = program == "cat";
    let files: Vec<&str> = arguments
        .iter()
        .copied()
        .filter(|word| !(cat && *word == "-n"))
        .collect();
    let readable = !files.is_empty()
        && files.len() <= MAX_FILES
        && files.iter().all(|file| paths.text_file(file));
    let reason = if cat {
        "`cat` on a text file is what the `read` tool is for: it numbers the lines and gives the snapshot tag `edit` needs."
    } else {
        "a pager on a text file is what the `read` tool is for: it numbers the lines and gives the snapshot tag `edit` needs."
    };
    readable.then(|| Redirection {
        tool: "read",
        reason,
        advice: advise(
            "read",
            &files
                .iter()
                .map(|file| json!({ "path": format!("{file}:1-") }))
                .collect::<Vec<_>>(),
        ),
    })
}

/// The file and the line count of a `head` or `tail` with `arguments`, read in any order, when they hold one file and no option but a line count.
fn head_or_tail_operands<'a>(arguments: &[&'a str]) -> Option<(&'a str, &'a str)> {
    let mut count = None;
    let mut file = None;
    let mut options_ended = false;
    let mut words = arguments.iter().copied();
    while let Some(word) = words.next() {
        if options_ended || !word.starts_with('-') || word == "-" {
            if file.replace(word).is_some() {
                return None;
            }
            continue;
        }
        let value = match word {
            "--" => {
                options_ended = true;
                continue;
            }
            "-n" | "--lines" => words.next()?,
            _ if word.starts_with("--lines=") => &word["--lines=".len()..],
            _ if word.starts_with("--") => return None,
            _ if word.starts_with("-n") => &word["-n".len()..],
            _ if word[1..].bytes().all(|byte| byte.is_ascii_digit()) => &word[1..],
            _ => return None,
        };
        if count.replace(value).is_some() {
            return None;
        }
    }
    Some((file?, count.unwrap_or(DEFAULT_LINES)))
}

/// `head` or `tail`, named `program`, of a number of lines of one text file, which `read` does with a line range.
fn sliced(program: &str, arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let (file, count) = head_or_tail_operands(arguments)?;
    if !paths.text_file(file) {
        return None;
    }
    let lines = |text: &str| {
        !text.is_empty()
            && text.bytes().all(|byte| byte.is_ascii_digit())
            && text.bytes().any(|byte| byte != b'0')
    };
    let range = match (program, count.strip_prefix('+')) {
        ("tail", Some(from)) if lines(from) => format!("{from}-"),
        ("tail", None) if lines(count) => format!("-{count}"),
        ("head", None) if lines(count) => format!("1-{count}"),
        _ => return None,
    };
    Some(Redirection {
        tool: "read",
        reason: "`head` and `tail` on a text file are what the `read` tool is for: give it a line range.",
        advice: advise("read", &[json!({ "path": format!("{file}:{range}") })]),
    })
}

/// `echo`, `printf` or a `cat` here-document, named `program`, whose text the shell passes on as written, sent over one file inside the working directory, which `write` does.
fn written(
    simple: &Simple,
    program: &str,
    arguments: &[&str],
    paths: &Paths,
) -> Option<Redirection> {
    let (outputs, inputs): (Vec<_>, Vec<_>) = simple
        .redirects
        .iter()
        .partition(|redirect| redirect.writes_stdout());
    let [output] = outputs.as_slice() else {
        return None;
    };
    if output.appends() {
        return None;
    }
    let here_doc = match inputs.as_slice() {
        [] => None,
        [input] if input.is_here_doc() => Some(input),
        _ => return None,
    };
    let fed_by_here_doc = program == "cat"
        && arguments.is_empty()
        && here_doc.is_some_and(|input| input.here_doc_verbatim());
    let printed = program != "cat" && here_doc.is_none();
    let target = output.target.as_ref()?.text.as_str();
    let inside = paths
        .inside(target)
        .is_some_and(|resolved| !resolved.is_dir());
    if !(fed_by_here_doc || printed) || !inside || target.starts_with("/dev/") {
        return None;
    }
    let plain_echo = program == "echo"
        && arguments
            .first()
            .is_none_or(|first| !first.starts_with('-'));
    let call = if plain_echo {
        json!({ "path": target, "content": format!("{}\n", arguments.join(" ")) })
    } else {
        json!({ "path": target, "content": "…" })
    };
    Some(Redirection {
        tool: "write",
        reason: "writing a file through the shell is what the `write` tool is for: it shows the change and keeps quoting out of the way.",
        advice: format!("Call `write` with {call} instead, the whole text as `content`."),
    })
}

#[cfg(all(test, unix))]
/// Tests for steering commands to dedicated tools.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// A sandbox holding `src/main.rs`, `notes.txt`, a binary `image.bin` and an empty `docs` directory.
    fn sandbox(name: &str) -> Sandbox {
        let dir = scratch(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "a\nb\n").unwrap();
        std::fs::write(dir.join("image.bin"), b"PNG\0\x01\x02").unwrap();
        Sandbox::new(&dir).unwrap()
    }

    /// The refusal message for `command`, failing when it would run.
    fn refused(sandbox: &Sandbox, command: &str) -> String {
        match check(command, sandbox) {
            Verdict::Refuse(message) => message,
            other => panic!("`{command}` was not refused: {other:?}"),
        }
    }

    /// The tip `command` runs with, failing when it would be turned away or run without one.
    fn hinted(sandbox: &Sandbox, command: &str) -> String {
        match check(command, sandbox) {
            Verdict::Hint(tip) => tip,
            other => panic!("`{command}` did not run with a tip: {other:?}"),
        }
    }

    /// Reads of whole text files or of a number of their lines, and unexpanded text written over a file, are turned away with the call that does exactly the same.
    #[test]
    fn refuses_exact_equivalents_with_their_calls() {
        let sandbox = sandbox("intercept-refuse");
        for (command, call) in [
            (
                "cat src/main.rs",
                r#"`read` with {"path":"src/main.rs:1-"}"#,
            ),
            (
                "cat -n src/main.rs notes.txt",
                r#"{"path":"src/main.rs:1-"}, and again with {"path":"notes.txt:1-"}"#,
            ),
            ("less notes.txt", r#"{"path":"notes.txt:1-"}"#),
            (
                "more notes.txt src/main.rs",
                r#"{"path":"notes.txt:1-"}, and again with {"path":"src/main.rs:1-"}"#,
            ),
            ("head -n 50 src/main.rs", r#"{"path":"src/main.rs:1-50"}"#),
            ("head -20 src/main.rs", r#"{"path":"src/main.rs:1-20"}"#),
            ("head src/main.rs -n 3", r#"{"path":"src/main.rs:1-3"}"#),
            ("tail notes.txt", r#"{"path":"notes.txt:-10"}"#),
            ("tail -n +5 notes.txt", r#"{"path":"notes.txt:5-"}"#),
            ("tail --lines=2 notes.txt", r#"{"path":"notes.txt:-2"}"#),
            ("tail -- notes.txt", r#"{"path":"notes.txt:-10"}"#),
            (
                "echo hello world > notes.txt",
                r#"`write` with {"content":"hello world\n","path":"notes.txt"}"#,
            ),
            (
                "printf 'a b' > new.txt",
                r#"`write` with {"content":"…","path":"new.txt"}"#,
            ),
            (
                "cat > new.txt <<'EOF'\nhello $USER\nEOF",
                r#"`write` with {"content":"…","path":"new.txt"}"#,
            ),
            (
                "cat > new.txt <<EOF\nplain text\nEOF",
                r#"`write` with {"content":"…","path":"new.txt"}"#,
            ),
        ] {
            let message = refused(&sandbox, command);
            assert!(message.contains(call), "`{command}`: {message}");
            assert!(message.starts_with("Not run: "), "{message}");
        }
    }

    /// Anything the dedicated tools would do differently runs, with no tip.
    #[test]
    fn runs_everything_else() {
        let sandbox = sandbox("intercept-run");
        for command in [
            "cat missing.txt",
            "cat /etc/hostname",
            "cat src/*.rs",
            "cat $FILE",
            "cat",
            "cat a b c d e",
            "cat image.bin",
            "cat docs",
            "less -N notes.txt",
            "head -n 3 image.bin",
            "tail -f notes.txt",
            "tail -F notes.txt",
            "tail --follow notes.txt",
            "tail -n 5 -f notes.txt",
            "head -c 100 notes.txt",
            "head --bytes=3 notes.txt",
            "head -q notes.txt",
            "head -v notes.txt",
            "head -n 0 notes.txt",
            "head -n -2 notes.txt",
            "tail -n 5 -n 6 notes.txt",
            "head -n 5 notes.txt src/main.rs",
            "echo hi",
            "echo hi > /dev/null",
            "echo hi > ../outside.txt",
            "echo $HOME > notes.txt",
            "echo hi 2> err.txt",
            "sudo cat notes.txt",
            "cat notes.txt > copy.txt",
            "cat < notes.txt",
            "cat > new.txt",
            "cat > new.txt <<EOF\n$HOME\nEOF",
            "cat > new.txt <<EOF\n`date`\nEOF",
            "cat > new.txt <<EOF\na\\tb\nEOF",
            "cat > new.txt <<< hello",
            "grep x < notes.txt",
            "wc -l notes.txt | sort",
            "cargo test | grep -rn x src",
            "git status",
        ] {
            assert_eq!(check(command, &sandbox), Verdict::Run, "`{command}`");
        }
    }

    /// Searches, finds, recursive listings, in-place edits and appends run, with a tip naming the tool that does a similar job.
    #[test]
    fn runs_near_equivalents_with_a_tip() {
        let sandbox = sandbox("intercept-near");
        for (command, tool) in [
            ("grep -rn 'fn main' src", "`grep`"),
            ("grep x notes.txt", "`grep`"),
            ("rg -n 'fn \\w+' -g '*.rs'", "`grep`"),
            ("LC_ALL=C grep -r x .", "`grep`"),
            ("find . -name '*.rs' -type f", "`glob`"),
            ("find src -name main.rs", "`glob`"),
            ("ls -R docs", "`glob`"),
            ("ls -laR", "`glob`"),
            ("sed -i 's/main/start/' src/main.rs", "`edit`"),
            ("echo more >> notes.txt", "`insert_after`"),
            ("echo first >> new.txt", "`write`"),
        ] {
            let tip = hinted(&sandbox, command);
            assert!(tip.contains(tool), "`{command}`: {tip}");
        }
    }

    /// A command that would be turned away alone runs with a tip when it is sent to the background, followed by a control operator, or part of a pipeline or list.
    #[test]
    fn hints_when_not_alone() {
        let sandbox = sandbox("intercept-hint");
        for command in [
            "cat notes.txt &",
            "cat notes.txt;",
            "cat src/main.rs | wc -l",
            "head -n 3 notes.txt && echo done",
            "cargo build; tail -n 5 notes.txt",
        ] {
            let tip = hinted(&sandbox, command);
            assert!(tip.contains("`read`"), "`{command}`: {tip}");
        }
        assert!(matches!(
            check("cargo build && grep -rn x src", &sandbox),
            Verdict::Hint(_)
        ));
    }
}
