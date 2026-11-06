//! A reading of a shell command line good enough to tell which programs it runs, with what words, and where their output goes.
//!
//! It is not a shell parser. Words are split and unquoted the way bash does it, pipelines and command lists are told apart, redirections and here-documents are recognised, and expansions are kept as written but noted. Constructs it does not follow, such as subshells, brace groups, control flow, function definitions and unterminated quotes, mark the line [`CommandLine::unusual`], and callers that act on what they read leave such lines alone.

/// Words that, at the start of a command, open a construct this reading does not follow.
const KEYWORDS: [&str; 20] = [
    "{", "}", "if", "then", "elif", "else", "fi", "for", "while", "until", "do", "done", "case",
    "esac", "function", "select", "[[", "]]", "!", "coproc",
];

/// Most lines of a here-document body skipped before it is treated as unterminated.
const MAX_HERE_DOC_LINES: usize = 1_000_000;

/// A word of a command as the program would receive it, as far as can be told without running the shell.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Word {
    /// The word with quotes and escapes removed, and any expansion left as written.
    pub(super) text: String,
    /// Whether the shell passes the word on as it is: no variable, command substitution, glob, brace or tilde expansion.
    pub(super) literal: bool,
}

/// A redirection of one of a command's streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Redirect {
    /// The operator with any descriptor number in front, such as `>`, `2>>`, `&>`, `<<` or `2>&`.
    pub(super) operator: String,
    /// The file, descriptor or here-document delimiter redirected to, when one was given.
    pub(super) target: Option<Word>,
}

impl Redirect {
    /// Whether it sends standard output into a file, replacing (`>`, `>|`, `1>`) or appending to (`>>`, `1>>`) it; `&>` and duplications such as `>&2` do not count.
    pub(super) fn writes_stdout(&self) -> bool {
        let file = self
            .target
            .as_ref()
            .is_some_and(|target| !target.text.starts_with('&'));
        file && matches!(
            self.operator.as_str(),
            ">" | ">>" | ">|" | "1>" | "1>>" | "1>|"
        )
    }

    /// Whether it appends rather than replaces.
    pub(super) fn appends(&self) -> bool {
        self.operator.ends_with(">>")
    }

    /// Whether it feeds a here-document to the command: `<<` or `<<-`.
    pub(super) fn is_here_doc(&self) -> bool {
        self.operator.ends_with("<<") || self.operator.ends_with("<<-")
    }
}

/// One command: its words and redirections.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Simple {
    /// The words, the program first, in order.
    pub(super) words: Vec<Word>,
    /// The redirections, in order.
    pub(super) redirects: Vec<Redirect>,
}

impl Simple {
    /// The words from the program on, past leading variable assignments and wrappers that run another command, such as `sudo`, `env`, `time`, `nice`, `timeout`, `python -m`, `uv run` and `npx`.
    pub(super) fn invocation(&self) -> &[Word] {
        let mut words = self.words.as_slice();
        loop {
            let after = skip_assignments(words);
            let inner = skip_wrapper(after);
            if inner.len() == words.len() {
                return words;
            }
            words = inner;
        }
    }
}

/// Commands joined by `|`, the output of each feeding the next.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct Pipeline {
    /// The commands, in order.
    pub(super) stages: Vec<Simple>,
}

/// A whole command line: pipelines run one after another, separated by `;`, `&&`, `||`, `&` or newlines.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(super) struct CommandLine {
    /// The pipelines, in order.
    pub(super) pipelines: Vec<Pipeline>,
    /// Whether the line holds something this reading does not follow, so what it read may be wrong.
    pub(super) unusual: bool,
}

/// Reads `command` into its pipelines, commands, words and redirections.
pub(super) fn parse(command: &str) -> CommandLine {
    let mut reader = Reader {
        chars: command.chars().collect(),
        ..Reader::default()
    };
    reader.read();
    reader.line
}

/// The last part of the path `word` names: the program name of `/usr/bin/git` is `git`.
pub(super) fn program_name(word: &Word) -> &str {
    word.text.rsplit('/').next().unwrap_or_default()
}

/// Whether `word` assigns a shell variable, such as `RUST_LOG=debug`.
fn is_assignment(word: &Word) -> bool {
    let Some((name, _)) = word.text.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `words` past any variable assignments at their start.
fn skip_assignments(words: &[Word]) -> &[Word] {
    let count = words.iter().take_while(|word| is_assignment(word)).count();
    &words[count..]
}

/// `words` past a wrapper that runs the command after it, with the wrapper's own options; `words` unchanged when they do not start with one.
fn skip_wrapper(words: &[Word]) -> &[Word] {
    let Some(first) = words.first() else {
        return words;
    };
    let second = words.get(1).map(|word| word.text.as_str());
    match (program_name(first), second) {
        ("sudo" | "doas", _) => skip_options(
            &words[1..],
            &["-u", "-g", "-C", "-D", "-h", "-p", "-r", "-t", "-U", "-R"],
        ),
        ("env", _) => skip_assignments(skip_options(&words[1..], &["-u", "-C", "-S"])),
        ("time" | "nohup" | "command" | "exec" | "stdbuf" | "npx" | "bunx", _) => {
            skip_options(&words[1..], &[])
        }
        ("nice", _) => skip_options(&words[1..], &["-n"]),
        ("timeout", _) => {
            let rest = skip_options(&words[1..], &["-s", "-k"]);
            rest.get(1..).unwrap_or(rest)
        }
        ("python" | "python3", Some("-m")) => &words[2..],
        ("uv" | "poetry" | "pipenv" | "pdm", Some("run")) => {
            skip_options(&words[2..], &["--with", "--python", "-p"])
        }
        _ => words,
    }
}

/// `words` past their leading options, where each option in `with_values` also takes the word after it.
fn skip_options<'a>(mut words: &'a [Word], with_values: &[&str]) -> &'a [Word] {
    while let Some(first) = words.first() {
        if first.text == "--" {
            return &words[1..];
        }
        if !first.text.starts_with('-') || first.text == "-" {
            break;
        }
        let takes_value = with_values.contains(&first.text.as_str());
        words = &words[if takes_value { 2 } else { 1 }.min(words.len())..];
    }
    words
}

/// A here-document whose body is still to be skipped when the line it was opened on ends.
#[derive(Debug, Default)]
struct HereDoc {
    /// The line that ends the body.
    delimiter: String,
    /// Whether leading tabs are stripped from body lines, as `<<-` asks.
    strip_tabs: bool,
}

/// The state of reading one command line.
#[derive(Debug, Default)]
struct Reader {
    /// The command line.
    chars: Vec<char>,
    /// Where reading has got to in `chars`.
    at: usize,
    /// The word being read, once one has started.
    word: Option<Word>,
    /// The operator of a redirection waiting for its target.
    redirect: Option<String>,
    /// Here-documents opened on the current line.
    here_docs: Vec<HereDoc>,
    /// The command being read.
    simple: Simple,
    /// The pipeline being read.
    pipeline: Pipeline,
    /// What has been read so far.
    line: CommandLine,
}

impl Reader {
    /// Reads every character, building [`Reader::line`].
    fn read(&mut self) {
        while let Some(c) = self.next() {
            match c {
                ' ' | '\t' => self.end_word(),
                '\n' => self.end_line(),
                '#' if self.word.is_none() => self.skip_comment(),
                '\'' => self.single_quoted(),
                '"' => self.double_quoted(),
                '\\' => self.escaped(),
                '$' => self.dollar(),
                '`' => self.backticks(),
                '|' => self.pipe(),
                '&' => self.ampersand(),
                ';' => self.semicolon(),
                '(' | ')' => self.subshell(),
                '<' | '>' => self.redirection(c),
                '*' | '?' | '[' | '{' => self.push_expanding(c),
                '~' if self.word.is_none() => self.push_expanding(c),
                _ => self.push(c),
            }
        }
        self.end_pipeline();
        if !self.here_docs.is_empty() {
            self.line.unusual = true;
        }
    }

    /// Takes the next character, when there is one.
    fn next(&mut self) -> Option<char> {
        let c = self.chars.get(self.at).copied();
        self.at += usize::from(c.is_some());
        c
    }

    /// The next character, left unread.
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// Takes the next character when it is `c`, saying whether it was.
    fn eat(&mut self, c: char) -> bool {
        let found = self.peek() == Some(c);
        self.at += usize::from(found);
        found
    }

    /// The word being read, starting an empty literal one when none has started.
    fn word(&mut self) -> &mut Word {
        self.word.get_or_insert_with(|| Word {
            text: String::new(),
            literal: true,
        })
    }

    /// Adds `c` to the word being read.
    fn push(&mut self, c: char) {
        self.word().text.push(c);
    }

    /// Adds `c`, which the shell may expand, to the word being read and marks the word as not literal.
    fn push_expanding(&mut self, c: char) {
        let word = self.word();
        word.text.push(c);
        word.literal = false;
    }

    /// Ends the word being read: it becomes the target of a waiting redirection, or the next word of the command.
    fn end_word(&mut self) {
        let Some(word) = self.word.take() else { return };
        if let Some(operator) = self.redirect.take() {
            if operator.ends_with("<<") || operator.ends_with("<<-") {
                self.here_docs.push(HereDoc {
                    delimiter: word.text.clone(),
                    strip_tabs: operator.ends_with('-'),
                });
            }
            self.simple.redirects.push(Redirect {
                operator,
                target: Some(word),
            });
            return;
        }
        if self.simple.words.is_empty() && KEYWORDS.contains(&word.text.as_str()) {
            self.line.unusual = true;
        }
        self.simple.words.push(word);
    }

    /// Ends the command being read, adding it to the pipeline when it has anything in it.
    fn end_simple(&mut self) {
        self.end_word();
        if let Some(operator) = self.redirect.take() {
            self.line.unusual = true;
            self.simple.redirects.push(Redirect {
                operator,
                target: None,
            });
        }
        let simple = std::mem::take(&mut self.simple);
        if !simple.words.is_empty() || !simple.redirects.is_empty() {
            self.pipeline.stages.push(simple);
        }
    }

    /// Ends the pipeline being read, adding it to the line when it has any command in it.
    fn end_pipeline(&mut self) {
        self.end_simple();
        let pipeline = std::mem::take(&mut self.pipeline);
        if !pipeline.stages.is_empty() {
            self.line.pipelines.push(pipeline);
        }
    }

    /// Ends the pipeline at a newline and skips the bodies of the here-documents opened on the line.
    fn end_line(&mut self) {
        self.end_pipeline();
        for here_doc in std::mem::take(&mut self.here_docs) {
            if !self.skip_here_doc(&here_doc) {
                self.line.unusual = true;
            }
        }
    }

    /// Skips the lines of a here-document body up to and including its delimiter line, saying whether the delimiter was found.
    fn skip_here_doc(&mut self, here_doc: &HereDoc) -> bool {
        for _ in 0..MAX_HERE_DOC_LINES {
            if self.at >= self.chars.len() {
                return false;
            }
            let line = self.take_line();
            let line = if here_doc.strip_tabs {
                line.trim_start_matches('\t')
            } else {
                line.as_str()
            };
            if line == here_doc.delimiter {
                return true;
            }
        }
        false
    }

    /// Takes the rest of the current line, without its newline.
    fn take_line(&mut self) -> String {
        let mut line = String::new();
        while let Some(c) = self.next() {
            if c == '\n' {
                break;
            }
            line.push(c);
        }
        line
    }

    /// Skips a comment to the end of its line, leaving the newline to end the pipeline.
    fn skip_comment(&mut self) {
        while self.peek().is_some_and(|c| c != '\n') {
            self.at += 1;
        }
    }

    /// Reads the rest of a `'…'` string into the word: everything up to the closing quote, as it is.
    fn single_quoted(&mut self) {
        self.word();
        loop {
            match self.next() {
                Some('\'') => return,
                Some(c) => self.push(c),
                None => {
                    self.line.unusual = true;
                    return;
                }
            }
        }
    }

    /// Reads the rest of a `"…"` string into the word, keeping the escapes bash keeps and noting expansions.
    fn double_quoted(&mut self) {
        self.word();
        loop {
            match self.next() {
                Some('"') => return,
                Some('\\') => self.escaped_in_double_quotes(),
                Some('$') => self.dollar(),
                Some('`') => self.backticks(),
                Some(c) => self.push(c),
                None => {
                    self.line.unusual = true;
                    return;
                }
            }
        }
    }

    /// Reads what follows a backslash inside double quotes: the escaped character for `$`, `` ` ``, `"`, `\` and a newline, the backslash and the character otherwise.
    fn escaped_in_double_quotes(&mut self) {
        match self.next() {
            Some('\n') => {}
            Some(c @ ('$' | '`' | '"' | '\\')) => self.push(c),
            Some(c) => {
                self.push('\\');
                self.push(c);
            }
            None => self.line.unusual = true,
        }
    }

    /// Reads what follows a backslash outside quotes: the character itself, or nothing for a line continuation.
    fn escaped(&mut self) {
        match self.next() {
            Some('\n') => {}
            Some(c) => self.push(c),
            None => self.push('\\'),
        }
    }

    /// Reads a `$` expansion as written into the word, with the whole of a `$(…)` or `${…}`, and marks the word as not literal.
    fn dollar(&mut self) {
        self.push_expanding('$');
        match self.peek() {
            Some('(') => self.copy_balanced('(', ')'),
            Some('{') => self.copy_balanced('{', '}'),
            _ => {}
        }
    }

    /// Copies a bracketed span starting at the next character, `open`, up to its matching `close`, into the word.
    fn copy_balanced(&mut self, open: char, close: char) {
        let mut depth = 0usize;
        while let Some(c) = self.next() {
            self.push(c);
            if c == open {
                depth += 1;
            } else if c == close {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
        }
        self.line.unusual = true;
    }

    /// Reads a `` `…` `` command substitution as written into the word and marks the word as not literal.
    fn backticks(&mut self) {
        self.push_expanding('`');
        loop {
            match self.next() {
                Some('`') => {
                    self.push('`');
                    return;
                }
                Some('\\') => {
                    self.push('\\');
                    if let Some(c) = self.next() {
                        self.push(c);
                    }
                }
                Some(c) => self.push(c),
                None => {
                    self.line.unusual = true;
                    return;
                }
            }
        }
    }

    /// Reads after a `|`: `||` ends the pipeline, `|` and `|&` start the next stage.
    fn pipe(&mut self) {
        if self.eat('|') {
            self.end_pipeline();
        } else {
            self.eat('&');
            self.end_simple();
        }
    }

    /// Reads after a `&`: `&&` and a lone `&` end the pipeline, `&>` and `&>>` redirect both streams.
    fn ampersand(&mut self) {
        if self.eat('&') {
            self.end_pipeline();
        } else if self.eat('>') {
            self.end_word();
            let operator = if self.eat('>') { "&>>" } else { "&>" };
            self.redirect = Some(operator.to_owned());
        } else {
            self.end_pipeline();
        }
    }

    /// Reads after a `;`: ends the pipeline, and `;;` (a `case` branch) makes the line unusual.
    fn semicolon(&mut self) {
        if self.eat(';') {
            self.line.unusual = true;
        }
        self.end_pipeline();
    }

    /// Reads a parenthesis, which opens or closes a subshell this reading does not follow.
    fn subshell(&mut self) {
        self.line.unusual = true;
        self.end_pipeline();
    }

    /// Reads a redirection operator starting with `first`, `<` or `>`, taking a descriptor number written just before it.
    fn redirection(&mut self, first: char) {
        let descriptor = self
            .word
            .take_if(|word| {
                word.literal
                    && !word.text.is_empty()
                    && word.text.chars().all(|c| c.is_ascii_digit())
            })
            .map(|word| word.text)
            .unwrap_or_default();
        self.end_word();
        let rest = match first {
            '>' => self.output_operator(),
            _ => self.input_operator(),
        };
        self.redirect = Some(format!("{descriptor}{rest}"));
    }

    /// Reads the rest of an operator starting with `>`: `>`, `>>`, `>|` or `>&`.
    fn output_operator(&mut self) -> &'static str {
        if self.eat('>') {
            ">>"
        } else if self.eat('|') {
            ">|"
        } else if self.eat('&') {
            ">&"
        } else {
            ">"
        }
    }

    /// Reads the rest of an operator starting with `<`: `<`, `<<`, `<<-`, `<<<`, `<&` or `<>`.
    fn input_operator(&mut self) -> &'static str {
        if self.eat('<') {
            if self.eat('<') {
                "<<<"
            } else if self.eat('-') {
                "<<-"
            } else {
                "<<"
            }
        } else if self.eat('&') {
            "<&"
        } else if self.eat('>') {
            "<>"
        } else {
            "<"
        }
    }
}

#[cfg(test)]
/// Tests for reading command lines.
mod tests {
    use super::*;

    /// The texts of the words of every command of `line`, pipeline by pipeline.
    fn words(line: &CommandLine) -> Vec<Vec<Vec<&str>>> {
        line.pipelines
            .iter()
            .map(|pipeline| {
                pipeline
                    .stages
                    .iter()
                    .map(|simple| simple.words.iter().map(|word| word.text.as_str()).collect())
                    .collect()
            })
            .collect()
    }

    /// Lists, pipelines and quoted words are split the way the shell splits them.
    #[test]
    fn splits_lists_pipelines_and_quotes() {
        let line = parse("cd 'my dir' && cargo test \"a b\" 2>&1 | tail -n 5; echo done\\ now");
        assert!(!line.unusual);
        assert_eq!(
            words(&line),
            vec![
                vec![vec!["cd", "my dir"]],
                vec![vec!["cargo", "test", "a b"], vec!["tail", "-n", "5"]],
                vec![vec!["echo", "done now"]],
            ]
        );
        let redirect = &line.pipelines[1].stages[0].redirects[0];
        assert_eq!(redirect.operator, "2>&");
        assert_eq!(redirect.target.as_ref().unwrap().text, "1");
        assert!(!redirect.writes_stdout());
    }

    /// Expansions are kept as written and mark their word; quoted globs do not.
    #[test]
    fn notes_expansions() {
        let line = parse("grep \"$x\" '*.rs' src/*.rs $(pwd) ~/a");
        let simple = &line.pipelines[0].stages[0];
        let literal: Vec<bool> = simple.words.iter().map(|word| word.literal).collect();
        assert_eq!(literal, vec![true, false, true, false, false, false]);
        assert_eq!(simple.words[4].text, "$(pwd)");
    }

    /// Redirections are taken out of the words, and a here-document body is skipped.
    #[test]
    fn reads_redirections_and_here_documents() {
        let line = parse("cat > out.txt <<'EOF'\nhello | world\nEOF\necho next");
        assert!(!line.unusual);
        assert_eq!(
            words(&line),
            vec![vec![vec!["cat"]], vec![vec!["echo", "next"]]]
        );
        let redirects = &line.pipelines[0].stages[0].redirects;
        assert!(redirects[0].writes_stdout());
        assert!(redirects[1].is_here_doc());
        assert_eq!(redirects[1].target.as_ref().unwrap().text, "EOF");
        assert!(parse("cat <<EOF\nnever ends").unusual);
    }

    /// Subshells, groups, control flow and broken quotes mark the line unusual.
    #[test]
    fn marks_what_it_does_not_follow() {
        for command in [
            "(cd x; ls)",
            "{ ls; }",
            "if true; then ls; fi",
            "for f in *; do cat $f; done",
            "echo 'open",
            "ls >",
        ] {
            assert!(parse(command).unusual, "{command}");
        }
        assert!(!parse("ls -la # a comment (with parens)").unusual);
    }

    /// The program is found past assignments and wrappers.
    #[test]
    fn finds_the_program_past_wrappers() {
        for (command, program) in [
            ("RUST_LOG=debug cargo build", "cargo"),
            ("sudo -u root -E apt-get install x", "apt-get"),
            ("env -i A=1 B=2 /usr/bin/make -j8", "make"),
            ("time nice -n 5 go test ./...", "go"),
            ("timeout -s KILL 60 npm ci", "npm"),
            ("python3 -m pytest -q", "pytest"),
            ("uv run pytest", "pytest"),
            ("git status", "git"),
        ] {
            let line = parse(command);
            let invocation = line.pipelines[0].stages[0].invocation();
            assert_eq!(program_name(&invocation[0]), program, "{command}");
        }
    }
}
