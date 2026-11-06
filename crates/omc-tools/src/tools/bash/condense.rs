//! Condensing the output of noisy commands before the model reads it.
//!
//! Which lines go is declared in rule files, one per family of tools, under `rules/` beside this module. A filter names the programs, and optionally the subcommands, whose output it condenses, and can drop lines, collapse runs of lines into one line that counts them, and fold repeats of the same line. Before any rule, colour and cursor codes are stripped and a line rewritten in place with carriage returns keeps only what was written last.
//!
//! Lines that report an error or a warning are never dropped or collapsed, whatever the rules say. Only what the model is shown is condensed; the stream is kept whole elsewhere.

use std::sync::LazyLock;

use regex::{Regex, RegexSet};
use serde::Deserialize;

use super::shell::{self, Word, program_name};

/// Every rule file, by the family of tools it covers.
const RULE_FILES: [(&str, &str); 9] = [
    ("git", include_str!("rules/git.toml")),
    ("cargo", include_str!("rules/cargo.toml")),
    ("node", include_str!("rules/node.toml")),
    ("make", include_str!("rules/make.toml")),
    ("cc", include_str!("rules/cc.toml")),
    ("go", include_str!("rules/go.toml")),
    ("pytest", include_str!("rules/pytest.toml")),
    ("docker", include_str!("rules/docker.toml")),
    ("apt", include_str!("rules/apt.toml")),
];

/// Lines that report an error, a warning or a failure, which are always kept.
const PROTECTED: &str = r"(?i)\berrors?\b|\bwarn(ings?)?\b|\bfatal\b|\bfail(s|ed|ures?)?\b|\bpanic(ked|s)?\b|\bexceptions?\b|\btraceback\b|\bdenied\b|\bconflicts?\b|\bcannot\b|\bcan't\b|\bunable\b|\babort(ed)?\b|\bERR!|^[EW]: ";

/// Colour, cursor and terminal-title escape sequences.
const ANSI: &str = r"\x1b\[[0-?]*[ -/]*[@-~]|\x1b\][^\x07\x1b]*(\x07|\x1b\\)|\x1b[@-Z\\-_]";

/// Longest unfinished line held before it is handled as a line of its own.
const MAX_PENDING_BYTES: usize = 64 * 1024;

/// The compiled [`PROTECTED`] pattern.
static PROTECTED_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(PROTECTED).expect("the protected-line pattern is valid"));

/// The compiled [`ANSI`] pattern.
static ANSI_CODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(ANSI).expect("the escape-sequence pattern is valid"));

/// Every filter of every rule file, compiled once.
static FILTERS: LazyLock<Vec<Filter>> = LazyLock::new(|| {
    RULE_FILES
        .iter()
        .flat_map(|(family, text)| load(family, text))
        .collect()
});

/// A rule file as written.
#[derive(Deserialize)]
struct RuleFile {
    /// Its filters.
    #[serde(default)]
    filter: Vec<FilterSpec>,
}

/// One filter as written in a rule file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FilterSpec {
    /// A name for it, for tests and messages.
    name: String,
    /// The programs whose output it condenses, by name without a directory.
    programs: Vec<String>,
    /// The subcommands it is limited to, `""` standing for none at all; any subcommand when empty.
    #[serde(default)]
    subcommands: Vec<String>,
    /// Options of the program, before the subcommand, that take the word after them as their value.
    #[serde(default)]
    value_options: Vec<String>,
    /// Patterns of lines kept even when another rule would remove them.
    #[serde(default)]
    keep: Vec<String>,
    /// Patterns of lines removed.
    #[serde(default)]
    drop: Vec<String>,
    /// Patterns of lines that, in runs, are replaced by one line counting them.
    #[serde(default)]
    collapse: Vec<CollapseSpec>,
    /// Whether a line repeated straight after itself is folded into a count.
    #[serde(default = "yes")]
    dedupe: bool,
}

/// A collapse rule as written.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CollapseSpec {
    /// Lines it collapses.
    pattern: String,
    /// What the summary line calls them, as in "… 143 Compiling lines".
    label: String,
}

/// The default of [`FilterSpec::dedupe`].
fn yes() -> bool {
    true
}

/// Compiles the filters of the rule file `text`, of the family `family`.
///
/// # Panics
///
/// Panics when the file or one of its patterns is malformed; the files are built in, and their tests load every one.
fn load(family: &str, text: &str) -> Vec<Filter> {
    let file: RuleFile = toml::from_str(text)
        .unwrap_or_else(|error| panic!("rule file {family}.toml is malformed: {error}"));
    file.filter
        .into_iter()
        .map(|spec| {
            Filter::compile(spec).unwrap_or_else(|error| panic!("rule file {family}.toml: {error}"))
        })
        .collect()
}

/// A compiled filter: which commands it applies to and what it does to their lines.
pub(super) struct Filter {
    /// Its name, which tests check filters are picked by.
    #[cfg(test)]
    name: String,
    /// The programs it applies to.
    programs: Vec<String>,
    /// The subcommands it is limited to; any when empty.
    subcommands: Vec<String>,
    /// Program options that take a value, skipped when looking for the subcommand.
    value_options: Vec<String>,
    /// Lines kept whatever other rules say.
    keep: RegexSet,
    /// Lines removed.
    drop: RegexSet,
    /// Lines collapsed in runs, with what each pattern's lines are called.
    collapse: Vec<(Regex, String)>,
    /// Whether repeats of a line are folded.
    dedupe: bool,
}

impl Filter {
    /// Compiles `spec`.
    ///
    /// # Errors
    ///
    /// Fails, naming the filter, when one of its patterns is not a valid regular expression.
    fn compile(spec: FilterSpec) -> Result<Self, String> {
        let invalid = |error: regex::Error| format!("filter {}: {error}", spec.name);
        let collapse = spec
            .collapse
            .iter()
            .map(|rule| {
                Ok((
                    Regex::new(&rule.pattern).map_err(invalid)?,
                    rule.label.clone(),
                ))
            })
            .collect::<Result<_, String>>()?;
        Ok(Self {
            keep: RegexSet::new(&spec.keep).map_err(invalid)?,
            drop: RegexSet::new(&spec.drop).map_err(invalid)?,
            collapse,
            #[cfg(test)]
            name: spec.name,
            programs: spec.programs,
            subcommands: spec.subcommands,
            value_options: spec.value_options,
            dedupe: spec.dedupe,
        })
    }

    /// Whether the filter applies to a command run as `invocation`, the program first.
    fn applies_to(&self, invocation: &[Word]) -> bool {
        let Some(program) = invocation.first() else {
            return false;
        };
        if !self
            .programs
            .iter()
            .any(|name| name == program_name(program))
        {
            return false;
        }
        self.subcommands.is_empty()
            || self
                .subcommands
                .iter()
                .any(|name| name == self.subcommand(&invocation[1..]))
    }

    /// The first word of `arguments` that is not an option or an option's value, or `""` when there is none.
    fn subcommand<'a>(&self, arguments: &'a [Word]) -> &'a str {
        let mut words = arguments.iter();
        while let Some(word) = words.next() {
            let text = word.text.as_str();
            if self.value_options.iter().any(|option| option == text) {
                words.next();
            } else if !text.starts_with('-') && !text.starts_with('+') {
                return text;
            }
        }
        ""
    }
}

/// The filters that apply to what a command line prints.
#[derive(Clone)]
pub(super) struct Rules {
    /// The filters, in rule-file order.
    filters: Vec<&'static Filter>,
}

impl Rules {
    /// The names of the filters, for tests and messages.
    #[cfg(test)]
    pub(super) fn names(&self) -> Vec<&str> {
        self.filters
            .iter()
            .map(|filter| filter.name.as_str())
            .collect()
    }

    /// Whether `line` must be kept.
    fn protects(&self, line: &str) -> bool {
        PROTECTED_LINE.is_match(line)
            || self.filters.iter().any(|filter| filter.keep.is_match(line))
    }

    /// Whether `line` is removed.
    fn drops(&self, line: &str) -> bool {
        self.filters.iter().any(|filter| filter.drop.is_match(line))
    }

    /// What lines like `line` are called when they are collapsed, when they are.
    fn collapses(&self, line: &str) -> Option<&'static str> {
        self.filters.iter().copied().find_map(|filter| {
            filter
                .collapse
                .iter()
                .find(|(pattern, _)| pattern.is_match(line))
                .map(|(_, label)| label.as_str())
        })
    }

    /// Whether repeats of a line are folded.
    fn dedupes(&self) -> bool {
        self.filters.iter().any(|filter| filter.dedupe)
    }
}

/// The rules for the output of `command`, or nothing when no filter applies to it.
///
/// Only the last command of the last pipeline is looked at, since that is the command whose output the line is run for: `cd x && cargo build` is condensed as cargo output, while `cargo build && ./target/debug/app`, whose output is mostly the app's, and `cargo test | grep foo` are left alone.
pub(super) fn rules_for(command: &str) -> Option<Rules> {
    let line = shell::parse(command);
    let invocation = line.pipelines.last()?.stages.last()?.invocation();
    let filters: Vec<&'static Filter> = FILTERS
        .iter()
        .filter(|filter| filter.applies_to(invocation))
        .collect();
    (!filters.is_empty()).then_some(Rules { filters })
}

/// What a terminal would show of `line` once its carriage returns have sent the cursor back: the last non-empty part, byte for byte, a carriage return ending the line aside.
fn last_written(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.rsplit('\r')
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .to_owned()
}

/// Lines collapsed so far in the current run.
struct Run {
    /// The first line of the run, shown as it is when the run stays one line long.
    first: String,
    /// How many lines of each kind, in the order the kinds first appeared.
    counts: Vec<(&'static str, usize)>,
}

impl Run {
    /// The line that stands for the run: its only line, or a count of its lines by kind.
    fn summary(self) -> String {
        let total: usize = self.counts.iter().map(|(_, count)| count).sum();
        match self.counts.as_slice() {
            _ if total == 1 => self.first,
            [(label, count)] => format!("… {count} {label} lines"),
            counts => {
                let kinds: Vec<String> = counts
                    .iter()
                    .map(|(label, count)| format!("{count} {label}"))
                    .collect();
                format!("… {total} lines: {}", kinds.join(", "))
            }
        }
    }
}

/// Condenses one output stream as it arrives, line by line.
pub(in crate::tools) struct Condenser {
    /// The rules applied.
    rules: Rules,
    /// The unfinished last line.
    pending: Vec<u8>,
    /// The run of collapsed lines in progress.
    run: Option<Run>,
    /// The last line shown and how many more times it came straight after itself.
    repeat: Option<(String, usize)>,
    /// Condensed text not yet taken.
    out: String,
    /// Lines read.
    lines_in: usize,
    /// Lines shown.
    lines_out: usize,
    /// Whether a line was left out: dropped, collapsed into a count, or folded into a count of its repeats. Cleaning up colour codes, redraws and repeated blank lines does not count.
    removed: bool,
}

impl Condenser {
    /// A condenser applying `rules`.
    pub(super) fn new(rules: Rules) -> Self {
        Self {
            rules,
            pending: Vec::new(),
            run: None,
            repeat: None,
            out: String::new(),
            lines_in: 0,
            lines_out: 0,
            removed: false,
        }
    }

    /// Takes in `bytes`, the next part of the stream, and returns the condensed text of the lines they finished.
    pub(in crate::tools) fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.pending.drain(..=end).collect();
            self.line(&line[..end]);
        }
        self.forget_overwritten();
        if self.pending.len() > MAX_PENDING_BYTES {
            let line = std::mem::take(&mut self.pending);
            self.line(&line);
        }
        std::mem::take(&mut self.out)
    }

    /// Ends the stream and returns the condensed text still held: the unfinished last line and any run or repeat in progress.
    pub(in crate::tools) fn finish(&mut self) -> String {
        if !self.pending.is_empty() {
            let line = std::mem::take(&mut self.pending);
            self.line(&line);
        }
        self.end_run();
        self.end_repeat();
        std::mem::take(&mut self.out)
    }

    /// How many lines were read.
    pub(in crate::tools) fn lines_in(&self) -> usize {
        self.lines_in
    }

    /// How many lines were shown.
    pub(in crate::tools) fn lines_out(&self) -> usize {
        self.lines_out
    }

    /// Whether a line was left out, so that the condensed text is worth showing and the stream must be kept whole elsewhere.
    pub(in crate::tools) fn removed(&self) -> bool {
        self.removed
    }

    /// Drops what the unfinished line has already overwritten with a carriage return, so that a progress bar redrawn for minutes does not pile up.
    fn forget_overwritten(&mut self) {
        let before_last = self.pending.len().saturating_sub(1);
        if let Some(cut) = self.pending[..before_last]
            .iter()
            .rposition(|&byte| byte == b'\r')
        {
            self.pending.drain(..=cut);
        }
    }

    /// Handles one line of raw output, without its newline.
    fn line(&mut self, raw: &[u8]) {
        self.lines_in += 1;
        let text = String::from_utf8_lossy(raw);
        let text = ANSI_CODE.replace_all(&text, "");
        let text = last_written(&text);
        if self.rules.protects(&text) {
            self.keep(text);
        } else if self.rules.drops(&text) {
            self.removed = true;
        } else if let Some(label) = self.rules.collapses(&text) {
            self.collapse(text, label);
        } else {
            self.keep(text);
        }
    }

    /// Adds `line` to the run of collapsed lines, calling it `label`.
    fn collapse(&mut self, line: String, label: &'static str) {
        if self.run.is_none() {
            self.end_repeat();
        }
        let run = self.run.get_or_insert_with(|| Run {
            first: line,
            counts: Vec::new(),
        });
        match run.counts.iter_mut().find(|(known, _)| *known == label) {
            Some((_, count)) => *count += 1,
            None => run.counts.push((label, 1)),
        }
    }

    /// Shows `line`, folding it into a count when it repeats the line shown before, or leaving it out when both are blank.
    fn keep(&mut self, line: String) {
        self.end_run();
        if self.rules.dedupes()
            && let Some((last, count)) = &mut self.repeat
            && *last == line
        {
            if !line.is_empty() {
                *count += 1;
            }
            return;
        }
        self.end_repeat();
        self.write(&line);
        self.repeat = Some((line, 0));
    }

    /// Ends the run of collapsed lines in progress, showing the line that stands for it.
    fn end_run(&mut self) {
        let Some(run) = self.run.take() else { return };
        if run.counts.iter().map(|(_, count)| count).sum::<usize>() > 1 {
            self.removed = true;
        }
        let summary = run.summary();
        self.write(&summary);
    }

    /// Ends the repeat in progress, showing the repeated line once more or a count of its repeats.
    fn end_repeat(&mut self) {
        match self.repeat.take() {
            Some((line, 1)) => self.write(&line),
            Some((_, count)) if count > 1 => {
                self.removed = true;
                self.write(&format!("… the line above repeated {count} more times"));
            }
            _ => {}
        }
    }

    /// Adds `line` to the condensed text.
    fn write(&mut self, line: &str) {
        self.out.push_str(line);
        self.out.push('\n');
        self.lines_out += 1;
    }
}

#[cfg(test)]
/// Tests for condensing output, including the cases written in the rule files.
mod tests {
    use super::*;

    /// The test cases of a rule file.
    #[derive(Deserialize)]
    struct Cases {
        /// The cases.
        #[serde(default)]
        test: Vec<Case>,
    }

    /// One test case: a command, what it printed and what the model should be shown.
    #[derive(Deserialize)]
    struct Case {
        /// The command line.
        command: String,
        /// What it printed.
        input: String,
        /// The condensed text.
        expected: String,
    }

    /// Condenses `input` as the output of `command`, fed in small pieces to cross line boundaries.
    fn condense(command: &str, input: &str) -> Option<String> {
        let mut condenser = Condenser::new(rules_for(command)?);
        let mut out = String::new();
        for piece in input.as_bytes().chunks(7) {
            out.push_str(&condenser.push(piece));
        }
        out.push_str(&condenser.finish());
        Some(out)
    }

    /// Every rule file loads, has tests, and condenses each test's input to what the test expects.
    #[test]
    fn rule_files_pass_their_own_tests() {
        assert!(FILTERS.len() >= RULE_FILES.len());
        for (family, text) in RULE_FILES {
            let cases: Cases = toml::from_str(text).unwrap();
            assert!(!cases.test.is_empty(), "{family}.toml has no tests");
            for case in cases.test {
                let condensed = condense(&case.command, &case.input)
                    .unwrap_or_else(|| panic!("{family}.toml: no filter for `{}`", case.command));
                assert_eq!(
                    condensed, case.expected,
                    "{family}.toml: `{}`",
                    case.command
                );
            }
        }
    }

    /// Filters are picked by program and subcommand from the last command of the last pipeline only, past `cd`, assignments and `sudo`; logs and diffs are never condensed.
    #[test]
    fn picks_filters_by_command() {
        let names = |command: &str| rules_for(command).map(|rules| rules.names().join(","));
        assert_eq!(names("cargo build --release").as_deref(), Some("cargo"));
        assert_eq!(
            names("cd crates/x && RUST_LOG=1 cargo +nightly test").as_deref(),
            Some("cargo")
        );
        assert_eq!(
            names("sudo apt-get install -y curl").as_deref(),
            Some("apt")
        );
        assert_eq!(names("git -C repo status").as_deref(), Some("git status"));
        assert_eq!(names("git pull && npm ci").as_deref(), Some("npm"));
        assert_eq!(names("cargo build && ./target/debug/app"), None);
        assert_eq!(names("make; ./run-tests"), None);
        assert_eq!(names("cargo test 2>&1 | grep FAILED"), None);
        for command in ["git diff", "git log -p", "git show HEAD", "git -C repo log"] {
            assert_eq!(names(command), None, "{command}");
        }
        assert_eq!(names("ls -la"), None);
    }

    /// Lines that are kept stay byte for byte as they were, trailing spaces and tabs included.
    #[test]
    fn keeps_lines_byte_exact() {
        let input = "   Compiling a v1.0.0\n   Compiling b v1.0.0\nkept with spaces   \n\tand a tab\t\n  |\n";
        assert_eq!(
            condense("cargo build", input).unwrap(),
            "… 2 Compiling lines\nkept with spaces   \n\tand a tab\t\n  |\n"
        );
    }

    /// Stripping colour codes, settling redraws and folding blank lines leave nothing meaningful out, so the output is not marked as condensed.
    #[test]
    fn cleanup_alone_is_not_condensing() {
        let mut condenser = Condenser::new(rules_for("make").unwrap());
        condenser.push(b"\x1b[1mbold\x1b[0m\n 10%\r 50%\r100%\n\n\n\nend\n");
        condenser.push(b"0%\r50%\r");
        condenser.finish();
        assert!(!condenser.removed());
    }

    /// Errors and warnings are kept even where a rule would remove them, and a run of collapsed lines is split around them.
    #[test]
    fn never_removes_errors_or_warnings() {
        let input = "   Compiling a v1.0.0\n   Compiling b v1.0.0\nwarning: unused variable: `x`\n   Compiling c v1.0.0\n   Compiling error-chain v0.12.0\n   Compiling d v1.0.0\nerror[E0425]: cannot find value `y`\n";
        let condensed = condense("cargo build", input).unwrap();
        assert_eq!(
            condensed,
            "… 2 Compiling lines\nwarning: unused variable: `x`\n   Compiling c v1.0.0\n   Compiling error-chain v0.12.0\n   Compiling d v1.0.0\nerror[E0425]: cannot find value `y`\n"
        );
    }

    /// Colour codes are stripped, a line redrawn with carriage returns keeps what was drawn last, and repeated lines are counted.
    #[test]
    fn cleans_redraws_colour_and_repeats() {
        let mut condenser = Condenser::new(rules_for("make").unwrap());
        let mut out = condenser.push(b"\x1b[32mok\x1b[0m\n 10%\r 50%\r100% done\n");
        out.push_str(&condenser.push(b"same\nsame\nsame\nsame\nlast"));
        out.push_str(&condenser.finish());
        assert_eq!(
            out,
            "ok\n100% done\nsame\n… the line above repeated 3 more times\nlast\n"
        );
        assert!(condenser.removed());
        assert_eq!((condenser.lines_in(), condenser.lines_out()), (7, 5));
    }

    /// A progress bar redrawn without newlines does not pile up while it runs.
    #[test]
    fn forgets_overwritten_progress() {
        let mut condenser = Condenser::new(rules_for("make").unwrap());
        for percent in 0..10_000 {
            condenser.push(format!("{percent}%\r").as_bytes());
        }
        assert!(condenser.pending.len() < 16);
        assert_eq!(condenser.finish(), "9999%\n");
    }

    /// Output a filter leaves alone is not marked as condensed.
    #[test]
    fn untouched_output_is_not_marked() {
        let mut condenser = Condenser::new(rules_for("cargo build").unwrap());
        condenser.push(b"    Finished `dev` profile in 0.1s\n");
        condenser.finish();
        assert!(!condenser.removed());
    }
}
