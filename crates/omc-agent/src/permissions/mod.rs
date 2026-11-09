//! Permission rules: which tool calls run without asking, which always ask, and which never run.
//!
//! Rules are written the way Claude Code writes them, a tool name with an
//! optional specifier in parentheses: `Bash(cargo test:*)`, `Edit(src/**)`,
//! `Read(./.env)`, `WebFetch(domain:example.com)`, `mcp__github__get_issue`.
//! Claude Code's tool names stand for the crab tools that do the same job,
//! and crab's own names work too. A [`RuleBook`] holds the allow, ask and deny
//! rules in force; [`RuleBook::judge`] says what they make of one call.
//!
//! A shell command is taken apart by [`shell::split`], and an allow rule lets
//! it run only when every simple command in it is allowed, so `cargo test;
//! rm -rf ~` never rides on `Bash(cargo test:*)`. Deny and ask rules match
//! when any simple command does, and look through `sudo`-like wrappers and
//! `sh -c` strings as well; they are a guard rail, not a sandbox.

pub mod shell;

use std::fmt;
use std::path::{Path, PathBuf};

use omc_tools::Subject;

use shell::Simple;

/// Programs that run other commands or code given as their arguments, or destroy files whatever their arguments; an "always allow" for one of these proposes the exact command, never a prefix.
const RUNNERS: &[&str] = &[
    "sudo", "doas", "su", "env", "xargs", "eval", "exec", "command", "builtin", "nohup", "time",
    "timeout", "nice", "ionice", "watch", "strace", "bash", "sh", "zsh", "fish", "dash", "ksh",
    "python", "python3", "node", "deno", "bun", "perl", "ruby", "php", "lua", "ssh", "rm", "dd",
    "mkfs", "chmod", "chown", "find", "awk", "sed", "source", ".", "trap", "parallel",
];

/// Programs whose first argument names a subcommand, so that an "always allow" proposes the program and that subcommand, such as `cargo test`.
const SUBCOMMANDS: &[&str] = &[
    "git",
    "cargo",
    "npm",
    "pnpm",
    "yarn",
    "go",
    "docker",
    "podman",
    "kubectl",
    "make",
    "pip",
    "pip3",
    "uv",
    "poetry",
    "gh",
    "rustup",
    "systemctl",
    "brew",
    "apt",
    "dnf",
    "pacman",
    "dotnet",
    "mvn",
    "gradle",
    "terraform",
    "helm",
    "npx",
    "just",
    "nix",
    "zig",
    "swift",
    "flutter",
    "mix",
    "bundle",
    "rake",
    "composer",
    "jj",
    "hg",
    "svn",
    "cmake",
    "ctest",
    "meson",
    "ninja",
    "tsc",
];

/// Wrappers that run the command after them, looked through when deny and ask rules are matched.
const WRAPPERS: &[&str] = &[
    "sudo", "doas", "env", "nohup", "time", "timeout", "nice", "ionice", "command", "builtin",
    "exec", "xargs", "stdbuf", "chronic",
];

/// Shells whose `-c` string deny and ask rules look into.
const SHELLS: &[&str] = &["bash", "sh", "zsh", "dash", "ksh", "fish"];

/// What a rule does to the calls it matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Effect {
    /// The call runs without asking.
    Allow,
    /// The call is asked about, even when an allow rule matches it.
    Ask,
    /// The call never runs, in yolo mode too.
    Deny,
}

impl Effect {
    /// The effect's name, as the `permissions` object of a settings file lists it: `allow`, `ask` or `deny`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        }
    }
}

/// One permission rule: a tool, and what in its calls it is about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    /// The tool, by Claude Code's name, crab's name, or an MCP name such as `mcp__server` or `mcp__server__tool`.
    pub tool: String,
    /// What the rule is about in the tool's calls, the text in the parentheses; nothing for every call of the tool.
    pub spec: Option<String>,
    /// The folder a path written as `/path` is relative to, the folder of the settings file the rule came from; the project's root when there is none.
    pub base: Option<PathBuf>,
}

impl Rule {
    /// The rule written as `text`, such as `Bash(cargo test:*)` or `WebSearch`.
    ///
    /// `Tool()` and `Tool(*)` are about every call of the tool, as `Tool` is.
    ///
    /// # Errors
    ///
    /// Returns why, when `text` is empty, the tool name holds characters a tool name cannot, or a parenthesis is not closed at the end.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (tool, spec) = match text.find('(') {
            Some(open) => {
                let Some(inner) = text[open + 1..].strip_suffix(')') else {
                    return Err(format!(
                        "{text:?} opens a parenthesis it does not close at its end"
                    ));
                };
                let inner = inner.trim();
                let spec = (!inner.is_empty() && inner != "*").then(|| inner.to_owned());
                (text[..open].trim(), spec)
            }
            None => (text, None),
        };
        if tool.is_empty() {
            return Err(format!("{text:?} names no tool"));
        }
        if !tool
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '*' | '.'))
        {
            return Err(format!("{tool:?} is not a tool name"));
        }
        Ok(Self {
            tool: tool.to_owned(),
            spec,
            base: None,
        })
    }

    /// The same rule, with `/path` paths relative to `base`.
    pub fn based(mut self, base: impl Into<PathBuf>) -> Self {
        self.base = Some(base.into());
        self
    }

    /// Whether the rule is about the crab tool `tool` whose call has `subject`.
    fn names(&self, tool: &str, subject: &Subject) -> bool {
        if self.tool == tool {
            return true;
        }
        if tool.starts_with("mcp__") {
            return self.tool.starts_with("mcp__")
                && (tool
                    .strip_prefix(self.tool.as_str())
                    .is_some_and(|rest| rest.starts_with("__"))
                    || (self.tool.contains('*') && glob_segment(&self.tool, tool)));
        }
        claude_names(tool, subject).contains(&self.tool.as_str())
    }
}

impl fmt::Display for Rule {
    /// Writes the rule as it is parsed: `Tool` or `Tool(spec)`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.spec {
            Some(spec) => write!(f, "{}({spec})", self.tool),
            None => f.write_str(&self.tool),
        }
    }
}

/// The names Claude Code's rules give the crab tool `tool` when its call has `subject`.
fn claude_names(tool: &str, subject: &Subject) -> &'static [&'static str] {
    match tool {
        "bash" => &["Bash"],
        "edit" => &["Edit", "MultiEdit"],
        "write" => &["Write", "Edit"],
        "ast_edit" => &["Edit"],
        "read" if matches!(subject, Subject::Site(_)) => &["WebFetch"],
        "read" => &["Read"],
        "grep" => &["Grep"],
        "glob" => &["Glob"],
        "web_search" => &["WebSearch"],
        "task" => &["Task", "Agent"],
        "todo" => &["TodoWrite"],
        "ask" => &["AskUserQuestion"],
        "exit_plan_mode" => &["ExitPlanMode"],
        "skill" => &["Skill"],
        _ => &[],
    }
}

/// The name a proposed rule gives the crab tool `tool` whose call has `subject`: the Claude Code name that covers every tool doing the same job, or crab's own.
fn proposed_name<'a>(tool: &'a str, subject: &Subject) -> &'a str {
    match tool {
        "edit" | "write" | "ast_edit" => "Edit",
        _ => claude_names(tool, subject).first().copied().unwrap_or(tool),
    }
}

/// The folders rules are read against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Places {
    /// The folder the agent works in, which `./path` and bare paths are relative to.
    pub directory: PathBuf,
    /// The project's root, which `/path` in a rule with no folder of its own is relative to.
    pub root: PathBuf,
    /// The reader's home folder, which `~/path` is relative to.
    pub home: Option<PathBuf>,
}

/// What the rules make of one call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Judgement {
    /// A deny rule matches; the call never runs.
    Denied(Rule),
    /// An ask rule matches; the reader is asked, whatever the allow rules say.
    Asked(Rule),
    /// Allow rules cover the whole call.
    Allowed,
    /// No rule decides.
    Open,
}

/// The rules in force, with the folders they are read against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RuleBook {
    /// Rules that let calls run without asking.
    pub allow: Vec<Rule>,
    /// Rules that make calls ask, whatever the allow rules say.
    pub ask: Vec<Rule>,
    /// Rules that stop calls, in yolo mode too.
    pub deny: Vec<Rule>,
    /// The folders paths in rules are read against.
    pub places: Places,
}

impl RuleBook {
    /// What the rules make of a call of `tool` with `subject`, with `extra` allow rules besides the book's own: deny beats ask, and ask beats allow.
    pub fn judge(&self, tool: &str, subject: &Subject, extra: &[Rule]) -> Judgement {
        if let Some(rule) = self
            .deny
            .iter()
            .find(|rule| self.touches(rule, tool, subject))
        {
            return Judgement::Denied(rule.clone());
        }
        if let Some(rule) = self
            .ask
            .iter()
            .find(|rule| self.touches(rule, tool, subject))
        {
            return Judgement::Asked(rule.clone());
        }
        if self.covered(tool, subject, extra) {
            return Judgement::Allowed;
        }
        Judgement::Open
    }

    /// Whether the allow rules, the book's and `extra`, cover all of a call of `tool` with `subject`: every simple command of a command line, every file, the whole folder, or the site.
    pub fn covered(&self, tool: &str, subject: &Subject, extra: &[Rule]) -> bool {
        let rules: Vec<&Rule> = self
            .allow
            .iter()
            .chain(extra)
            .filter(|rule| rule.names(tool, subject))
            .collect();
        if rules.iter().any(|rule| rule.spec.is_none()) {
            return true;
        }
        match subject {
            Subject::Whole => false,
            Subject::Command(line) => {
                let whole = normalized(line);
                if rules.iter().any(|rule| {
                    rule.spec
                        .as_deref()
                        .is_some_and(|spec| normalized(spec) == whole)
                }) {
                    return true;
                }
                match shell::split(line) {
                    Ok(simples) => {
                        !simples.is_empty()
                            && simples
                                .iter()
                                .all(|simple| rules.iter().any(|rule| allows_simple(rule, simple)))
                    }
                    Err(_) => false,
                }
            }
            Subject::Files(files) => {
                !files.is_empty()
                    && files.iter().all(|file| {
                        rules.iter().any(|rule| {
                            self.pattern(rule)
                                .is_some_and(|pattern| path_matches(&pattern, &slashed(file)))
                        })
                    })
            }
            Subject::Tree(tree) => rules.iter().any(|rule| {
                self.pattern(rule).is_some_and(|pattern| {
                    pattern.ends_with("/**") && path_matches(&pattern, &slashed(tree))
                })
            }),
            Subject::Site(host) => rules.iter().any(|rule| {
                rule.spec
                    .as_deref()
                    .is_some_and(|spec| site_matches(spec, host))
            }),
        }
    }

    /// Whether the deny or ask rule `rule` touches any part of a call of `tool` with `subject`: any simple command, looked through wrappers and `sh -c`, any file, anything below the folder, or the site.
    fn touches(&self, rule: &Rule, tool: &str, subject: &Subject) -> bool {
        if !rule.names(tool, subject) {
            return false;
        }
        let Some(spec) = rule.spec.as_deref() else {
            return true;
        };
        match subject {
            Subject::Whole => false,
            Subject::Command(line) => command_touched(spec, line, 0),
            Subject::Files(files) => self.pattern(rule).is_some_and(|pattern| {
                files
                    .iter()
                    .any(|file| path_matches(&pattern, &slashed(file)))
            }),
            Subject::Tree(tree) => self.pattern(rule).is_some_and(|pattern| {
                let tree = slashed(tree);
                path_matches(&pattern, &tree) || path_could_contain(&pattern, &tree)
            }),
            Subject::Site(host) => site_matches(spec, host),
        }
    }

    /// The absolute path pattern the path specifier of `rule` stands for, with `/` between the parts: `//path` from the root of the file system, `~/path` from the home folder, `/path` from the rule's folder or the project's root, `./path` and paths with a `/` in them from the working folder, and a bare name at any depth below the working folder. A trailing `/` stands for everything below.
    fn pattern(&self, rule: &Rule) -> Option<String> {
        let spec = rule.spec.as_deref()?;
        let places = &self.places;
        let joined = |base: &Path, rest: &str| {
            format!(
                "{}/{}",
                slashed(base).trim_end_matches('/'),
                rest.trim_start_matches('/')
            )
        };
        let mut pattern = if let Some(rest) = spec.strip_prefix("//") {
            format!("/{rest}")
        } else if let Some(rest) = spec.strip_prefix("~/") {
            joined(places.home.as_deref()?, rest)
        } else if spec.starts_with('/') {
            joined(rule.base.as_deref().unwrap_or(&places.root), spec)
        } else if let Some(rest) = spec.strip_prefix("./") {
            joined(&places.directory, rest)
        } else if spec.trim_end_matches('/').contains('/') {
            joined(&places.directory, spec)
        } else {
            joined(&places.directory, &format!("**/{spec}"))
        };
        if pattern.ends_with('/') {
            pattern.push_str("**");
        }
        Some(pattern)
    }

    /// The allow rules that would let a call of `tool` with `subject` run without asking from now on, for the part of it the book's rules and `extra` do not cover yet; nothing when no rule can cover it, as for a command line the splitter does not take apart or that writes to a file.
    ///
    /// `everywhere` asks for rules kept for every project, whose paths are written from the root of the file system rather than from the project's root.
    pub fn propose(
        &self,
        tool: &str,
        subject: &Subject,
        extra: &[Rule],
        everywhere: bool,
    ) -> Vec<String> {
        let name = proposed_name(tool, subject);
        let proposed: Option<Vec<String>> = match subject {
            Subject::Whole => Some(vec![name.to_owned()]),
            Subject::Command(line) => self.propose_commands(tool, line, extra),
            Subject::Files(files) => files
                .iter()
                .filter(|file| !self.covered(tool, &Subject::Files(vec![(*file).clone()]), extra))
                .map(|file| {
                    let folder = if file.starts_with(&self.places.root) {
                        file.parent().unwrap_or(file)
                    } else {
                        file
                    };
                    self.path_rule(
                        name,
                        folder,
                        file.starts_with(&self.places.root),
                        everywhere,
                    )
                })
                .collect(),
            Subject::Tree(tree) => self
                .path_rule(name, tree, true, everywhere)
                .map(|rule| vec![rule]),
            Subject::Site(host) => (!host.is_empty() && !host.contains(['/', ':', ' ', '(', ')']))
                .then(|| vec![format!("{name}(domain:{host})")]),
        };
        let proposed = proposed.unwrap_or_default();
        let mut unique = Vec::new();
        for rule in proposed {
            if !unique.contains(&rule) {
                unique.push(rule);
            }
        }
        let parsed: Vec<Rule> = unique
            .iter()
            .filter_map(|text| Rule::parse(text).ok())
            .chain(extra.iter().cloned())
            .collect();
        if unique.is_empty() || !self.covered(tool, subject, &parsed) {
            return Vec::new();
        }
        unique
    }

    /// The command rules for the simple commands of `line` that the book's allow rules and `extra` do not cover; nothing when the line cannot be taken apart or a command in it writes to a file.
    fn propose_commands(&self, tool: &str, line: &str, extra: &[Rule]) -> Option<Vec<String>> {
        let simples = shell::split(line).ok()?;
        let rules: Vec<&Rule> = self
            .allow
            .iter()
            .chain(extra)
            .filter(|rule| rule.names(tool, &Subject::Command(String::new())))
            .collect();
        let mut proposed = Vec::new();
        for simple in simples {
            if rules.iter().any(|rule| allows_simple(rule, &simple)) {
                continue;
            }
            if simple.writes || !simple.plain_program() {
                return None;
            }
            proposed.push(format!("Bash({})", prefix_of(&simple)));
        }
        Some(proposed)
    }

    /// The rule `Name(path)` for `path`, a folder taken with everything below it when `below` is set: written from the project's root when it is inside the project and `everywhere` is not set, and from the root of the file system otherwise; nothing for a path with characters a pattern would read as more than itself.
    fn path_rule(&self, name: &str, path: &Path, below: bool, everywhere: bool) -> Option<String> {
        let shown = slashed(path);
        if shown.contains(['*', '?', '[', ']', '{', '}', '(', ')']) {
            return None;
        }
        let suffix = if below { "/**" } else { "" };
        let root = slashed(&self.places.root);
        if !everywhere && !root.is_empty() && path.starts_with(&self.places.root) {
            let inside = shown[root.len()..].trim_matches('/');
            let relative = if inside.is_empty() {
                String::new()
            } else {
                format!("/{inside}")
            };
            if relative.is_empty() && !below {
                return None;
            }
            return Some(format!("{name}({relative}{suffix})"));
        }
        Some(format!(
            "{name}(//{}{suffix})",
            shown.trim_start_matches('/')
        ))
    }
}

/// `text` with its runs of white space made single spaces and its ends trimmed.
fn normalized(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `path` with `/` between its parts on every system.
fn slashed(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

/// The prefix rule an "always allow" proposes for `simple`: its program and subcommand followed by `:*`, its program alone followed by `:*`, or, for a program that runs other commands or destroys files, the whole command.
fn prefix_of(simple: &Simple) -> String {
    let program = simple.words[0].as_str();
    let name = program.rsplit('/').next().unwrap_or(program);
    if RUNNERS.contains(&name) || program.contains('/') {
        return simple.text();
    }
    if SUBCOMMANDS.contains(&name)
        && let Some(sub) = simple.words.get(1)
        && sub.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && sub
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
    {
        return format!("{program} {sub}:*");
    }
    format!("{program}:*")
}

/// Whether the allow rule `rule` lets `simple` run: the rule is about every command, or the command writes to no file, names its program plainly and its words match the rule's specifier.
fn allows_simple(rule: &Rule, simple: &Simple) -> bool {
    let Some(spec) = rule.spec.as_deref() else {
        return true;
    };
    !simple.writes && simple.plain_program() && spec_matches(spec, &simple.text())
}

/// Whether the command specifier `spec` matches the command `text`: `prefix:*` matches the prefix alone or followed by a space and anything; a specifier with `*` in it is a pattern where `*` stands for anything; any other specifier matches the command exactly, white space aside.
fn spec_matches(spec: &str, text: &str) -> bool {
    let text = normalized(text);
    if let Some(prefix) = spec.strip_suffix(":*") {
        let prefix = normalized(prefix);
        return text == prefix
            || text
                .strip_prefix(prefix.as_str())
                .is_some_and(|rest| rest.starts_with(' '));
    }
    if spec.contains('*') {
        return wildcard(&normalized(spec), &text);
    }
    normalized(spec) == text
}

/// Whether `text` matches `pattern`, where `*` stands for any run of characters, `/` and spaces included.
fn wildcard(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let (first, rest) = parts.split_first().unwrap_or((&"", &[]));
    let Some(mut remaining) = text.strip_prefix(first) else {
        return false;
    };
    let Some((last, middle)) = rest.split_last() else {
        return remaining.is_empty();
    };
    for part in middle {
        match remaining.find(part) {
            Some(at) => remaining = &remaining[at + part.len()..],
            None => return false,
        }
    }
    remaining.len() >= last.len() && remaining.ends_with(last)
}

/// Whether the deny or ask specifier `spec` touches any simple command of `line`, `depth` shells deep: by its words, by its words with wrappers such as `sudo` and leading assignments taken off, or by the program's file name alone; inside `sh -c` and `eval` strings too. A line the splitter does not take apart is cut at every operator instead, so what follows a keyword is still seen.
fn command_touched(spec: &str, line: &str, depth: usize) -> bool {
    if depth > 4 {
        return false;
    }
    if normalized(spec) == normalized(line) {
        return true;
    }
    let simples = match shell::split(line) {
        Ok(simples) => simples,
        Err(_) => loose_split(line),
    };
    simples.iter().any(|simple| {
        variants(simple).iter().any(|words| {
            let text = words.join(" ");
            spec_matches(spec, &text)
                || nested_line(words).is_some_and(|inner| command_touched(spec, &inner, depth + 1))
        })
    })
}

/// The ways deny and ask rules see `simple`: as written, with leading assignments taken off, and, after a wrapper such as `sudo` or `timeout`, every run of words to the end, since what the wrapper runs starts after options whose values cannot be told apart from it; each of those also with the program named by its file name alone and its words unquoted.
fn variants(simple: &Simple) -> Vec<Vec<String>> {
    let mut seen = vec![simple.words.clone()];
    let mut words: &[String] = &simple.words;
    while words.first().is_some_and(|word| is_assignment(word)) {
        words = &words[1..];
    }
    if words.len() != simple.words.len() {
        seen.push(words.to_vec());
    }
    if let Some(first) = words.first() {
        let first = unquoted(first);
        let name = first.rsplit('/').next().unwrap_or(&first);
        if WRAPPERS.contains(&name) {
            seen.extend((1..words.len()).map(|start| words[start..].to_vec()));
        }
    }
    let mut all = Vec::new();
    for words in seen {
        if words.is_empty() {
            continue;
        }
        let plain: Vec<String> = words.iter().map(|word| unquoted(word)).collect();
        let mut named = plain.clone();
        named[0] = named[0].rsplit('/').next().unwrap_or(&plain[0]).to_owned();
        all.push(words);
        all.push(plain);
        all.push(named);
    }
    all
}

/// The command line a shell `-c` or an `eval` among `words` runs, unquoted, when there is one.
fn nested_line(words: &[String]) -> Option<String> {
    let first = unquoted(&words[0]);
    let name = first.rsplit('/').next().unwrap_or(&first);
    if name == "eval" {
        return Some(
            words[1..]
                .iter()
                .map(|word| unquoted(word))
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    if SHELLS.contains(&name) {
        let at = words.iter().position(|word| {
            word.starts_with('-') && !word.starts_with("--") && word.contains('c')
        })?;
        return words.get(at + 1).map(|word| unquoted(word));
    }
    None
}

/// Whether `word` is a variable assignment, such as `FOO=bar`.
fn is_assignment(word: &str) -> bool {
    word.split_once('=').is_some_and(|(name, _)| {
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && !name.starts_with(|c: char| c.is_ascii_digit())
    })
}

/// `word` with its quotes and backslash escapes taken off, as the shell would hand it to the program.
fn unquoted(word: &str) -> String {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (None, '\'' | '"') => quote = Some(c),
            (Some(open), c) if c == open => quote = None,
            (Some('\''), c) => out.push(c),
            (_, '\\') => {
                if let Some(next) = chars.next() {
                    out.push(next);
                }
            }
            (_, c) => out.push(c),
        }
    }
    out
}

/// The simple commands of a line the splitter does not take apart, found by cutting it at every operator, parenthesis, brace and quote-blind keyword, for deny and ask rules to look at; every piece counts as writing nothing.
fn loose_split(line: &str) -> Vec<Simple> {
    line.split([';', '&', '|', '\n', '(', ')', '{', '}', '`'])
        .flat_map(|piece| piece.split("$("))
        .filter_map(|piece| {
            let mut words: Vec<String> = piece.split_whitespace().map(str::to_owned).collect();
            while words.first().is_some_and(|word| {
                matches!(
                    word.as_str(),
                    "if" | "then" | "else" | "elif" | "do" | "while" | "until" | "!" | "time"
                )
            }) {
                words.remove(0);
            }
            (!words.is_empty()).then_some(Simple {
                words,
                writes: false,
            })
        })
        .collect()
}

/// Whether the site specifier `spec` matches `host`: `domain:example.com` matches that host alone, `domain:*.example.com` its subdomains and itself.
fn site_matches(spec: &str, host: &str) -> bool {
    let Some(domain) = spec.strip_prefix("domain:") else {
        return false;
    };
    let domain = domain.trim().to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    match domain.strip_prefix("*.") {
        Some(parent) => host == parent || host.ends_with(&format!(".{parent}")),
        None => host == domain,
    }
}

/// Whether the absolute `path` matches `pattern`, both with `/` between their parts: `**` stands for any number of parts, none included, and in a part `*` stands for any run of characters and `?` for one.
fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    parts_match(&pattern, &path, false)
}

/// Whether something below the folder `path` could match `pattern`: `path` matches the start of `pattern` and part of the pattern is left over.
fn path_could_contain(pattern: &str, path: &str) -> bool {
    let pattern: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let path: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    parts_match(&pattern, &path, true)
}

/// Whether the parts `path` match the parts `pattern`; with `prefix` set, also when the path runs out while pattern parts are left.
fn parts_match(pattern: &[&str], path: &[&str], prefix: bool) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => {
            (0..=path.len()).any(|skip| parts_match(rest, &path[skip..], prefix))
                || (prefix && path.is_empty())
        }
        Some((first, rest)) => match path.split_first() {
            None => prefix,
            Some((part, more)) => glob_segment(first, part) && parts_match(rest, more, prefix),
        },
    }
}

/// Whether one part `text` matches the part pattern `pattern`, where `*` stands for any run of characters and `?` for one.
fn glob_segment(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == '?' || pattern[p] == text[t]) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((sp, st)) = star {
            p = sp + 1;
            t = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

#[cfg(test)]
mod shell_tests;

#[cfg(test)]
mod tests;
