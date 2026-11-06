//! What the harness asks of the client's language services, and what comes back.
//!
//! A client that runs language servers of its own says so when it opens a conversation, and the harness then asks it, instead of starting servers itself, to format a file it has just written and to say what is wrong with the files it has changed. The messages are the harness's own, not the language server protocol, so a client may answer them however it likes.

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

/// What the harness asks the client's language services.
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
}
