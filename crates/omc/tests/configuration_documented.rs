//! Every environment variable the harness reads at run time is listed in `docs/configuration.md`.
//!
//! The workspace's Rust sources are scanned, test code left out, for the names
//! of environment variables: every string literal that starts with `OMC_`, the
//! key variables of provider definitions (`env: &[...]`), every literal that
//! names a key, token or address by its ending (`_API_KEY`, `_TOKEN`, `_URL`),
//! and every upper-case literal in a source
//! file that reads the environment itself. A name found that the page does not
//! mention fails the test.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One piece of Rust source, as far as the scan needs to tell them apart.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    /// A name or keyword, or a number.
    Word(String),
    /// The text of a string literal, escapes left as written.
    Text(String),
    /// Any other single character.
    Mark(char),
}

/// The tokens of `source`, without comments, white space, character literals or lifetimes.
fn tokenize(source: &str) -> Vec<Token> {
    let chars: Vec<char> = source.chars().collect();
    let mut tokens = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        if c.is_whitespace() {
            at += 1;
        } else if c == '/' && chars.get(at + 1) == Some(&'/') {
            at = line_end(&chars, at);
        } else if c == '/' && chars.get(at + 1) == Some(&'*') {
            at = block_comment_end(&chars, at);
        } else if let Some((text, next)) = string_literal(&chars, at) {
            tokens.push(Token::Text(text));
            at = next;
        } else if c == '\'' {
            at = quote_end(&chars, at);
        } else if c.is_alphanumeric() || c == '_' {
            let start = at;
            while at < chars.len() && (chars[at].is_alphanumeric() || chars[at] == '_') {
                at += 1;
            }
            tokens.push(Token::Word(chars[start..at].iter().collect()));
        } else {
            tokens.push(Token::Mark(c));
            at += 1;
        }
    }
    tokens
}

/// The index just past the line comment that starts at `at`.
fn line_end(chars: &[char], at: usize) -> usize {
    chars[at..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |offset| at + offset + 1)
}

/// The index just past the block comment that starts at `at`, comments nested in it included.
fn block_comment_end(chars: &[char], mut at: usize) -> usize {
    let mut depth = 0usize;
    while at < chars.len() {
        if chars[at] == '/' && chars.get(at + 1) == Some(&'*') {
            depth += 1;
            at += 2;
        } else if chars[at] == '*' && chars.get(at + 1) == Some(&'/') {
            depth -= 1;
            at += 2;
            if depth == 0 {
                return at;
            }
        } else {
            at += 1;
        }
    }
    at
}

/// The text of the string literal that starts at `at` and the index just past it, when one starts there.
///
/// Plain, byte and C strings (`"…"`, `b"…"`, `c"…"`) and raw ones (`r"…"`,
/// `r#"…"#`, `br#"…"#`) are recognized; a raw identifier such as `r#type` is not
/// a string. It is only asked at the start of a token, so a prefix letter is
/// never the end of a longer word.
fn string_literal(chars: &[char], at: usize) -> Option<(String, usize)> {
    let mut cursor = at;
    if matches!(chars.get(cursor), Some('b' | 'c')) {
        cursor += 1;
    }
    if chars.get(cursor) == Some(&'r') {
        cursor += 1;
        let hashes = chars[cursor..].iter().take_while(|&&c| c == '#').count();
        cursor += hashes;
        if chars.get(cursor) != Some(&'"') {
            return None;
        }
        let start = cursor + 1;
        let mut end = start;
        while end < chars.len() {
            if chars[end] == '"'
                && chars[end + 1..]
                    .iter()
                    .take(hashes)
                    .filter(|&&c| c == '#')
                    .count()
                    == hashes
            {
                return Some((chars[start..end].iter().collect(), end + 1 + hashes));
            }
            end += 1;
        }
        return Some((chars[start..].iter().collect(), chars.len()));
    }
    if chars.get(cursor) != Some(&'"') {
        return None;
    }
    let start = cursor + 1;
    let mut end = start;
    while end < chars.len() && chars[end] != '"' {
        end += if chars[end] == '\\' { 2 } else { 1 };
    }
    let end = end.min(chars.len());
    Some((
        chars[start..end].iter().collect(),
        (end + 1).min(chars.len()),
    ))
}

/// The index just past the character literal or lifetime whose quote is at `at`.
fn quote_end(chars: &[char], at: usize) -> usize {
    if chars.get(at + 1) == Some(&'\\') {
        let mut end = at + 2;
        while end < chars.len() && chars[end] != '\'' {
            end += 1;
        }
        return end + 1;
    }
    if chars.get(at + 2) == Some(&'\'') {
        return at + 3;
    }
    at + 1
}

/// What the scan of one source file found.
#[derive(Debug, Default)]
struct Scanned {
    /// The tokens of the file outside test code.
    tokens: Vec<Token>,
    /// The modules the file declares, as `mod name;`, only for tests.
    test_modules: Vec<String>,
}

/// Whether the tokens from `at` are the attribute `#[cfg(test)]`, or `#![cfg(test)]` when `inner`.
fn is_test_attribute(tokens: &[Token], at: usize, inner: bool) -> bool {
    let word = |name: &str| Token::Word(name.to_owned());
    let mut expected = vec![Token::Mark('#')];
    if inner {
        expected.push(Token::Mark('!'));
    }
    expected.extend([
        Token::Mark('['),
        word("cfg"),
        Token::Mark('('),
        word("test"),
        Token::Mark(')'),
        Token::Mark(']'),
    ]);
    tokens.get(at..at + expected.len()) == Some(&expected[..])
}

/// The index just past the bracketed group whose opening bracket is at `at`.
fn group_end(tokens: &[Token], at: usize) -> usize {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(at) {
        match token {
            Token::Mark('(' | '[' | '{') => depth += 1,
            Token::Mark(')' | ']' | '}') => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return index + 1;
                }
            }
            _ => {}
        }
    }
    tokens.len()
}

/// The index just past the item that starts at `at`: it ends at a `;` or `,` outside brackets, after a braced block, or before a closing bracket it did not open.
fn item_end(tokens: &[Token], mut at: usize) -> usize {
    while at < tokens.len() {
        match &tokens[at] {
            Token::Mark(';' | ',') => return at + 1,
            Token::Mark('{') => {
                let end = group_end(tokens, at);
                return if tokens.get(end) == Some(&Token::Mark(';')) {
                    end + 1
                } else {
                    end
                };
            }
            Token::Mark('(' | '[') => at = group_end(tokens, at),
            Token::Mark(')' | ']' | '}') => return at,
            _ => at += 1,
        }
    }
    at
}

/// The tokens of `source` with the code under `#[cfg(test)]` taken out, and the test modules it declares by name.
fn without_tests(source: &str) -> Scanned {
    let tokens = tokenize(source);
    let mut scanned = Scanned::default();
    if (0..tokens.len()).any(|at| is_test_attribute(&tokens, at, true)) {
        return scanned;
    }
    let mut at = 0;
    while at < tokens.len() {
        if !is_test_attribute(&tokens, at, false) {
            scanned.tokens.push(tokens[at].clone());
            at += 1;
            continue;
        }
        at += 7;
        while tokens.get(at) == Some(&Token::Mark('#')) {
            at = group_end(&tokens, at + 1);
        }
        if tokens.get(at) == Some(&Token::Word("pub".to_owned())) {
            at += 1;
            if tokens.get(at) == Some(&Token::Mark('(')) {
                at = group_end(&tokens, at);
            }
        }
        if let (Some(Token::Word(keyword)), Some(Token::Word(name)), Some(Token::Mark(';'))) =
            (tokens.get(at), tokens.get(at + 1), tokens.get(at + 2))
            && keyword == "mod"
        {
            scanned.test_modules.push(name.clone());
            at += 3;
            continue;
        }
        at = item_end(&tokens, at);
    }
    scanned
}

/// Whether `name` looks like an environment variable: upper-case letters, digits and `_`, starting with a letter, at least four long.
fn looks_like_variable(name: &str) -> bool {
    name.len() >= 4
        && name.starts_with(|c: char| c.is_ascii_uppercase())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// Whether `tokens` read the process environment with `env::var`, `env::var_os` or `env::vars`.
fn reads_environment(tokens: &[Token]) -> bool {
    tokens.windows(4).any(|window| {
        matches!(
            window,
            [Token::Word(module), Token::Mark(':'), Token::Mark(':'), Token::Word(call)]
                if module == "env" && matches!(call.as_str(), "var" | "var_os" | "vars")
        )
    })
}

/// Whether the string literal at `at` is the argument of a macro read at build time (`env!`, `option_env!`) or names a variable set for a child process (`.env(`, `.env_remove(`).
fn names_no_runtime_read(tokens: &[Token], at: usize) -> bool {
    let before = |back: usize| at.checked_sub(back).and_then(|index| tokens.get(index));
    if before(1) != Some(&Token::Mark('(')) {
        return false;
    }
    match (before(3), before(2)) {
        (Some(Token::Word(name)), Some(Token::Mark('!'))) => {
            matches!(name.as_str(), "env" | "option_env")
        }
        (Some(Token::Mark('.')), Some(Token::Word(name))) => {
            matches!(name.as_str(), "env" | "env_remove")
        }
        _ => false,
    }
}

/// The indices of the string literals in `tokens` that list key variables of a provider, as in `env: &["GROQ_API_KEY"]`.
fn provider_key_literals(tokens: &[Token]) -> BTreeSet<usize> {
    let mut found = BTreeSet::new();
    for at in 0..tokens.len() {
        let opens = matches!(
            tokens.get(at..at + 4),
            Some([Token::Word(field), Token::Mark(':'), Token::Mark('&'), Token::Mark('[')]) if field == "env"
        );
        if !opens {
            continue;
        }
        let mut inside = at + 4;
        while let Some(token) = tokens.get(inside) {
            match token {
                Token::Text(_) => {
                    found.insert(inside);
                }
                Token::Mark(',') => {}
                _ => break,
            }
            inside += 1;
        }
    }
    found
}

/// The environment variable names the tokens of one source file read at run time.
fn variables_in(tokens: &[Token]) -> BTreeSet<String> {
    let reads = reads_environment(tokens);
    let keys = provider_key_literals(tokens);
    let mut names = BTreeSet::new();
    for (at, token) in tokens.iter().enumerate() {
        let Token::Text(text) = token else { continue };
        if !looks_like_variable(text) {
            continue;
        }
        let own = text.starts_with("OMC_");
        let key = keys.contains(&at)
            || ["_API_KEY", "_TOKEN", "_URL"]
                .iter()
                .any(|ending| text.ends_with(ending));
        let local = reads && !names_no_runtime_read(tokens, at);
        if own || key || local {
            names.insert(text.clone());
        }
    }
    names
}

/// The root of the workspace.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The `.rs` files under `folder`, folders inside it included, in name order.
fn rust_files(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            files.extend(rust_files(&path));
        } else if path.extension().is_some_and(|extension| extension == "rs") {
            files.push(path);
        }
    }
    files
}

/// The folder the modules a file declares live in: its own folder for `lib.rs`, `main.rs` and `mod.rs`, else a folder named after it.
fn module_folder(file: &Path) -> PathBuf {
    let folder = file.parent().unwrap_or(Path::new("")).to_path_buf();
    match file.file_stem().and_then(|stem| stem.to_str()) {
        Some("lib" | "main" | "mod") | None => folder,
        Some(stem) => folder.join(stem),
    }
}

/// Every environment variable name the non-test sources of the workspace's crates read at run time, with the first file each was found in.
///
/// Only `src` of each crate is read, so integration tests, benchmarks and
/// examples are left out; so are files and folders of modules declared only
/// for tests.
fn variables_read() -> BTreeMap<String, PathBuf> {
    let crates = workspace().join("crates");
    let mut folders: Vec<PathBuf> = std::fs::read_dir(&crates)
        .expect("the workspace has a crates folder")
        .flatten()
        .map(|entry| entry.path().join("src"))
        .filter(|src| src.is_dir())
        .collect();
    folders.sort();
    let mut found = BTreeMap::new();
    for src in folders {
        let files = rust_files(&src);
        let scanned: Vec<(PathBuf, Scanned)> = files
            .into_iter()
            .map(|file| {
                let source = std::fs::read_to_string(&file).expect("a source file is readable");
                let scanned = without_tests(&source);
                (file, scanned)
            })
            .collect();
        let mut excluded: Vec<PathBuf> = Vec::new();
        for (file, scan) in &scanned {
            for name in &scan.test_modules {
                let folder = module_folder(file);
                excluded.push(folder.join(format!("{name}.rs")));
                excluded.push(folder.join(name));
            }
        }
        for (file, scan) in scanned {
            if excluded.iter().any(|excluded| file.starts_with(excluded)) {
                continue;
            }
            for name in variables_in(&scan.tokens) {
                found.entry(name).or_insert_with(|| file.clone());
            }
        }
    }
    found
}

/// Whether `page` mentions `name` as a whole word, so `OMC_SEARCH` is not taken as mentioned by `OMC_SEARCHES`.
fn mentions(page: &str, name: &str) -> bool {
    let part_of_name = |c: char| c.is_ascii_alphanumeric() || c == '_';
    page.match_indices(name).any(|(at, _)| {
        let before = page[..at].chars().next_back();
        let after = page[at + name.len()..].chars().next();
        !before.is_some_and(part_of_name) && !after.is_some_and(part_of_name)
    })
}

/// Every environment variable the harness reads is mentioned in the configuration page.
#[test]
fn every_variable_is_documented() {
    let page = std::fs::read_to_string(workspace().join("docs/configuration.md"))
        .expect("docs/configuration.md exists");
    let read = variables_read();
    for sure in [
        "OMC_PROVIDERS_FILE",
        "GROQ_API_KEY",
        "CLAUDE_CONFIG_DIR",
        "GH_TOKEN",
    ] {
        assert!(
            read.contains_key(sure),
            "the scan should find {sure}, which the harness reads; it found {:?}",
            read.keys().collect::<Vec<_>>()
        );
    }
    let missing: Vec<String> = read
        .iter()
        .filter(|(name, _)| !mentions(&page, name))
        .map(|(name, file)| format!("{name} (read in {})", file.display()))
        .collect();
    assert!(
        missing.is_empty(),
        "docs/configuration.md does not mention these environment variables:\n{}",
        missing.join("\n")
    );
}

/// Code under `#[cfg(test)]` is left out of the scan, whether a module, a function or a field, and test modules declared by name are reported.
#[test]
fn test_code_is_left_out() {
    let source = r##"
        const KEY: &str = "OMC_REAL";
        struct State {
            kept: u8,
            #[cfg(test)]
            today: Option<&'static str>,
        }
        #[cfg(test)]
        #[allow(dead_code)]
        fn helper() -> &'static str { "OMC_HELPER" }
        #[cfg(test)]
        mod testutil;
        #[cfg(test)]
        mod tests {
            const RAW: &str = r#"{ "OMC_INSIDE" }"#;
            fn f() -> char { '}' }
        }
        const AFTER: &str = "OMC_AFTER";
    "##;
    let scanned = without_tests(source);
    let names = variables_in(&scanned.tokens);
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        ["OMC_AFTER", "OMC_REAL"]
    );
    assert_eq!(scanned.test_modules, ["testutil"]);
    assert!(scanned.tokens.contains(&Token::Word("kept".to_owned())));
    assert!(!scanned.tokens.contains(&Token::Word("today".to_owned())));
    assert!(
        without_tests("#![cfg(test)]\nconst A: &str = \"OMC_A\";")
            .tokens
            .is_empty()
    );
}

/// Provider keys, key, token and address names, and upper-case names in a file that reads the environment are found; build-time and child-process names are not.
#[test]
fn variables_are_recognized() {
    let source = r#"
        static DEF: Def = Def { auth: Auth::ApiKey { env: &["SOME_PROVIDER_SECRET"] } };
        const LIST: [&str; 3] = ["BRAVE_API_KEY", "GH_TOKEN", "SEARXNG_URL"];
        fn read() -> Option<String> { std::env::var("HOSTNAME").ok() }
        const AGENT: &str = concat!("crab/", env!("CARGO_PKG_VERSION"));
        fn run(command: &mut Command) { command.env("GIT_CONFIG_NOSYSTEM", "1"); }
        const WORD: &str = "SELECT";
    "#;
    let names = variables_in(&tokenize(source));
    assert_eq!(
        names.into_iter().collect::<Vec<_>>(),
        [
            "BRAVE_API_KEY",
            "GH_TOKEN",
            "HOSTNAME",
            "SEARXNG_URL",
            "SELECT",
            "SOME_PROVIDER_SECRET"
        ]
    );
    let quiet = r#"const WORD: &str = "SELECT"; const SET: &str = "GIT_DIR";"#;
    assert!(variables_in(&tokenize(quiet)).is_empty());
}

/// A name counts as mentioned only as a whole word.
#[test]
fn mentions_are_whole_words() {
    assert!(mentions("set `OMC_SEARCH=x`", "OMC_SEARCH"));
    assert!(mentions("`OMC_DAP_<ID>`", "OMC_DAP_"));
    assert!(!mentions("`OMC_SEARCHES`", "OMC_SEARCH"));
    assert!(!mentions("`XOMC_SEARCH`", "OMC_SEARCH"));
}
