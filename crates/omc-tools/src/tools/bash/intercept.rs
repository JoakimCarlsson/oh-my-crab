//! Steering the model from shell commands to the dedicated tool that does the same job better.
//!
//! A command line that is nothing but one plain use of `cat`, `head`, `tail`, `less`, `grep`, `rg`, `find`, `ls -R`, `sed -i`, or `echo`/`printf`/`cat` into a file is turned away with a message naming the tool to use and the arguments that do the same. Only forms whose meaning is certain are turned away: every word must be literal, every path must lie inside the working directory, and every option must have an equivalent in the dedicated tool. Anything else runs, and a command line that holds such a use inside a pipeline or a list of commands runs with a short tip naming the tool.

use std::path::PathBuf;

use serde_json::{Map, Value, json};

use super::shell::{self, CommandLine, Simple, Word};
use crate::Sandbox;

/// Most files a `cat` may name and still be turned away, one `read` per file.
const MAX_FILES: usize = 4;

/// Lines `head` and `tail` show when given no count.
const DEFAULT_LINES: &str = "10";

/// Short options of `grep` that the `grep` tool does the same as, or that change nothing it shows.
const GREP_FLAGS: &str = "rRniEHsI";

/// Short options of `rg` that the `grep` tool does the same as, or that change nothing it shows.
const RG_FLAGS: &str = "niH";

/// Characters that mean something in an extended or Rust regular expression but are plain in a basic one, so a basic `grep` pattern holding one means something else to the `grep` tool.
const BASIC_REGEX_PLAIN: [char; 8] = ['\\', '+', '?', '(', ')', '{', '}', '|'];

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

/// A dedicated tool that does what a command does.
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
        && let Some(redirection) = redirect(alone, sandbox)
    {
        return Verdict::Refuse(format!(
            "Not run: {} {} Pipelines, redirections and anything else that needs the shell still run with bash.",
            redirection.reason, redirection.advice
        ));
    }
    if line.unusual {
        return Verdict::Run;
    }
    line.pipelines
        .iter()
        .filter_map(|pipeline| pipeline.stages.first())
        .find_map(|first| Some((first, redirect(first, sandbox)?)))
        .map_or(Verdict::Run, |(simple, redirection)| {
            Verdict::Hint(format!(
                "[tip: the `{}` tool does what `{}` did here, without the shell]",
                redirection.tool, simple.words[0].text
            ))
        })
}

/// The one command `line` consists of, when it is a single plain command.
fn only_command(line: &CommandLine) -> Option<&Simple> {
    match line.pipelines.as_slice() {
        [pipeline] if !line.unusual && pipeline.stages.len() == 1 => pipeline.stages.first(),
        _ => None,
    }
}

/// The dedicated tool that does what `simple` does, when it is a plain use of a command one of them replaces.
fn redirect(simple: &Simple, sandbox: &Sandbox) -> Option<Redirection> {
    let literal = |word: &Word| word.literal;
    let targets_literal = simple
        .redirects
        .iter()
        .all(|redirect| redirect.target.as_ref().is_some_and(literal));
    if !simple.words.iter().all(literal) || !targets_literal {
        return None;
    }
    let (program, arguments) = simple.words.split_first()?;
    let arguments: Vec<&str> = arguments.iter().map(|word| word.text.as_str()).collect();
    let paths = Paths(sandbox);
    if !simple.redirects.is_empty() {
        return match program.text.as_str() {
            "echo" | "printf" | "cat" => written(simple, &program.text, &arguments, &paths),
            _ => None,
        };
    }
    match program.text.as_str() {
        "cat" => concatenated(&arguments, &paths),
        "head" | "tail" => sliced(&program.text, &arguments, &paths),
        "less" | "more" => paged(&arguments, &paths),
        "grep" | "egrep" => grepped(program.text == "egrep", &arguments, &paths),
        "rg" => ripgrepped(&arguments, &paths),
        "find" => found(&arguments, &paths),
        "ls" => listed(&arguments, &paths),
        "sed" => edited(&arguments, &paths),
        _ => None,
    }
}

/// Checks paths against the working directory the dedicated tools are confined to.
struct Paths<'a>(&'a Sandbox);

impl Paths<'_> {
    /// Whether `path` names a file `read` can open.
    fn readable_file(&self, path: &str) -> bool {
        !path.starts_with('-')
            && self
                .0
                .resolve_readable(path)
                .is_ok_and(|resolved| resolved.is_file())
    }

    /// Where `path` lies inside the working directory, when it does.
    fn inside(&self, path: &str) -> Option<PathBuf> {
        (!path.starts_with('-'))
            .then(|| self.0.resolve(path).ok())
            .flatten()
    }

    /// Whether `path` names a file inside the working directory.
    fn file(&self, path: &str) -> bool {
        self.inside(path).is_some_and(|resolved| resolved.is_file())
    }

    /// Whether `path` names a directory inside the working directory.
    fn directory(&self, path: &str) -> bool {
        self.inside(path).is_some_and(|resolved| resolved.is_dir())
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

/// `cat` of one or a few files, which `read` does.
fn concatenated(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let files: Vec<&str> = arguments
        .iter()
        .copied()
        .filter(|word| *word != "-n")
        .collect();
    let readable = !files.is_empty()
        && files.len() <= MAX_FILES
        && files.iter().all(|file| paths.readable_file(file));
    readable.then(|| Redirection {
        tool: "read",
        reason: "`cat` on a file is what the `read` tool is for: it numbers the lines and gives the snapshot tag `edit` needs.",
        advice: advise("read", &files.iter().map(|file| json!({ "path": file })).collect::<Vec<_>>()),
    })
}

/// `head` or `tail` of one file, which `read` does with a line range.
fn sliced(program: &str, arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let mut count = None;
    let mut file = None;
    let mut words = arguments.iter().copied();
    while let Some(word) = words.next() {
        let value = match word {
            "-n" | "--lines" => words.next()?,
            _ if word.starts_with("--lines=") => &word["--lines=".len()..],
            _ if word.starts_with("-n") => &word[2..],
            _ if word.starts_with('-') => &word[1..],
            _ if file.is_none() => {
                file = Some(word);
                continue;
            }
            _ => return None,
        };
        count = Some(value);
    }
    let file = file.filter(|file| paths.readable_file(file))?;
    let count = count.unwrap_or(DEFAULT_LINES);
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    let range = match (program, count.strip_prefix('+')) {
        ("tail", Some(from)) if digits(from) => format!("{from}-"),
        ("tail", None) if digits(count) => format!("-{count}"),
        ("head", None) if digits(count) => format!("1-{count}"),
        _ => return None,
    };
    Some(Redirection {
        tool: "read",
        reason: "`head` and `tail` on a file are what the `read` tool is for: give it a line range.",
        advice: advise("read", &[json!({ "path": format!("{file}:{range}") })]),
    })
}

/// `less` or `more` of one file, which `read` does.
fn paged(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let [file] = arguments else { return None };
    paths.readable_file(file).then(|| Redirection {
        tool: "read",
        reason: "a pager on a file is what the `read` tool is for.",
        advice: advise("read", &[json!({ "path": file })]),
    })
}

/// The options and operands of a search, as the `grep` tool takes them.
#[derive(Default)]
struct Search<'a> {
    /// The pattern given with `-e`.
    pattern: Option<&'a str>,
    /// The glob paths must match.
    glob: Option<&'a str>,
    /// Whether to search directories.
    recursive: bool,
    /// Whether the pattern is an extended rather than a basic regular expression.
    extended: bool,
    /// Whether to ignore case.
    ignore_case: bool,
    /// The words that are not options.
    operands: Vec<&'a str>,
}

impl<'a> Search<'a> {
    /// Sets the flag the short option `flag` stands for.
    fn flag(&mut self, flag: char) {
        match flag {
            'r' | 'R' => self.recursive = true,
            'i' => self.ignore_case = true,
            'E' => self.extended = true,
            _ => {}
        }
    }

    /// Sets `slot` to `value`, failing when it was set already.
    fn once(slot: &mut Option<&'a str>, value: Option<&'a str>) -> Option<()> {
        match (slot.is_none(), value) {
            (true, Some(value)) => {
                *slot = Some(value);
                Some(())
            }
            _ => None,
        }
    }

    /// The pattern and the one path to search, when the operands give them.
    fn pattern_and_path(&mut self) -> Option<(&'a str, Option<&'a str>)> {
        let pattern = match self.pattern {
            Some(pattern) => pattern,
            None if !self.operands.is_empty() => self.operands.remove(0),
            None => return None,
        };
        match self.operands.as_slice() {
            [] => Some((pattern, None)),
            [path] if *path != "-" => Some((pattern, Some(path))),
            _ => None,
        }
    }

    /// The `grep` tool call that does the same search of `path`.
    fn call(&self, pattern: &str, path: &str) -> Redirection {
        let mut args = Map::new();
        args.insert("pattern".into(), json!(pattern));
        if path != "." {
            args.insert("path".into(), json!(path));
        }
        if let Some(glob) = self.glob {
            args.insert("glob".into(), json!(glob));
        }
        if self.ignore_case {
            args.insert("case_insensitive".into(), json!(true));
        }
        Redirection {
            tool: "grep",
            reason: "searching files is what the `grep` tool is for: it skips ignored and binary files and gives the snapshot tags `edit` needs.",
            advice: advise("grep", &[Value::Object(args)]),
        }
    }
}

/// A `grep` search of one file, or of one directory with `-r`, using options the `grep` tool has; `egrep` when `extended`.
fn grepped(extended: bool, arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let mut search = Search {
        extended,
        ..Search::default()
    };
    let mut words = arguments.iter().copied();
    let mut options_ended = false;
    while let Some(word) = words.next() {
        if options_ended || !word.starts_with('-') || word == "-" {
            search.operands.push(word);
            continue;
        }
        match word {
            "--" => options_ended = true,
            "-e" | "--regexp" => Search::once(&mut search.pattern, words.next())?,
            "--include" => Search::once(&mut search.glob, words.next())?,
            "--recursive" | "--dereference-recursive" => search.recursive = true,
            "--ignore-case" => search.ignore_case = true,
            "--extended-regexp" => search.extended = true,
            "--line-number" | "--with-filename" | "--no-messages" => {}
            _ if word.starts_with("--include=") => {
                Search::once(&mut search.glob, word.strip_prefix("--include="))?
            }
            _ if word.starts_with("--color") => {}
            _ if word[1..].chars().all(|flag| GREP_FLAGS.contains(flag)) => {
                word[1..].chars().for_each(|flag| search.flag(flag));
            }
            _ => return None,
        }
    }
    let (pattern, path) = search.pattern_and_path()?;
    let same_meaning = if search.extended {
        !pattern.contains("\\<") && !pattern.contains("\\>") && !has_backreference(pattern)
    } else {
        !pattern.contains(BASIC_REGEX_PLAIN)
    };
    let searchable = match (path, search.recursive) {
        (None, true) => Some("."),
        (Some(path), true) if paths.inside(path).is_some_and(|resolved| resolved.exists()) => {
            Some(path)
        }
        (Some(path), false) if paths.file(path) => Some(path),
        _ => None,
    }?;
    (same_meaning && regex::Regex::new(pattern).is_ok()).then(|| search.call(pattern, searchable))
}

/// Whether `pattern` refers back to a group, as `\1` does, which Rust regular expressions cannot.
fn has_backreference(pattern: &str) -> bool {
    pattern
        .as_bytes()
        .windows(2)
        .any(|pair| pair[0] == b'\\' && pair[1].is_ascii_digit())
}

/// An `rg` search of one path, using options the `grep` tool has.
fn ripgrepped(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let mut search = Search {
        extended: true,
        recursive: true,
        ..Search::default()
    };
    let mut words = arguments.iter().copied();
    let mut options_ended = false;
    while let Some(word) = words.next() {
        if options_ended || !word.starts_with('-') || word == "-" {
            search.operands.push(word);
            continue;
        }
        match word {
            "--" => options_ended = true,
            "-e" | "--regexp" => Search::once(&mut search.pattern, words.next())?,
            "-g" | "--glob" => Search::once(&mut search.glob, words.next())?,
            "--ignore-case" => search.ignore_case = true,
            "--line-number" | "--with-filename" | "--no-heading" => {}
            _ if word.starts_with("--glob=") => {
                Search::once(&mut search.glob, word.strip_prefix("--glob="))?
            }
            _ if word.starts_with("--color") => {}
            _ if word[1..].chars().all(|flag| RG_FLAGS.contains(flag)) => {
                word[1..].chars().for_each(|flag| search.flag(flag));
            }
            _ => return None,
        }
    }
    let (pattern, path) = search.pattern_and_path()?;
    let path = path.unwrap_or(".");
    let usable = search.glob.is_none_or(|glob| !glob.starts_with('!'))
        && paths.inside(path).is_some_and(|resolved| resolved.exists())
        && regex::Regex::new(pattern).is_ok();
    usable.then(|| search.call(pattern, path))
}

/// `find [dir] -name pattern [-type f]`, which `glob` does.
fn found(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let (directory, expression) = match arguments.split_first() {
        Some((first, rest)) if !first.starts_with('-') => (*first, rest),
        _ => (".", arguments),
    };
    let mut name = None;
    let mut words = expression.iter().copied();
    while let Some(word) = words.next() {
        match (word, words.next()) {
            ("-name", Some(pattern)) if name.is_none() => name = Some(pattern),
            ("-type", Some("f")) => {}
            _ => return None,
        }
    }
    let name = name.filter(|name| !name.contains('/'))?;
    if !paths.directory(directory) {
        return None;
    }
    let mut args = Map::new();
    args.insert("pattern".into(), json!(name));
    if directory != "." {
        args.insert("path".into(), json!(directory));
    }
    Some(Redirection {
        tool: "glob",
        reason: "finding files by name is what the `glob` tool is for: it skips ignored files and lists the newest first.",
        advice: advise("glob", &[Value::Object(args)]),
    })
}

/// `ls -R [dir]`, which `glob` does.
fn listed(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let directory = match arguments {
        ["-R"] => ".",
        ["-R", directory] => directory,
        _ => return None,
    };
    if !paths.directory(directory) {
        return None;
    }
    let mut args = Map::new();
    args.insert("pattern".into(), json!("**/*"));
    if directory != "." {
        args.insert("path".into(), json!(directory));
    }
    Some(Redirection {
        tool: "glob",
        reason: "listing a directory tree is what the `glob` tool is for: it skips ignored files.",
        advice: advise("glob", &[Value::Object(args)]),
    })
}

/// `sed -i` on one file, which `read` and `edit` do.
fn edited(arguments: &[&str], paths: &Paths) -> Option<Redirection> {
    let mut in_place = false;
    let mut script = None;
    let mut operands = Vec::new();
    let mut words = arguments.iter().copied();
    while let Some(word) = words.next() {
        match word {
            "-e" | "--expression" => Search::once(&mut script, words.next())?,
            "-E" | "-r" | "--regexp-extended" => {}
            _ if word.starts_with("-i") || word.starts_with("--in-place") => in_place = true,
            _ if word.starts_with('-') => return None,
            _ => operands.push(word),
        }
    }
    if script.is_none() && !operands.is_empty() {
        operands.remove(0);
    }
    let [file] = operands.as_slice() else {
        return None;
    };
    (in_place && paths.file(file)).then(|| Redirection {
        tool: "edit",
        reason: "editing a file with `sed -i` is what the `edit` tool is for: it checks the lines against a snapshot and shows the change.",
        advice: format!(
            "Call `read` with {} for the lines and their snapshot tag, then `edit`. To change many files at once, sed over several files still runs.",
            json!({ "path": file })
        ),
    })
}

/// `echo`, `printf` or a `cat` here-document sent into one file inside the working directory, which `write` or `edit` does.
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
    let here_doc = match inputs.as_slice() {
        [] => false,
        [input] if input.is_here_doc() => true,
        _ => return None,
    };
    let fed_by_here_doc = program == "cat" && here_doc && arguments.is_empty();
    let printed = program != "cat" && !here_doc;
    let target = output.target.as_ref()?.text.as_str();
    let inside = paths
        .inside(target)
        .is_some_and(|resolved| !resolved.is_dir());
    if !(fed_by_here_doc || printed) || !inside || target.starts_with("/dev/") {
        return None;
    }
    if output.appends() {
        return Some(Redirection {
            tool: "edit",
            reason: "adding to a file through the shell is what the `edit` tool is for.",
            advice: format!(
                "Call `read` with {} for its last lines and snapshot tag, then `edit` with `insert_after` on the last line.",
                json!({ "path": target })
            ),
        });
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

    /// A sandbox holding `src/main.rs`, `notes.txt` and an empty `docs` directory.
    fn sandbox(name: &str) -> Sandbox {
        let dir = scratch(name);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "a\nb\n").unwrap();
        Sandbox::new(&dir).unwrap()
    }

    /// The refusal message for `command`, failing when it would run.
    fn refused(sandbox: &Sandbox, command: &str) -> String {
        match check(command, sandbox) {
            Verdict::Refuse(message) => message,
            other => panic!("`{command}` was not refused: {other:?}"),
        }
    }

    /// Plain reads, searches, listings and edits are turned away with the tool and arguments that do the same.
    #[test]
    fn refuses_plain_uses_with_equivalent_calls() {
        let sandbox = sandbox("intercept-refuse");
        for (command, call) in [
            ("cat src/main.rs", r#"`read` with {"path":"src/main.rs"}"#),
            (
                "cat -n src/main.rs notes.txt",
                r#"{"path":"src/main.rs"}, and again with {"path":"notes.txt"}"#,
            ),
            ("head -n 50 src/main.rs", r#"{"path":"src/main.rs:1-50"}"#),
            ("head -20 src/main.rs", r#"{"path":"src/main.rs:1-20"}"#),
            ("tail notes.txt", r#"{"path":"notes.txt:-10"}"#),
            ("tail -n +5 notes.txt", r#"{"path":"notes.txt:5-"}"#),
            ("less notes.txt", r#"{"path":"notes.txt"}"#),
            (
                "grep -rn 'fn main' src",
                r#"`grep` with {"path":"src","pattern":"fn main"}"#,
            ),
            (
                "grep -ri --include='*.rs' todo",
                r#"{"case_insensitive":true,"glob":"*.rs","pattern":"todo"}"#,
            ),
            (
                "grep -E 'a|b' notes.txt",
                r#"{"path":"notes.txt","pattern":"a|b"}"#,
            ),
            (
                "rg -n 'fn \\w+' -g '*.rs'",
                r#"{"glob":"*.rs","pattern":"fn \\w+"}"#,
            ),
            (
                "find . -name '*.rs' -type f",
                r#"`glob` with {"pattern":"*.rs"}"#,
            ),
            (
                "find src -name main.rs",
                r#"{"path":"src","pattern":"main.rs"}"#,
            ),
            ("ls -R docs", r#"{"path":"docs","pattern":"**/*"}"#),
            (
                "sed -i 's/main/start/' src/main.rs",
                r#"`read` with {"path":"src/main.rs"}"#,
            ),
            (
                "echo hello world > notes.txt",
                r#"`write` with {"content":"hello world\n","path":"notes.txt"}"#,
            ),
            (
                "cat > new.txt <<'EOF'\nhello\nEOF",
                r#"`write` with {"content":"…","path":"new.txt"}"#,
            ),
            ("echo more >> notes.txt", "`insert_after`"),
        ] {
            let message = refused(&sandbox, command);
            assert!(message.contains(call), "`{command}`: {message}");
            assert!(message.starts_with("Not run: "), "{message}");
        }
    }

    /// Anything that needs the shell, or whose meaning differs in the dedicated tool, runs.
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
            "tail -f notes.txt",
            "head -c 100 notes.txt",
            "grep pattern",
            "grep -v x notes.txt",
            "grep -c x notes.txt",
            "grep -A 3 x notes.txt",
            "grep 'a\\|b' notes.txt",
            "grep x src",
            "grep -r x ../elsewhere",
            "rg -F 'a(' src",
            "rg -g '!*.rs' x",
            "find . -name '*.rs' -exec rm {} +",
            "find . -mtime -1",
            "find . -type d -name src",
            "ls -la",
            "ls -R /",
            "sed 's/a/b/' notes.txt",
            "sed -n -i 1p notes.txt",
            "sed -i s/a/b/ notes.txt src/main.rs",
            "echo hi",
            "echo hi > /dev/null",
            "echo hi > ../outside.txt",
            "echo $HOME > notes.txt",
            "echo hi 2> err.txt",
            "sudo cat notes.txt",
            "LC_ALL=C grep -r x .",
            "cat notes.txt > copy.txt",
            "cat < notes.txt",
            "git status",
        ] {
            assert_eq!(check(command, &sandbox), Verdict::Run, "`{command}`");
        }
    }

    /// A replaceable command inside a pipeline or list runs, with a tip naming the tool.
    #[test]
    fn hints_inside_pipelines_and_lists() {
        let sandbox = sandbox("intercept-hint");
        let Verdict::Hint(tip) = check("cat src/main.rs | wc -l", &sandbox) else {
            panic!("expected a tip");
        };
        assert!(tip.contains("`read`") && tip.contains("`cat`"), "{tip}");
        assert!(matches!(
            check("cargo build && grep -rn x src", &sandbox),
            Verdict::Hint(_)
        ));
        assert_eq!(check("wc -l notes.txt | sort", &sandbox), Verdict::Run);
        assert_eq!(check("cargo test | grep -rn x src", &sandbox), Verdict::Run);
    }
}
