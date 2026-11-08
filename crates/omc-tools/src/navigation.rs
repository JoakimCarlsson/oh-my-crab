//! What a language service can be asked about code besides formatting it and saying what is wrong with it: where things are defined and used, what they are, which symbols a file or the project has, and how to rename one.
//!
//! Every place here is counted the way people and the tools count them: lines and characters from one, a character being a Unicode scalar value. A service that speaks the language server protocol converts to and from its UTF-16 positions itself.

use std::fmt;
use std::path::PathBuf;

/// A place in a file: the file, and the line and character in it, both counted from one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Spot {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// The line, counted from one.
    pub line: u32,
    /// The character of the line, counted from one.
    pub column: u32,
}

/// A stretch of a file a language service points at.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Location {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// The line it starts on, counted from one.
    pub line: u32,
    /// The character of that line it starts at, counted from one.
    pub column: u32,
    /// The line it ends on, counted from one.
    pub end_line: u32,
    /// The character of that line it ends before, counted from one.
    pub end_column: u32,
}

/// What to look for from a place in the code.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Lookup {
    /// Where the symbol is defined.
    Definition,
    /// Where the type of the symbol is defined.
    TypeDefinition,
    /// Where the symbol, a trait, interface or abstract method, is implemented.
    Implementation,
    /// Everywhere the symbol is used, its declaration included.
    References,
}

impl Lookup {
    /// The words the results of the lookup are named by, such as `definition`.
    pub fn noun(self) -> &'static str {
        match self {
            Lookup::Definition => "definition",
            Lookup::TypeDefinition => "type definition",
            Lookup::Implementation => "implementation",
            Lookup::References => "reference",
        }
    }
}

/// A symbol a language service knows: a function, type, field, module and so on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Symbol {
    /// Its name.
    pub name: String,
    /// What kind of symbol it is, in lower case, such as `function` or `struct`.
    pub kind: String,
    /// More about it, such as a function's signature, when the service says.
    pub detail: Option<String>,
    /// The symbol it is inside of, such as the type of a method, when the service says.
    pub container: Option<String>,
    /// How deeply it is nested in the symbols of its file, from nought for the outermost; nought for the symbols of a project.
    pub depth: usize,
    /// Where its name is.
    pub location: Location,
}

/// One file a rename, a code action or the move of another file changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Renamed {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// Its whole text with the change made.
    pub text: String,
    /// How many places in it the change touches.
    pub edits: usize,
}

/// A change a language server offers for a stretch of a file: a quick fix, a refactoring, organising imports and so on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CodeAction {
    /// What the server calls it, as the reader would see it in an editor.
    pub title: String,
    /// Its kind, such as `quickfix` or `refactor.extract`, when the server says.
    pub kind: Option<String>,
    /// Whether the server prefers it among those offered, as the fix it would apply first.
    pub preferred: bool,
    /// Why it cannot be applied now, when the server offers it but says so.
    pub disabled: Option<String>,
}

/// What applying a code action changes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Applied {
    /// The title of the action applied.
    pub title: String,
    /// The files it changes, each with its whole new text made from its text on disk now; nothing is written.
    pub files: Vec<Renamed>,
}

/// The code action to apply among those offered for a stretch of a file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chosen {
    /// Its place in the list offered, counted from one.
    pub index: usize,
    /// Its title, when the caller knows it; an action of that title is taken over the one at `index` when the list has changed.
    pub title: Option<String>,
}

/// One language server of the conversation and how it stands.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerStatus {
    /// The name it is known by, such as `rust-analyzer`.
    pub name: String,
    /// The languages it reads.
    pub languages: Vec<String>,
    /// How it stands, in a word or two: `running`, `loading`, `starting`, `stopped`, `failed`, `not started` or `not installed`.
    pub state: String,
    /// More about it, such as why it failed or how many times it was started again, when there is more to say.
    pub detail: Option<String>,
}

/// Why a language service could not answer a question about code.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LanguageError {
    /// No language server reads files like the one asked about, or none is installed or could be started.
    NoServer(String),
    /// The language services, or the server of the file, do not answer this kind of question.
    Unsupported(String),
    /// The server was asked and failed.
    Failed(String),
}

impl LanguageError {
    /// The reason given, whatever kind of failure it is.
    pub fn reason(&self) -> &str {
        match self {
            LanguageError::NoServer(reason)
            | LanguageError::Unsupported(reason)
            | LanguageError::Failed(reason) => reason,
        }
    }
}

impl fmt::Display for LanguageError {
    /// Writes the reason.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.reason())
    }
}

impl std::error::Error for LanguageError {}

/// What a language service that only formats files and says what is wrong with them answers when asked to navigate code.
pub(crate) fn cannot_navigate() -> LanguageError {
    LanguageError::Unsupported(
        "these language services only format files and report their diagnostics; they do not navigate code"
            .to_owned(),
    )
}
