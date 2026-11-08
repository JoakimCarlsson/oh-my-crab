//! What the harness asks of the client's language services, and what comes back.
//!
//! A client that runs language servers of its own says so when it opens a conversation, and the harness then asks it, instead of starting servers itself, to format a file it has just written and to say what is wrong with the files it has changed. A client that also says it navigates code is asked, besides, where symbols are defined and used, what they are, which symbols a file or the project has, what is wrong with the whole project, how to rename a symbol, which code actions are offered and what applying one changes, how to format a stretch of a file, what moving a file changes, and which servers run, and to restart them. The messages are the harness's own, not the language server protocol, so a client may answer them however it likes.
//!
//! Every place is counted the way the harness counts them: lines and characters from one, a character being a Unicode scalar value, not a UTF-16 code unit.

use std::path::PathBuf;

/// How bad a diagnostic is.
#[derive(
    Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The code is wrong.
    Error,
    /// The code may be wrong.
    Warning,
    /// Something worth knowing.
    Information,
    /// A suggestion.
    Hint,
}

impl Severity {
    /// The word the severity is shown by.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Information => "info",
            Severity::Hint => "hint",
        }
    }
}

/// Something a language service says about a place in a file.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Diagnostic {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// The line it starts on, counted from one.
    pub line: u32,
    /// The character of the line it starts at, counted from one.
    pub column: u32,
    /// How bad it is.
    pub severity: Severity,
    /// What it says.
    pub message: String,
    /// What said it, such as the compiler or a linter, when that is known.
    #[serde(default)]
    pub source: Option<String>,
    /// The code of the diagnostic, such as `E0308`, when it has one.
    #[serde(default)]
    pub code: Option<String>,
}

/// A place in a file the harness asks about: the file, and the line and character in it, both counted from one.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Place {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// The line, counted from one.
    pub line: u32,
    /// The character of the line, counted from one.
    pub column: u32,
}

/// A stretch of a file the client's language services point at.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stretch {
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

/// A symbol the client's language services know: a function, type, field, module and so on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SymbolInfo {
    /// Its name.
    pub name: String,
    /// What kind of symbol it is, in lower case, such as `function` or `struct`.
    pub kind: String,
    /// More about it, such as a function's signature, when the server says.
    #[serde(default)]
    pub detail: Option<String>,
    /// The symbol it is inside of, such as the type of a method, when the server says.
    #[serde(default)]
    pub container: Option<String>,
    /// How deeply it is nested in the symbols of its file, from nought for the outermost; nought for the symbols of a project.
    #[serde(default)]
    pub depth: u32,
    /// Where its name is.
    pub location: Stretch,
}

/// One stretch of a file's text replaced: from the line and character it starts at to the line and character it ends before, all counted from one, in the text as it is on disk.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TextEdit {
    /// The line the stretch starts on, counted from one.
    pub line: u32,
    /// The character of that line it starts at, counted from one.
    pub column: u32,
    /// The line it ends on, counted from one.
    pub end_line: u32,
    /// The character of that line it ends before, counted from one.
    pub end_column: u32,
    /// What replaces it.
    pub text: String,
}

/// The edits a rename makes to one file, each placed in the file's text as it is on disk before any of them.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FileEdits {
    /// The file, as an absolute path.
    pub path: PathBuf,
    /// The edits, in any order; none of them overlap.
    pub edits: Vec<TextEdit>,
}

/// What the harness asks the client's language services.
///
/// Only [`LanguageAsk::Format`] and [`LanguageAsk::Diagnostics`] are asked of a client that set [`crate::Opening::languages`] alone; the others, those added later among them, are asked only of a client that also set [`crate::Opening::navigation`].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageAsk {
    /// The file at `path`, as it is on disk now, formatted as its language says; answered with [`LanguageAnswer::Formatted`].
    Format {
        /// The file, as an absolute path.
        path: PathBuf,
    },
    /// What is wrong with the files at `paths`, as they are on disk now; answered with [`LanguageAnswer::Diagnostics`].
    ///
    /// The files were just written. The client waits up to `wait_ms` milliseconds for its servers to say what they find in the new text, then answers with what it has.
    Diagnostics {
        /// The files, as absolute paths.
        paths: Vec<PathBuf>,
        /// How long the client may wait for fresh diagnostics, in milliseconds.
        wait_ms: u64,
    },
    /// Where the symbol at `at` is defined; answered with [`LanguageAnswer::Locations`].
    Definition {
        /// The symbol's place.
        at: Place,
    },
    /// Where the type of the symbol at `at` is defined; answered with [`LanguageAnswer::Locations`].
    TypeDefinition {
        /// The symbol's place.
        at: Place,
    },
    /// Where the symbol at `at`, a trait, interface or abstract method, is implemented; answered with [`LanguageAnswer::Locations`].
    Implementation {
        /// The symbol's place.
        at: Place,
    },
    /// Everywhere the symbol at `at` is used, its declaration included; answered with [`LanguageAnswer::Locations`].
    References {
        /// The symbol's place.
        at: Place,
    },
    /// What the symbol at `at` is, its type and documentation; answered with [`LanguageAnswer::Hover`].
    Hover {
        /// The symbol's place.
        at: Place,
    },
    /// The symbols of the file at `path`, outermost first and each followed by those inside it; answered with [`LanguageAnswer::Symbols`].
    DocumentSymbols {
        /// The file, as an absolute path.
        path: PathBuf,
    },
    /// The symbols of the project whose names match `query`; answered with [`LanguageAnswer::Symbols`].
    WorkspaceSymbols {
        /// What to match the names against.
        query: String,
        /// A file whose language's server to ask, or nothing to ask every server of the project.
        #[serde(default)]
        near: Option<PathBuf>,
    },
    /// Everything the servers of the project have said is wrong with any of its files; answered with [`LanguageAnswer::Diagnostics`].
    WorkspaceDiagnostics {
        /// How long the client may wait for servers still loading or checking the project, in milliseconds.
        wait_ms: u64,
    },
    /// The edits that rename the symbol at `at` to `new_name` in every file that uses it, made to the files as they are on disk now; answered with [`LanguageAnswer::Edits`].
    ///
    /// The client writes nothing: the harness makes the edits itself, asking the reader first when it must, and then formats the files and checks them.
    Rename {
        /// The symbol's place.
        at: Place,
        /// The name to rename it to.
        new_name: String,
    },
    /// The whole text of the file `range` names, as it is on disk now, with only that stretch of it formatted; answered with [`LanguageAnswer::Formatted`].
    FormatRange {
        /// The stretch to format, in the file it names.
        range: Stretch,
    },
    /// The code actions the server of the file offers for the stretch `range` of it: quick fixes for what is wrong there, refactorings and the like; answered with [`LanguageAnswer::Actions`].
    CodeActions {
        /// The stretch, in the file it names.
        range: Stretch,
        /// Only actions whose kinds start with one of these, such as `quickfix`; every action when empty.
        #[serde(default)]
        only: Vec<String>,
    },
    /// The edits applying one of the code actions offered for `range` and `only` makes, as [`LanguageAsk::CodeActions`] would list them: the action titled `title` when there is one, otherwise the `index`-th, counted from one; answered with [`LanguageAnswer::Applied`].
    ///
    /// The client asks for the actions again, resolves the chosen one when its server needs that, and, when it has a command, has the server carry it out, keeping the edits the server asks to have made while it does instead of making them. It writes nothing: the harness makes the edits itself, as it does a rename's. An action that would create, move or delete files is answered with [`LanguageAnswer::Failed`].
    ApplyCodeAction {
        /// The stretch the action was offered for, in the file it names.
        range: Stretch,
        /// The kinds the actions were narrowed to, as in [`LanguageAsk::CodeActions`].
        #[serde(default)]
        only: Vec<String>,
        /// The action's place in the list, counted from one.
        index: u32,
        /// The action's title, when the harness knows it.
        #[serde(default)]
        title: Option<String>,
    },
    /// The edits the servers of the project make before the file at `from` is moved to `to`, such as to the imports that name it, made to the files as they are on disk now; answered with [`LanguageAnswer::Edits`]. Nothing is moved or written: the harness moves the file and makes the edits, then sends [`LanguageAsk::FileRenamed`].
    WillRenameFile {
        /// The file, as an absolute path.
        from: PathBuf,
        /// Where it is going, as an absolute path.
        to: PathBuf,
    },
    /// The file at `from` has been moved to `to`; the client tells its servers, and answers [`LanguageAnswer::Done`].
    FileRenamed {
        /// Where it was, as an absolute path.
        from: PathBuf,
        /// Where it is now, as an absolute path.
        to: PathBuf,
    },
    /// The client's language servers for the project and how each stands; answered with [`LanguageAnswer::Servers`].
    Status {},
    /// Stops and starts again the server named `server`, or the one that reads the file at `near`, or every server running for the project when neither is given; answered with [`LanguageAnswer::Servers`], listing those restarted.
    Reload {
        /// The name of the server, as [`LanguageAnswer::Servers`] gives it.
        #[serde(default)]
        server: Option<String>,
        /// A file whose server to restart.
        #[serde(default)]
        near: Option<PathBuf>,
    },
}

/// A code action a server offers: a quick fix, a refactoring, organising imports and so on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActionInfo {
    /// What the server calls it.
    pub title: String,
    /// Its kind, such as `quickfix` or `refactor.extract`, when the server says.
    #[serde(default)]
    pub kind: Option<String>,
    /// Whether the server prefers it among those offered.
    #[serde(default)]
    pub preferred: bool,
    /// Why it cannot be applied now, when the server offers it but says so.
    #[serde(default)]
    pub disabled: Option<String>,
}

/// One of the client's language servers and how it stands.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ServerInfo {
    /// The name it is known by, such as `rust-analyzer`.
    pub name: String,
    /// The languages it reads.
    #[serde(default)]
    pub languages: Vec<String>,
    /// How it stands, in a word or two, such as `running`, `loading`, `starting`, `stopped` or `failed`.
    pub state: String,
    /// More about it, such as why it failed.
    #[serde(default)]
    pub detail: Option<String>,
}

/// One question to the client's language services, under a ticket the answer names.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct LanguageRequest {
    /// The ticket the answer is sent under, with [`crate::Command::Language`].
    pub ticket: u64,
    /// What is asked.
    pub ask: LanguageAsk,
}

/// What the client's language services answer.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanguageAnswer {
    /// The whole formatted text of the file, or nothing when it cannot be formatted or is formatted already.
    Formatted {
        /// The formatted text.
        text: Option<String>,
    },
    /// What is wrong with the files asked about.
    Diagnostics {
        /// What the servers said, in any order.
        diagnostics: Vec<Diagnostic>,
    },
    /// The places a definition, type definition, implementation or references question found; empty when there are none.
    Locations {
        /// The places, in any order.
        locations: Vec<Stretch>,
    },
    /// What the symbol asked about is, as plain text or Markdown, or nothing when the server has nothing to say about it.
    Hover {
        /// What the server said.
        text: Option<String>,
    },
    /// The symbols of a file or of the project.
    Symbols {
        /// The symbols, a file's outermost first and each followed by those inside it.
        symbols: Vec<SymbolInfo>,
    },
    /// The edits a rename makes, by file; the client has written none of them.
    Edits {
        /// The files the rename changes, each with its edits.
        files: Vec<FileEdits>,
    },
    /// The client's language services, or the server of the file, do not answer this kind of question.
    Unsupported {
        /// Why, as the model is told it.
        reason: String,
    },
    /// No language server reads files like the one asked about, or the one that would is not installed or could not be started.
    NoServer {
        /// Why, as the model is told it.
        reason: String,
    },
    /// The server was asked and failed, or refused, as a rename that would create, move or delete files is.
    Failed {
        /// Why, as the model is told it.
        reason: String,
    },
    /// The code actions offered, in the order the server gave them, which [`LanguageAsk::ApplyCodeAction`] counts by.
    Actions {
        /// The actions.
        actions: Vec<ActionInfo>,
    },
    /// The edits applying a code action makes, by file; the client has written none of them.
    Applied {
        /// The title of the action applied.
        title: String,
        /// The files it changes, each with its edits.
        files: Vec<FileEdits>,
    },
    /// The client's language servers and how each stands.
    Servers {
        /// The servers.
        servers: Vec<ServerInfo>,
    },
    /// What was asked has been done.
    Done {},
}

#[cfg(test)]
/// Tests for the language messages.
mod tests {
    use super::*;

    /// A request and its answer read back as they were written.
    #[test]
    fn messages_round_trip() {
        let request = LanguageRequest {
            ticket: 7,
            ask: LanguageAsk::Diagnostics {
                paths: vec![PathBuf::from("/w/src/lib.rs")],
                wait_ms: 3000,
            },
        };
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(
            serde_json::from_str::<LanguageRequest>(&text).unwrap(),
            request
        );
        let answer = LanguageAnswer::Diagnostics {
            diagnostics: vec![Diagnostic {
                path: PathBuf::from("/w/src/lib.rs"),
                line: 3,
                column: 9,
                severity: Severity::Error,
                message: "mismatched types".into(),
                source: Some("rustc".into()),
                code: Some("E0308".into()),
            }],
        };
        let text = serde_json::to_string(&answer).unwrap();
        assert_eq!(
            serde_json::from_str::<LanguageAnswer>(&text).unwrap(),
            answer
        );
    }

    /// The navigation questions and answers read back as they were written, with snake_case tags, and a workspace symbol search that leaves out `near` reads as one with none.
    #[test]
    fn navigation_messages_round_trip() {
        let at = Place {
            path: PathBuf::from("/w/src/lib.rs"),
            line: 3,
            column: 8,
        };
        let asks = [
            LanguageAsk::References { at: at.clone() },
            LanguageAsk::TypeDefinition { at: at.clone() },
            LanguageAsk::Rename {
                at,
                new_name: "total".into(),
            },
            LanguageAsk::WorkspaceDiagnostics { wait_ms: 3000 },
        ];
        for ask in asks {
            let text = serde_json::to_string(&ask).unwrap();
            assert_eq!(serde_json::from_str::<LanguageAsk>(&text).unwrap(), ask);
        }
        assert!(
            serde_json::to_string(&LanguageAsk::TypeDefinition {
                at: Place {
                    path: PathBuf::from("/w/a.rs"),
                    line: 1,
                    column: 1
                }
            })
            .unwrap()
            .starts_with(r#"{"type_definition":"#)
        );
        assert_eq!(
            serde_json::from_str::<LanguageAsk>(r#"{"workspace_symbols":{"query":"Config"}}"#)
                .unwrap(),
            LanguageAsk::WorkspaceSymbols {
                query: "Config".into(),
                near: None
            }
        );
        let answer = LanguageAnswer::Edits {
            files: vec![FileEdits {
                path: PathBuf::from("/w/src/lib.rs"),
                edits: vec![TextEdit {
                    line: 3,
                    column: 8,
                    end_line: 3,
                    end_column: 13,
                    text: "total".into(),
                }],
            }],
        };
        let text = serde_json::to_string(&answer).unwrap();
        assert_eq!(
            serde_json::from_str::<LanguageAnswer>(&text).unwrap(),
            answer
        );
        assert_eq!(
            serde_json::from_str::<LanguageAnswer>(r#"{"no_server":{"reason":"none"}}"#).unwrap(),
            LanguageAnswer::NoServer {
                reason: "none".into()
            }
        );
    }

    /// The questions about code actions, formatting a stretch, moving files and the servers, and their answers, read back as they were written; fields left out read as empty, and the asks without fields are written as empty objects.
    #[test]
    fn action_and_server_messages_round_trip() {
        let range = Stretch {
            path: PathBuf::from("/w/src/lib.rs"),
            line: 3,
            column: 1,
            end_line: 5,
            end_column: 2,
        };
        let asks = [
            LanguageAsk::FormatRange {
                range: range.clone(),
            },
            LanguageAsk::CodeActions {
                range: range.clone(),
                only: vec!["quickfix".into()],
            },
            LanguageAsk::ApplyCodeAction {
                range: range.clone(),
                only: Vec::new(),
                index: 2,
                title: Some("Import `HashMap`".into()),
            },
            LanguageAsk::WillRenameFile {
                from: PathBuf::from("/w/src/a.rs"),
                to: PathBuf::from("/w/src/b.rs"),
            },
            LanguageAsk::FileRenamed {
                from: PathBuf::from("/w/src/a.rs"),
                to: PathBuf::from("/w/src/b.rs"),
            },
            LanguageAsk::Status {},
            LanguageAsk::Reload {
                server: Some("rust-analyzer".into()),
                near: None,
            },
        ];
        for ask in asks {
            let text = serde_json::to_string(&ask).unwrap();
            assert_eq!(serde_json::from_str::<LanguageAsk>(&text).unwrap(), ask);
        }
        assert_eq!(
            serde_json::to_string(&LanguageAsk::Status {}).unwrap(),
            r#"{"status":{}}"#
        );
        assert_eq!(
            serde_json::from_str::<LanguageAsk>(r#"{"reload":{}}"#).unwrap(),
            LanguageAsk::Reload {
                server: None,
                near: None
            }
        );
        let answers = [
            LanguageAnswer::Actions {
                actions: vec![ActionInfo {
                    title: "Import `HashMap`".into(),
                    kind: Some("quickfix".into()),
                    preferred: true,
                    disabled: None,
                }],
            },
            LanguageAnswer::Applied {
                title: "Import `HashMap`".into(),
                files: Vec::new(),
            },
            LanguageAnswer::Servers {
                servers: vec![ServerInfo {
                    name: "rust-analyzer".into(),
                    languages: vec!["rust".into()],
                    state: "running".into(),
                    detail: None,
                }],
            },
            LanguageAnswer::Done {},
        ];
        for answer in answers {
            let text = serde_json::to_string(&answer).unwrap();
            assert_eq!(
                serde_json::from_str::<LanguageAnswer>(&text).unwrap(),
                answer
            );
        }
    }
}
