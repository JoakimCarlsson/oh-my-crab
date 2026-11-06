//! What the structural tools `ast_grep` and `ast_edit` share: the languages
//! they parse, patterns compiled per language, the file walk and captures.

use std::borrow::Cow;
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ast_grep_core::matcher::MatcherExt;
use ast_grep_core::meta_var::MetaVarEnv;
use ast_grep_core::tree_sitter::{LanguageExt, StrDoc};
use ast_grep_core::{Doc, Language, Matcher, Node, NodeMatch, Pattern};
use ast_grep_language::SupportLang;
use bit_set::BitSet;
use ignore::WalkBuilder;
use ignore::overrides::OverrideBuilder;

use crate::ToolError;

/// Largest file the structural tools parse, in bytes; bigger files are skipped.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// Longest capture shown, in characters.
const MAX_CAPTURE_CHARS: usize = 200;

/// The languages whose grammars are built in, each with the name callers use for it.
const LANGUAGES: &[(SupportLang, &str)] = &[
    (SupportLang::C, "c"),
    (SupportLang::Cpp, "cpp"),
    (SupportLang::Css, "css"),
    (SupportLang::Go, "go"),
    (SupportLang::Html, "html"),
    (SupportLang::Java, "java"),
    (SupportLang::JavaScript, "javascript"),
    (SupportLang::Json, "json"),
    (SupportLang::Lua, "lua"),
    (SupportLang::Python, "python"),
    (SupportLang::Rust, "rust"),
    (SupportLang::Tsx, "tsx"),
    (SupportLang::TypeScript, "typescript"),
];

/// A match of a pattern in a parsed file.
pub type Match<'t> = NodeMatch<'t, StrDoc<SupportLang>>;

/// The names callers may pass as `lang`, for the argument schemas.
pub fn language_names() -> Vec<&'static str> {
    LANGUAGES.iter().map(|(_, name)| *name).collect()
}

/// The name a caller uses for `lang`.
pub fn language_name(lang: SupportLang) -> &'static str {
    LANGUAGES
        .iter()
        .find(|(known, _)| *known == lang)
        .map_or("unknown", |(_, name)| *name)
}

/// Looks up the built-in language called `name`, accepting common aliases such as `ts`, `py` or `rs`.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] naming the supported languages when `name` is not one of them.
fn language(name: &str) -> Result<SupportLang, ToolError> {
    name.trim()
        .parse::<SupportLang>()
        .ok()
        .filter(|lang| LANGUAGES.iter().any(|(known, _)| known == lang))
        .ok_or_else(|| {
            ToolError::InvalidArguments(format!(
                "unsupported language {name:?}; use one of: {}",
                language_names().join(", ")
            ))
        })
}

/// The built-in language a file is written in, judged by its extension.
pub(super) fn detect(path: &Path) -> Option<SupportLang> {
    SupportLang::from_path(path).filter(|lang| LANGUAGES.iter().any(|(known, _)| known == lang))
}

/// How to fix a pattern that does not compile, appended to every compile error.
const HINT: &str = "write one complete expression, statement or declaration; wrap a fragment in \
                    enough surrounding code to parse, and match lists with $$$NAME inside it";

/// The ways one pattern can be read in a language, tried in order on every node.
///
/// Most patterns have one reading. In languages whose files cannot hold bare
/// statements at the top level, a pattern such as `fmt.Println($A)` or
/// `malloc($N)` also gets a reading taken from inside a function body, where it
/// parses as the call it is meant to be rather than as a type or declaration.
pub struct Patterns {
    /// The readings, the plain one first.
    readings: Vec<Pattern>,
}

impl Patterns {
    /// The names of the metavariables any reading binds.
    pub fn defined_vars(&self) -> HashSet<&str> {
        self.readings
            .iter()
            .flat_map(Pattern::defined_vars)
            .collect()
    }
}

impl Matcher for Patterns {
    /// Matches `node` with the first reading that fits it.
    fn match_node_with_env<'tree, D: Doc>(
        &self,
        node: Node<'tree, D>,
        env: &mut Cow<MetaVarEnv<'tree, D>>,
    ) -> Option<Node<'tree, D>> {
        self.readings
            .iter()
            .find_map(|reading| reading.match_node_with_env(node.clone(), env))
    }

    /// The node kinds any reading can match, or nothing when one of them can match any kind.
    fn potential_kinds(&self) -> Option<BitSet> {
        let mut kinds = BitSet::new();
        for reading in &self.readings {
            kinds.union_with(&reading.potential_kinds()?);
        }
        Some(kinds)
    }

    /// The length of the match of the first reading that fits `node`, leaving out trailing punctuation.
    fn get_match_len<D: Doc>(&self, node: Node<'_, D>) -> Option<usize> {
        self.readings
            .iter()
            .find(|reading| reading.match_node(node.clone()).is_some())
            .and_then(|reading| reading.get_match_len(node))
    }
}

/// The code put around a pattern to read it as a statement in a function body, for languages that need it.
fn statement_context(lang: SupportLang) -> Option<(&'static str, &'static str)> {
    match lang {
        SupportLang::Go => Some(("func _() {\n", "\n}")),
        SupportLang::C | SupportLang::Cpp => Some(("void _() {\n", "\n}")),
        _ => None,
    }
}

/// Whether the parse of `code` holds an error node below the top level, or anywhere when `strict`.
fn broken(code: &str, lang: SupportLang, strict: bool) -> bool {
    let parsed = lang.ast_grep(code);
    parsed.root().dfs().any(|node| {
        node.is_error() && (strict || node.parent().is_some_and(|up| up.parent().is_some()))
    })
}

/// Compiles `source` as written, insisting that it parses cleanly as one node.
///
/// A parse error wrapping the whole pattern is allowed, since a bare
/// expression such as `return $X` is not valid at the top level of most
/// grammars yet still yields a usable node; an error anywhere inside is not.
///
/// # Errors
///
/// Returns a message for the model when the pattern does not parse, holds more
/// than one node, or is a bare `$$$` variable.
fn plain(source: &str, lang: SupportLang) -> Result<Pattern, String> {
    let name = language_name(lang);
    let pattern = Pattern::try_new(source, lang)
        .map_err(|error| format!("pattern is not a valid {name} pattern: {error}; {HINT}"))?;
    if pattern.has_error() || broken(&lang.pre_process_pattern(source), lang, false) {
        return Err(format!(
            "pattern does not parse as {name} code: {source:?}; {HINT}"
        ));
    }
    Ok(pattern)
}

/// Compiles `source` as a statement inside a function body, for the languages
/// of [`statement_context`], or nothing when that reading does not parse
/// cleanly or cannot be singled out from the code around it.
fn in_statement(source: &str, lang: SupportLang) -> Option<Pattern> {
    let (before, after) = statement_context(lang)?;
    let processed = lang.pre_process_pattern(source);
    let trimmed = processed.trim_end();
    let end = if trimmed.ends_with(';') || trimmed.ends_with('}') {
        after.to_owned()
    } else {
        format!(";{after}")
    };
    let context = format!("{before}{processed}{end}");
    if broken(&context, lang, true) {
        return None;
    }
    let span = before.len()..before.len() + processed.len();
    let parsed = lang.ast_grep(&context);
    let root = parsed.root();
    let target = root.dfs().find(|node| node.range() == span)?;
    let kind = target.kind();
    let first = root.dfs().find(|node| node.kind() == kind)?;
    if first.range() != span {
        return None;
    }
    Pattern::contextual(&context, &kind, lang).ok()
}

/// Compiles `source` for `lang` into every reading that works.
///
/// # Errors
///
/// Returns why the plain reading fails when no reading works.
fn compile(source: &str, lang: SupportLang) -> Result<Patterns, String> {
    let first = plain(source, lang);
    let second = in_statement(source, lang);
    let readings: Vec<Pattern> = first
        .as_ref()
        .ok()
        .cloned()
        .into_iter()
        .chain(second)
        .collect();
    if readings.is_empty() {
        return Err(first.err().unwrap_or_default());
    }
    Ok(Patterns { readings })
}

/// A pattern, compiled on demand for each language it meets.
pub struct Query {
    /// The pattern as the caller wrote it.
    source: String,
    /// The language every file is read as, when the caller named one.
    forced: Option<SupportLang>,
    /// The pattern compiled for each language met so far, or why it would not compile.
    compiled: HashMap<SupportLang, Result<Patterns, String>>,
}

impl Query {
    /// Prepares `source` for matching, compiling it at once when `lang` names the language.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when `source` is empty, `lang` is
    /// not a built-in language, or the pattern does not compile for it.
    pub fn new(source: &str, lang: Option<&str>) -> Result<Self, ToolError> {
        if source.trim().is_empty() {
            return Err(ToolError::InvalidArguments(
                "pattern must not be empty".into(),
            ));
        }
        let mut query = Self {
            source: source.to_owned(),
            forced: None,
            compiled: HashMap::new(),
        };
        if let Some(name) = lang {
            let lang = language(name)?;
            query.forced = Some(lang);
            if let Err(message) = query.pattern(lang) {
                return Err(ToolError::InvalidArguments(message.to_owned()));
            }
        }
        Ok(query)
    }

    /// The language to read `path` as, or nothing to skip it.
    ///
    /// With a language named by the caller, a file named directly is always read
    /// as that language, and a file found by walking only when its extension
    /// says it is written in it.
    pub fn language_of(&self, path: &Path, named_directly: bool) -> Option<SupportLang> {
        match (self.forced, named_directly) {
            (Some(lang), true) => Some(lang),
            (Some(lang), false) => detect(path).filter(|found| *found == lang),
            (None, _) => detect(path),
        }
    }

    /// The pattern compiled for `lang`, compiling it the first time.
    ///
    /// # Errors
    ///
    /// Returns why the pattern does not compile for `lang`.
    pub fn pattern(&mut self, lang: SupportLang) -> Result<&Patterns, &str> {
        let entry = match self.compiled.entry(lang) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(compile(&self.source, lang)),
        };
        entry.as_ref().map_err(String::as_str)
    }

    /// Whether the pattern compiled for at least one language.
    pub fn compiled_any(&self) -> bool {
        self.compiled.values().any(Result::is_ok)
    }

    /// Why the pattern would not compile, one line per language that rejected it, in name order.
    pub fn failures(&self) -> Vec<String> {
        let mut failures: Vec<String> = self
            .compiled
            .values()
            .filter_map(|result| result.as_ref().err().cloned())
            .collect();
        failures.sort();
        failures
    }
}

/// Lists the files under `target`, or `target` itself when it is a file,
/// honoring ignore files and the optional `glob`, in path order.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for a bad glob.
pub fn files(target: &Path, glob: Option<&str>) -> Result<Vec<PathBuf>, ToolError> {
    let mut walker = WalkBuilder::new(target);
    walker.require_git(false).sort_by_file_path(|a, b| a.cmp(b));
    if let Some(glob) = glob {
        let overrides = OverrideBuilder::new(target)
            .add(glob)
            .and_then(|builder| builder.build())
            .map_err(|e| ToolError::InvalidArguments(format!("invalid glob: {e}")))?;
        walker.overrides(overrides);
    }
    Ok(walker
        .build()
        .flatten()
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect())
}

/// Reads `path` as UTF-8 text, or nothing when it is too large, unreadable or not text.
pub fn read_source(path: &Path) -> Option<String> {
    let size = std::fs::metadata(path).ok()?.len();
    if size > MAX_FILE_BYTES {
        return None;
    }
    String::from_utf8(std::fs::read(path).ok()?).ok()
}

/// The metavariables `pattern` binds in `found`, each as its name with `$` or `$$$` and the code it matched, in name order.
pub fn captures(pattern: &Patterns, found: &Match<'_>, source: &str) -> Vec<(String, String)> {
    let mut names: Vec<&str> = pattern.defined_vars().into_iter().collect();
    names.sort_unstable();
    let env = found.get_env();
    names
        .into_iter()
        .filter_map(|name| {
            if let Some(node) = env.get_match(name) {
                return Some((format!("${name}"), node.text().into_owned()));
            }
            let nodes = env.get_multiple_matches(name);
            let first = nodes.first()?.range().start;
            let last = nodes.last()?.range().end;
            Some((
                format!("$$${name}"),
                source.get(first..last).unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

/// Squeezes the whitespace of `code` to single spaces and cuts it to [`MAX_CAPTURE_CHARS`] characters, for a one-line view.
pub fn one_line(code: &str) -> String {
    let squeezed = code.split_whitespace().collect::<Vec<_>>().join(" ");
    match squeezed.char_indices().nth(MAX_CAPTURE_CHARS) {
        Some((end, _)) => format!("{}…", &squeezed[..end]),
        None => squeezed,
    }
}

/// Returns the 0-indexed line of `text` that byte `offset` falls on.
pub fn line_at(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Common patterns compile in every language they are written for.
    #[test]
    fn compiles_common_patterns() {
        let cases = [
            ("rust", "$A.unwrap()"),
            ("rust", "fn $NAME($$$ARGS) { $$$BODY }"),
            ("rust", "let $X = $Y;"),
            ("rust", "return $X"),
            ("typescript", "console.log($$$ARGS)"),
            ("tsx", "<$TAG $$$PROPS />"),
            ("javascript", "const $X = require($M)"),
            ("python", "def $F($$$ARGS): $$$BODY"),
            ("python", "print($X)"),
            ("go", "fmt.Println($$$A)"),
            ("go", "if err != nil { $$$B }"),
            ("c", "malloc($N)"),
            ("cpp", "std::move($X)"),
            ("java", "System.out.println($X)"),
            ("lua", "print($X)"),
        ];
        for (lang, source) in cases {
            assert!(
                Query::new(source, Some(lang)).is_ok(),
                "{lang}: {source} should compile"
            );
        }
    }

    /// Fragments, several nodes and unknown languages are refused with a reason.
    #[test]
    fn refuses_bad_patterns_and_languages() {
        let fragment = Query::new("fn foo(", Some("rust"));
        assert!(matches!(fragment, Err(ToolError::InvalidArguments(m)) if m.contains("rust")));
        let inner = Query::new("foo(a b)", Some("rust"));
        assert!(matches!(inner, Err(ToolError::InvalidArguments(_))));
        let several = Query::new("let a = 1; let b = 2;", Some("rust"));
        assert!(matches!(several, Err(ToolError::InvalidArguments(_))));
        let bare = Query::new("$$$A", Some("python"));
        assert!(matches!(bare, Err(ToolError::InvalidArguments(_))));
        let unknown = Query::new("x", Some("cobol"));
        assert!(matches!(unknown, Err(ToolError::InvalidArguments(m)) if m.contains("rust")));
        assert!(Query::new("x", Some("ts")).is_ok());
    }

    /// Without a named language the extension decides; with one, walked files must agree.
    #[test]
    fn picks_languages_by_extension() {
        let open = Query::new("x", None).unwrap();
        assert_eq!(
            open.language_of(Path::new("a.rs"), false),
            Some(SupportLang::Rust)
        );
        assert_eq!(open.language_of(Path::new("a.txt"), false), None);
        let rust = Query::new("x", Some("rust")).unwrap();
        assert_eq!(rust.language_of(Path::new("a.py"), false), None);
        assert_eq!(
            rust.language_of(Path::new("a.txt"), true),
            Some(SupportLang::Rust)
        );
    }
}
