//! The read-only mode of the `bash` tool, for agents marked read-only.
//!
//! Where the bubblewrap sandbox (`bwrap`) runs, every command runs inside a mount namespace in which the whole filesystem is bound read-only, with private scratch space for `/tmp`, `/var/tmp`, `/dev/shm` and `/run`, the network as it is and the working directory unchanged ([`Mode::Sandboxed`]). Elsewhere a command runs only when [`vet`] finds it made of programs that only look ([`Mode::Vetted`]).

use std::path::Path;
use std::sync::OnceLock;

use tokio::process::Command;

use super::shell::{self, Redirect, Simple, Word};

/// How the read-only mode keeps commands from changing anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    /// Any command runs, inside a sandbox where the filesystem is read-only.
    Sandboxed,
    /// Only commands made of programs that only look run, checked before they start.
    Vetted,
}

/// What the model is told about the shell in sandboxed read-only mode.
pub(super) const SANDBOXED_NOTE: &str = include_str!("../../prompts/bash_read_only_sandboxed.md");

/// What the model is told about the shell in vetted read-only mode.
pub(super) const VETTED_NOTE: &str = include_str!("../../prompts/bash_read_only_vetted.md");

/// Programs that only look, whatever their arguments, apart from the options [`refused_option`] names for some of them.
const LOOKING_PROGRAMS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "sort",
    "uniq",
    "cut",
    "tr",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "find",
    "jq",
    "stat",
    "file",
    "du",
    "df",
    "date",
    "echo",
    "printf",
    "pwd",
    "env",
    "which",
    "realpath",
    "basename",
    "dirname",
    "readlink",
    "nl",
    "tac",
    "rev",
    "paste",
    "diff",
    "cmp",
    "comm",
    "seq",
    "md5sum",
    "sha1sum",
    "sha256sum",
    "uname",
    "whoami",
    "id",
    "true",
    "false",
    "git",
];

/// Subcommands of `git` that only look, whatever their arguments.
const LOOKING_GIT: &[&str] = &[
    "status",
    "log",
    "diff",
    "show",
    "blame",
    "rev-parse",
    "ls-files",
    "ls-tree",
    "cat-file",
    "describe",
    "shortlog",
    "grep",
    "rev-list",
    "merge-base",
    "show-ref",
    "for-each-ref",
    "name-rev",
    "count-objects",
];

/// Options of `git branch` and `git tag` that change a branch or tag.
const GIT_REF_CHANGES: &[&str] = &[
    "-d",
    "-D",
    "-m",
    "-M",
    "-c",
    "-C",
    "-f",
    "-u",
    "-a",
    "-s",
    "-F",
    "-e",
    "--delete",
    "--move",
    "--copy",
    "--force",
    "--set-upstream-to",
    "--unset-upstream",
    "--edit-description",
    "--annotate",
    "--sign",
    "--message",
    "--file",
    "--edit",
];

/// Options of `git config` that change the configuration.
const GIT_CONFIG_CHANGES: &[&str] = &[
    "--add",
    "--unset",
    "--unset-all",
    "--replace-all",
    "--rename-section",
    "--remove-section",
    "--edit",
    "-e",
];

/// Files an output redirection may go to without writing anything.
const HARMLESS_SINKS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

/// Folders the sandbox covers with private, empty scratch space when they exist.
#[cfg(unix)]
const SCRATCH: &[&str] = &["/tmp", "/var/tmp", "/run"];

/// Folders under `/run` bound back into the sandbox read-only after it is covered, so that name lookups keep working.
#[cfg(unix)]
const KEPT_UNDER_RUN: &[&str] = &["/run/systemd/resolve", "/run/resolvconf"];

/// The mode read-only commands run in on this machine: [`Mode::Sandboxed`] when `bwrap` is installed and can make a sandbox here, found out once and remembered, and [`Mode::Vetted`] otherwise.
pub(super) fn mode() -> Mode {
    static MODE: OnceLock<Mode> = OnceLock::new();
    *MODE.get_or_init(|| {
        if sandbox_works() {
            Mode::Sandboxed
        } else {
            Mode::Vetted
        }
    })
}

/// Whether `bwrap` runs `true` in the sandbox [`sandboxed`] builds, which fails where it is missing or the system does not let it make namespaces.
#[cfg(target_os = "linux")]
fn sandbox_works() -> bool {
    std::process::Command::new("bwrap")
        .args(sandbox_args(Path::new("/")))
        .arg("true")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// No sandbox is made outside Linux.
#[cfg(not(target_os = "linux"))]
fn sandbox_works() -> bool {
    false
}

/// The arguments that have `bwrap` bind the whole filesystem read-only, give it fresh `/dev` and `/proc`, cover the [`SCRATCH`] folders and `/dev/shm` with private scratch space, keep the name lookup folders, bind `root` read-only again in case scratch space covered it, and start in `root`; the command to run follows them.
#[cfg(unix)]
fn sandbox_args(root: &Path) -> Vec<String> {
    let mut args: Vec<String> = ["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc"]
        .map(str::to_owned)
        .to_vec();
    for folder in SCRATCH.iter().filter(|folder| Path::new(folder).is_dir()) {
        args.extend(["--tmpfs".to_owned(), (*folder).to_owned()]);
    }
    args.extend(["--tmpfs".to_owned(), "/dev/shm".to_owned()]);
    for folder in KEPT_UNDER_RUN {
        args.extend([
            "--ro-bind-try".to_owned(),
            (*folder).to_owned(),
            (*folder).to_owned(),
        ]);
    }
    let root = root.display().to_string();
    args.extend([
        "--ro-bind".to_owned(),
        root.clone(),
        root.clone(),
        "--chdir".to_owned(),
        root,
        "--setenv".to_owned(),
        "TMPDIR".to_owned(),
        "/tmp".to_owned(),
        "--die-with-parent".to_owned(),
    ]);
    args
}

/// Builds the command that runs `command` with `bash -c` inside the read-only sandbox, starting in `root`, in a process group of its own so that the whole group can be killed.
#[cfg(unix)]
pub(super) fn sandboxed(command: &str, root: &Path) -> Command {
    let mut wrapped = Command::new("bwrap");
    wrapped
        .args(sandbox_args(root))
        .args(["bash", "-c", command])
        .process_group(0);
    wrapped
}

/// Builds the command that runs `command` with `cmd /C`, which has no sandbox; [`mode`] never picks one outside Linux.
#[cfg(windows)]
pub(super) fn sandboxed(command: &str, _root: &Path) -> Command {
    let mut shell = Command::new("cmd");
    shell.arg("/C").arg(command);
    shell
}

/// Checks that `command` only looks: it substitutes no command, uses no shell construct the reading does not follow, starts nothing in the background, sends no output into a file, and every command it runs is a program on the list of those that only look, without the options that would make it change something.
///
/// # Errors
///
/// Returns a message for the model saying that the shell is read-only, what was refused and why.
pub(super) fn vet(command: &str) -> Result<(), String> {
    let refuse = |what: &str, why: &str| {
        Err(format!(
            "the shell is read-only for this agent: {what} was refused because {why}. Only pipelines of programs that only look run here (ls, cat, head, tail, wc, sort, uniq, cut, tr, grep, rg, find, jq, stat, file, du, df, git status/log/diff/show/blame and the like)."
        ))
    };
    if ["`", "$(", "<(", ">("]
        .iter()
        .any(|opening| command.contains(opening))
    {
        return refuse(
            "command substitution",
            "a substituted command could run anything",
        );
    }
    let line = shell::parse(command);
    if line.unusual {
        return refuse(
            "the command line",
            "it uses shell syntax the check does not follow, such as subshells, groups, control flow or unbalanced quotes",
        );
    }
    for pipeline in &line.pipelines {
        if pipeline.background {
            return refuse("`&`", "background runs are not allowed");
        }
        for simple in &pipeline.stages {
            vet_simple(simple).or_else(|(what, why)| refuse(&what, &why))?;
        }
    }
    Ok(())
}

/// Checks one command of a line: its redirections write no file, it sets no variable for the program, and the program is one that only looks, used without the options that change something.
///
/// # Errors
///
/// Returns what was refused and why.
fn vet_simple(simple: &Simple) -> Result<(), (String, String)> {
    if let Some(redirect) = simple.redirects.iter().find(|redirect| writes(redirect)) {
        return Err((
            format!("the redirection `{}`", redirect.operator),
            "output may not be written to a file".to_owned(),
        ));
    }
    let Some(program) = simple.words.first() else {
        return Ok(());
    };
    if program.text.contains('=') {
        return Err((
            format!("`{}`", program.text),
            "setting variables for a command is not allowed".to_owned(),
        ));
    }
    let name = shell::program_name(program);
    if !program.literal || !LOOKING_PROGRAMS.contains(&name) {
        return Err((
            format!("`{}`", program.text),
            "it is not on the list of programs that only look".to_owned(),
        ));
    }
    let args: Vec<&str> = simple.words[1..]
        .iter()
        .map(|word: &Word| word.text.as_str())
        .collect();
    match refused_option(name, &args) {
        Some(why) => Err((format!("`{}`", simple_text(simple)), why)),
        None => Ok(()),
    }
}

/// The words of `simple` joined by spaces, for messages.
fn simple_text(simple: &Simple) -> String {
    simple
        .words
        .iter()
        .map(|word| word.text.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `redirect` writes a file: an output redirection whose target is neither a harmless sink such as `/dev/null` nor a descriptor it duplicates.
fn writes(redirect: &Redirect) -> bool {
    if !redirect.operator.contains('>') {
        return false;
    }
    let target = redirect
        .target
        .as_ref()
        .map(|target| target.text.as_str())
        .unwrap_or_default();
    let duplicated = target.trim_start_matches('&');
    let duplicates = (redirect.operator.ends_with('&') || target.starts_with('&'))
        && (duplicated == "-"
            || (!duplicated.is_empty() && duplicated.chars().all(|c| c.is_ascii_digit())));
    !duplicates && !HARMLESS_SINKS.contains(&target)
}

/// Why `program` with `args` would change something or run another program, or nothing when it only looks.
fn refused_option(program: &str, args: &[&str]) -> Option<String> {
    let found = match program {
        "find" => args
            .iter()
            .find(|arg| {
                matches!(
                    **arg,
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprint"
                        | "-fprint0"
                        | "-fprintf"
                        | "-fls"
                )
            })
            .map(|arg| (*arg).to_owned()),
        "sort" => option(args, &['o'], &["--output", "--compress-program"]),
        "rg" => option(args, &[], &["--pre"]),
        "date" => option(args, &['s'], &["--set"]),
        "file" => option(args, &['C'], &["--compile"]),
        "uniq" => (args.iter().filter(|arg| !arg.starts_with('-')).count() > 1)
            .then(|| "an output file".to_owned()),
        "env" => args
            .iter()
            .find(|arg| !arg.starts_with('-') && !arg.contains('='))
            .map(|arg| format!("running `{arg}`")),
        "git" => return git(args),
        _ => None,
    };
    found.map(|what| format!("`{program}` with {what} can change something or run another program"))
}

/// The first of `args` that is one of the options `short`, alone or in a cluster such as `-uo`, or `long`, whole, with a value after `=`, or abbreviated to three letters or more; nothing when none is.
fn option(args: &[&str], short: &[char], long: &[&str]) -> Option<String> {
    for arg in args {
        if *arg == "--" {
            break;
        }
        let hit = if let Some(name) = arg.strip_prefix("--") {
            let name = format!("--{}", name.split('=').next().unwrap_or_default());
            name.len() > 4 && long.iter().any(|option| option.starts_with(&name))
        } else if let Some(cluster) = arg.strip_prefix('-') {
            cluster.chars().any(|c| short.contains(&c))
        } else {
            false
        };
        if hit {
            return Some(format!("`{arg}`"));
        }
    }
    None
}

/// Why the `git` call with `args` would change something or run another program, or nothing when it only looks: the global options `-C`, `--git-dir`, `--work-tree`, `--no-pager` and `--paginate` are passed over, `-c` and every other global option refused, and the subcommand must be on [`LOOKING_GIT`] or be `branch`, `tag`, `remote`, `stash` or `config` used only to list.
fn git(args: &[&str]) -> Option<String> {
    let mut rest = args;
    while let Some(first) = rest.first() {
        rest = match *first {
            "-C" | "--git-dir" | "--work-tree" => rest.get(2..).unwrap_or_default(),
            "--no-pager" | "--paginate" | "-p" | "-P" => &rest[1..],
            flag if flag.starts_with("--git-dir=") || flag.starts_with("--work-tree=") => {
                &rest[1..]
            }
            flag if flag.starts_with('-') => {
                return Some(format!(
                    "`git {flag}` can change how git behaves, so only `-C`, `--git-dir`, `--work-tree` and `--no-pager` are allowed before the subcommand"
                ));
            }
            _ => break,
        };
    }
    let (subcommand, rest) = rest.split_first()?;
    if let Some(flag) = rest.iter().find(|arg| {
        arg.starts_with("--output")
            || **arg == "--ext-diff"
            || **arg == "-O"
            || arg.starts_with("--open-files-in-pager")
    }) {
        return Some(format!(
            "`git {subcommand} {flag}` can write a file or run another program"
        ));
    }
    let looks = match *subcommand {
        listed if LOOKING_GIT.contains(&listed) => true,
        "branch" | "tag" => lists_refs(rest),
        "remote" => matches!(
            rest.first().copied(),
            None | Some("-v" | "--verbose" | "get-url")
        ),
        "stash" => matches!(rest.first().copied(), Some("list" | "show")),
        "config" => {
            rest.iter().any(|arg| {
                matches!(
                    *arg,
                    "--get" | "--get-all" | "--get-regexp" | "--list" | "-l"
                )
            }) && !rest.iter().any(|arg| GIT_CONFIG_CHANGES.contains(arg))
        }
        _ => false,
    };
    (!looks).then(|| {
        format!("`git {subcommand}` is not one of the git subcommands that only look (status, log, diff, show, blame, branch --list, rev-parse and the like)")
    })
}

/// Whether `git branch` or `git tag` with `args` only lists: no arguments, or `--list`, `-l` or `--show-current` among them and none of [`GIT_REF_CHANGES`].
fn lists_refs(args: &[&str]) -> bool {
    let changes = args.iter().any(|arg| {
        let name = arg.split('=').next().unwrap_or_default();
        GIT_REF_CHANGES.contains(&name)
    });
    let lists = args.is_empty()
        || args
            .iter()
            .any(|arg| matches!(*arg, "--list" | "-l" | "--show-current"));
    lists && !changes
}

#[cfg(test)]
/// Tests for the read-only mode.
mod tests {
    use super::*;

    /// Pipelines of programs that only look pass the check.
    #[test]
    fn looking_commands_pass() {
        for command in [
            "ls -la ~/.local/share",
            "cat a.jsonl | jq -r '.type' | sort | uniq -c | sort -rn | head -20",
            "grep -c error log.txt 2>/dev/null",
            "rg foo src 2>&1 | wc -l",
            "find . -name '*.rs' -type f",
            "git status && git log --oneline -5",
            "git -C repo diff HEAD~1 --stat",
            "git branch --list 'feat/*'",
            "git branch",
            "wc -l < input.txt",
            "echo $HOME; pwd",
            "du -sh * | sort -h",
            "env",
        ] {
            assert_eq!(vet(command), Ok(()), "{command}");
        }
    }

    /// Commands that could change something, or run something unchecked, are refused with a message saying the shell is read-only.
    #[test]
    fn changing_commands_are_refused() {
        for command in [
            "echo hi > out.txt",
            "cat a >> b",
            "ls &> listing",
            "rm -rf build",
            "git commit -m x",
            "git branch -D old",
            "git -c core.pager=sh log",
            "git diff --output=patch",
            "find . -name '*.tmp' -delete",
            "find . -exec rm {} \\;",
            "sed -i s/a/b/ file",
            "echo $(rm -rf x)",
            "ls `touch y`",
            "python3 -c 'print(1)'",
            "sudo ls",
            "sort -o sorted data",
            "uniq in out",
            "env rm x",
            "FOO=1 ls",
            "sleep 10 &",
            "(cd x && ls)",
            "cat a | tee b",
        ] {
            let refused = vet(command).expect_err(command);
            assert!(refused.contains("read-only"), "{command}: {refused}");
        }
    }

    /// The message names what was refused.
    #[test]
    fn the_refusal_names_the_command() {
        let refused = vet("ls && rm -rf build").unwrap_err();
        assert!(refused.contains("`rm`"), "{refused}");
        let refused = vet("cat x > y").unwrap_err();
        assert!(refused.contains("`>`"), "{refused}");
    }
}
