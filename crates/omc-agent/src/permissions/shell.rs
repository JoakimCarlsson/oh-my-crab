//! Taking a shell command line apart into the simple commands it runs, so that permission rules can be matched against each of them.
//!
//! A command line such as `cargo test && git status | head` runs three
//! programs; a rule that allows `cargo test` must not let the other two ride
//! along. [`split`] finds every simple command a line runs: those joined by
//! `&&`, `||`, `;`, `|`, `|&`, `&` and newlines, those inside `( … )`
//! subshells, and those inside `$( … )` and backtick substitutions, wherever
//! they appear, in double quotes too. Redirections are taken off each command
//! and output redirections to a file are noted, since they write.
//!
//! What the splitter does not understand it does not guess at: a line with
//! control flow (`if`, `for`, `while`, `case`, functions, `{ … }` groups),
//! here-documents, process substitution, arithmetic expansion, a substitution
//! inside `${ … }`, or quotes left open is [`Opaque`], and no rule short of
//! one allowing every command lets it run without asking.

/// Most nested subshells and substitutions taken apart before a line counts as opaque.
const MAX_DEPTH: usize = 16;

/// Words that start control flow or a group, which the splitter does not take apart.
const RESERVED: &[&str] = &[
    "if", "then", "else", "elif", "fi", "for", "while", "until", "do", "done", "case", "esac",
    "function", "select", "coproc", "{", "}", "[[", "]]",
];

/// Files an output redirection may go to without writing anything.
const HARMLESS_SINKS: &[&str] = &["/dev/null", "/dev/stdout", "/dev/stderr", "/dev/tty"];

/// One simple command a command line runs: a program and its arguments, without the redirections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Simple {
    /// The words as written, quotes and expansions kept, the program first; variable assignments before the program are words too.
    pub words: Vec<String>,
    /// Whether an output redirection sends something to a file, which writes it.
    pub writes: bool,
}

impl Simple {
    /// The words joined by single spaces, as rules are matched against.
    pub fn text(&self) -> String {
        self.words.join(" ")
    }

    /// Whether the program word is written plainly, with no quotes, expansions, globs or escapes, so that what it names is what runs.
    pub fn plain_program(&self) -> bool {
        self.words.first().is_some_and(|word| {
            !word.is_empty()
                && word.chars().all(|c| {
                    c.is_ascii_alphanumeric()
                        || matches!(c, '_' | '-' | '.' | '/' | '+' | '@' | ':' | ',' | '%')
                })
        })
    }
}

/// A command line the splitter will not take apart, with why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opaque(pub String);

/// Every simple command `line` runs, in the order they appear, those inside substitutions after the command they are in.
///
/// # Errors
///
/// Returns [`Opaque`] when the line uses something the splitter does not take apart: control flow, a group, a function, a here-document, process substitution, arithmetic, a substitution inside `${ … }`, an unbalanced quote or parenthesis, or nesting deeper than it follows.
pub fn split(line: &str) -> Result<Vec<Simple>, Opaque> {
    let chars: Vec<char> = line.chars().collect();
    let mut scanner = Scanner {
        chars: &chars,
        at: 0,
        found: Vec::new(),
        depth: 0,
    };
    scanner.list(Close::End)?;
    Ok(scanner.found)
}

/// What ends the list of commands being read.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Close {
    /// The end of the line.
    End,
    /// A `)` closing a subshell or a `$(` substitution.
    Paren,
}

/// How a word read ended up, besides its text.
enum Read {
    /// A word, as written.
    Word(String),
    /// The start of a redirection, with its operator.
    Redirect(String),
}

/// Reads a command line one character at a time.
struct Scanner<'a> {
    /// The line.
    chars: &'a [char],
    /// Where reading is.
    at: usize,
    /// The simple commands found so far.
    found: Vec<Simple>,
    /// How deep in subshells and substitutions reading is.
    depth: usize,
}

impl Scanner<'_> {
    /// The character at the reading position, when there is one.
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    /// The character `ahead` places past the reading position, when there is one.
    fn peek_at(&self, ahead: usize) -> Option<char> {
        self.chars.get(self.at + ahead).copied()
    }

    /// An [`Opaque`] error saying `why`.
    fn opaque<T>(why: &str) -> Result<T, Opaque> {
        Err(Opaque(why.to_owned()))
    }

    /// Skips spaces, tabs and escaped line ends, but not line ends themselves.
    fn blanks(&mut self) {
        loop {
            match self.peek() {
                Some(' ' | '\t' | '\r') => self.at += 1,
                Some('\\') if self.peek_at(1) == Some('\n') => self.at += 2,
                _ => break,
            }
        }
    }

    /// Reads simple commands and the operators between them until `close`: the end of the line, or a `)` that it consumes.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] for what the splitter does not take apart, or a list closed wrongly.
    fn list(&mut self, close: Close) -> Result<(), Opaque> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Self::opaque("nested too deeply");
        }
        loop {
            self.blanks();
            match self.peek() {
                None => {
                    if close == Close::Paren {
                        return Self::opaque("a parenthesis is not closed");
                    }
                    break;
                }
                Some(')') => {
                    if close == Close::End {
                        return Self::opaque("a parenthesis closes that was not opened");
                    }
                    self.at += 1;
                    break;
                }
                Some(';') if self.peek_at(1) == Some(';') => {
                    return Self::opaque("`;;` belongs to a case statement");
                }
                Some(';' | '\n' | '&' | '|') => {
                    self.at += 1;
                }
                Some('#') => self.comment(),
                Some('(') => {
                    self.at += 1;
                    self.list(Close::Paren)?;
                    self.trailing_redirections()?;
                }
                Some(_) => self.simple()?,
            }
        }
        self.depth -= 1;
        Ok(())
    }

    /// Skips a comment, up to the end of its line.
    fn comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == '\n' {
                break;
            }
            self.at += 1;
        }
    }

    /// Reads the redirections after a subshell's `)`, up to the operator that ends the command; a write to a file is recorded as a command of its own that writes, so it is never allowed by a rule about a program.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] when anything but a redirection follows.
    fn trailing_redirections(&mut self) -> Result<(), Opaque> {
        loop {
            self.blanks();
            match self.peek() {
                None | Some(';' | '\n' | '&' | '|' | ')') => return Ok(()),
                Some(_) => match self.word()? {
                    Read::Redirect(operator) => {
                        if self.redirection(&operator)? {
                            self.found.push(Simple {
                                words: vec![format!("( … ) {operator}")],
                                writes: true,
                            });
                        }
                    }
                    Read::Word(_) => return Self::opaque("a word follows a subshell"),
                },
            }
        }
    }

    /// Reads one simple command up to the operator that ends it, and records it with the commands found inside its words.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] for control flow, groups, functions and what [`Self::word`] turns away.
    fn simple(&mut self) -> Result<(), Opaque> {
        let mut command = Simple {
            words: Vec::new(),
            writes: false,
        };
        let mut inner = Vec::new();
        loop {
            self.blanks();
            match self.peek() {
                Some('&') if self.peek_at(1) == Some('>') => {
                    let Some(operator) = self.redirect_operator() else {
                        return Self::opaque("a stray `&`");
                    };
                    let before = std::mem::take(&mut self.found);
                    let writes = self.redirection(&operator)?;
                    inner.extend(std::mem::replace(&mut self.found, before));
                    command.writes |= writes;
                }
                None | Some(';' | '\n' | '&' | '|' | ')') => break,
                Some('(') => return Self::opaque("a parenthesis inside a command"),
                Some('#') => {
                    self.comment();
                    break;
                }
                Some(_) => {
                    let before = std::mem::take(&mut self.found);
                    let read = self.word()?;
                    inner.extend(std::mem::replace(&mut self.found, before));
                    match read {
                        Read::Redirect(operator) => {
                            let before = std::mem::take(&mut self.found);
                            let writes = self.redirection(&operator)?;
                            inner.extend(std::mem::replace(&mut self.found, before));
                            command.writes |= writes;
                        }
                        Read::Word(word) => {
                            if command.words.is_empty() && RESERVED.contains(&word.as_str()) {
                                return Self::opaque(&format!("`{word}` starts control flow"));
                            }
                            if command.words.is_empty() && word == "!" {
                                continue;
                            }
                            command.words.push(word);
                        }
                    }
                }
            }
        }
        if !command.words.is_empty() || command.writes {
            if command.words.is_empty() {
                command.words.push(":".to_owned());
            }
            self.found.push(command);
        }
        self.found.extend(inner);
        Ok(())
    }

    /// Reads the target of a redirection with `operator`, and says whether it writes to a file.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] for here-documents and process substitution, and when the target is missing.
    fn redirection(&mut self, operator: &str) -> Result<bool, Opaque> {
        let bare = operator.trim_start_matches(|c: char| c.is_ascii_digit());
        if bare == "<<" || bare == "<<-" {
            return Self::opaque("a here-document");
        }
        self.blanks();
        if self.peek() == Some('(') {
            return Self::opaque("process substitution");
        }
        if matches!(self.peek(), None | Some(';' | '\n' | '&' | '|' | ')')) {
            return Self::opaque("a redirection without a target");
        }
        let Read::Word(target) = self.word()? else {
            return Self::opaque("a redirection without a target");
        };
        let output = bare.contains('>');
        let duplicates = matches!(bare, ">&" | "<&")
            && (target == "-" || target.chars().all(|c| c.is_ascii_digit()));
        Ok(output && !duplicates && !HARMLESS_SINKS.contains(&target.as_str()))
    }

    /// Reads one word, as written, recording the commands of the substitutions in it; or the operator of a redirection when the word is one, such as `2>`, `>>` or `&>`.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] for unclosed quotes and the expansions the splitter does not take apart.
    fn word(&mut self) -> Result<Read, Opaque> {
        if let Some(operator) = self.redirect_operator() {
            return Ok(Read::Redirect(operator));
        }
        let mut word = String::new();
        while let Some(c) = self.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | ')' => break,
                '<' | '>' => {
                    if self.peek_at(1) == Some('(') {
                        return Self::opaque("process substitution");
                    }
                    break;
                }
                '(' => return Self::opaque("a parenthesis inside a word"),
                '\\' => {
                    match self.peek_at(1) {
                        Some('\n') => {}
                        Some(next) => {
                            word.push('\\');
                            word.push(next);
                        }
                        None => word.push('\\'),
                    }
                    self.at += 2;
                }
                '\'' => {
                    word.push_str(&self.single_quoted()?);
                }
                '"' => {
                    word.push_str(&self.double_quoted()?);
                }
                '`' => {
                    word.push_str(&self.backticks()?);
                }
                '$' => {
                    word.push_str(&self.dollar()?);
                }
                other => {
                    word.push(other);
                    self.at += 1;
                }
            }
        }
        Ok(Read::Word(word))
    }

    /// The redirection operator at the reading position, consumed, with any file descriptor number before it; nothing, and nothing consumed, when there is none.
    fn redirect_operator(&mut self) -> Option<String> {
        let start = self.at;
        let mut operator = String::new();
        while let Some(c) = self.peek().filter(char::is_ascii_digit) {
            operator.push(c);
            self.at += 1;
        }
        let rest: String = self.chars[self.at..].iter().take(3).collect();
        let found = [
            "&>>", "<<<", "<<-", ">>", "<<", "<>", ">|", ">&", "<&", "&>", ">", "<",
        ]
        .into_iter()
        .find(|candidate| rest.starts_with(candidate));
        match found {
            Some(found) if !(found.starts_with('&') && !operator.is_empty()) => {
                self.at += found.chars().count();
                operator.push_str(found);
                Some(operator)
            }
            _ => {
                self.at = start;
                None
            }
        }
    }

    /// Reads a `'…'` string, quotes included.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] when it is not closed.
    fn single_quoted(&mut self) -> Result<String, Opaque> {
        let mut text = String::from('\'');
        self.at += 1;
        loop {
            match self.peek() {
                None => return Self::opaque("a quote is not closed"),
                Some('\'') => {
                    self.at += 1;
                    text.push('\'');
                    return Ok(text);
                }
                Some(c) => {
                    text.push(c);
                    self.at += 1;
                }
            }
        }
    }

    /// Reads a `"…"` string, quotes included, recording the commands of the substitutions inside it.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] when it is not closed or holds an expansion the splitter does not take apart.
    fn double_quoted(&mut self) -> Result<String, Opaque> {
        let mut text = String::from('"');
        self.at += 1;
        loop {
            match self.peek() {
                None => return Self::opaque("a quote is not closed"),
                Some('"') => {
                    self.at += 1;
                    text.push('"');
                    return Ok(text);
                }
                Some('\\') => {
                    text.push('\\');
                    if let Some(next) = self.peek_at(1) {
                        text.push(next);
                    }
                    self.at += 2;
                }
                Some('`') => text.push_str(&self.backticks()?),
                Some('$') => text.push_str(&self.dollar()?),
                Some(c) => {
                    text.push(c);
                    self.at += 1;
                }
            }
        }
    }

    /// Reads an expansion starting with `$`, as written, taking apart the commands of a `$( … )` substitution.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] for arithmetic, a substitution inside `${ … }`, and what is not closed.
    fn dollar(&mut self) -> Result<String, Opaque> {
        let start = self.at;
        match self.peek_at(1) {
            Some('(') if self.peek_at(2) == Some('(') => Self::opaque("arithmetic expansion"),
            Some('(') => {
                self.at += 2;
                self.list(Close::Paren)?;
                Ok(self.chars[start..self.at].iter().collect())
            }
            Some('{') => {
                self.at += 2;
                let mut depth = 1;
                while depth > 0 {
                    match self.peek() {
                        None => return Self::opaque("a `${` is not closed"),
                        Some('{') => depth += 1,
                        Some('}') => depth -= 1,
                        Some('`') => return Self::opaque("a substitution inside `${`"),
                        Some('$') if self.peek_at(1) == Some('(') => {
                            return Self::opaque("a substitution inside `${`");
                        }
                        Some('\\') => self.at += 1,
                        Some(_) => {}
                    }
                    self.at += 1;
                }
                Ok(self.chars[start..self.at].iter().collect())
            }
            Some('\'') => {
                self.at += 2;
                loop {
                    match self.peek() {
                        None => return Self::opaque("a quote is not closed"),
                        Some('\\') => self.at += 2,
                        Some('\'') => {
                            self.at += 1;
                            break;
                        }
                        Some(_) => self.at += 1,
                    }
                }
                Ok(self.chars[start..self.at.min(self.chars.len())]
                    .iter()
                    .collect())
            }
            _ => {
                self.at += 1;
                Ok("$".to_owned())
            }
        }
    }

    /// Reads a `` `…` `` substitution, as written, and records the commands of what it runs, taken apart on their own.
    ///
    /// # Errors
    ///
    /// Returns [`Opaque`] when it is not closed or what it runs is opaque.
    fn backticks(&mut self) -> Result<String, Opaque> {
        let start = self.at;
        self.at += 1;
        let mut inside = String::new();
        loop {
            match self.peek() {
                None => return Self::opaque("a backtick is not closed"),
                Some('`') => {
                    self.at += 1;
                    break;
                }
                Some('\\') => {
                    match self.peek_at(1) {
                        Some(next @ ('`' | '\\' | '$')) => inside.push(next),
                        Some(next) => {
                            inside.push('\\');
                            inside.push(next);
                        }
                        None => return Self::opaque("a backtick is not closed"),
                    }
                    self.at += 2;
                }
                Some(c) => {
                    inside.push(c);
                    self.at += 1;
                }
            }
        }
        if self.depth >= MAX_DEPTH {
            return Self::opaque("nested too deeply");
        }
        let chars: Vec<char> = inside.chars().collect();
        let mut nested = Scanner {
            chars: &chars,
            at: 0,
            found: Vec::new(),
            depth: self.depth,
        };
        nested.list(Close::End)?;
        self.found.extend(nested.found);
        Ok(self.chars[start..self.at].iter().collect())
    }
}
