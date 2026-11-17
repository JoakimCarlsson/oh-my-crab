//! What a conversation and the language server daemon say to each other.
//!
//! The two talk over a Unix socket in JSON lines. The conversation opens with a [`Hello`], naming the version of the protocol it speaks and the project it works in, and the daemon answers with a [`Welcome`] naming its own. From then on the conversation sends [`Request`]s and the daemon answers each with a [`Response`] under the same id, in whatever order the answers are ready. The questions and answers are those a client's language services are asked, [`omc_wire::LanguageAsk`] and [`omc_wire::LanguageAnswer`], with a few of the daemon's own for what those cannot say.

use std::path::{Path, PathBuf};
use std::time::Duration;

use omc_tools::{
    Applied, CodeAction, LanguageError, Location, Renamed, ServerStatus, Spot, Symbol,
};
use omc_wire::{ActionInfo, LanguageAnswer, LanguageAsk, Place, ServerInfo, Stretch, SymbolInfo};

/// The version of the protocol this build speaks; a conversation and a daemon that name different versions do not talk.
pub(crate) const PROTOCOL_VERSION: u32 = 1;

/// The environment variable that names the socket the daemon listens on, instead of the one in the user's runtime folder.
pub const SOCKET_VARIABLE: &str = "OMC_LSP_SOCKET";

/// How long the servers of a project nobody asks about are kept, and how long a daemon nobody is connected to stays.
pub(crate) const IDLE: Duration = Duration::from_secs(600);

/// The first line a conversation sends: who it is and which project's servers it wants.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Hello {
    /// The version of the protocol the conversation speaks.
    pub(crate) version: u32,
    /// The root of the project, as an absolute path.
    pub(crate) root: PathBuf,
    /// Whether the agent is trusted with everything, so that the project's servers start at once instead of on the first question.
    pub(crate) trusted: bool,
}

/// The first line the daemon sends, in answer to a [`Hello`].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Welcome {
    /// The version of the protocol the daemon speaks; when it is not the conversation's, the daemon hangs up after saying so.
    pub(crate) version: u32,
    /// The process id of the daemon.
    #[serde(default)]
    pub(crate) pid: u32,
}

/// What a conversation asks the daemon.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Ask {
    /// A question a client's language services could be asked as well.
    Language(LanguageAsk),
    /// How long to wait for the diagnostics of the files at `paths`; answered with [`Answer::Patience`].
    Patience {
        /// The files, as absolute paths.
        paths: Vec<PathBuf>,
    },
    /// Whether a server reads the file at `path`; answered with [`LanguageAnswer::Done`] or a failure.
    Serves {
        /// The file, as an absolute path.
        path: PathBuf,
    },
    /// The whole file at `path` formatted, with the reasons it cannot be told apart; answered with [`LanguageAnswer::Formatted`] or a failure.
    Reformat {
        /// The file, as an absolute path.
        path: PathBuf,
    },
}

/// A file with the whole text a rename, a code action or a move gives it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Rewrite {
    /// The file, as an absolute path.
    pub(crate) path: PathBuf,
    /// Its whole new text.
    pub(crate) text: String,
    /// How many edits were made to it.
    pub(crate) edits: u64,
}

/// What the daemon answers.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Answer {
    /// An answer a client's language services could give as well.
    Language(LanguageAnswer),
    /// How long to wait for diagnostics.
    Patience {
        /// The wait, in milliseconds.
        wait_ms: u64,
    },
    /// The files a rename or a move changes, each with its whole new text; none is written.
    Rewritten {
        /// The files.
        files: Vec<Rewrite>,
    },
    /// The files applying a code action changes, each with its whole new text; none is written.
    Applied {
        /// The title of the action applied.
        title: String,
        /// The files.
        files: Vec<Rewrite>,
    },
}

/// One question to the daemon, under an id its answer names.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Request {
    /// The id the answer is sent under; the conversation never uses one twice on a connection.
    pub(crate) id: u64,
    /// What is asked.
    pub(crate) ask: Ask,
}

/// The daemon's answer to the [`Request`] with the same id.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Response {
    /// The id of the request answered.
    pub(crate) id: u64,
    /// The answer.
    pub(crate) answer: Answer,
}

/// The id of a request whose question could not be read, so that it can still be answered with a failure.
#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize)]
pub(crate) struct Unread {
    /// The id of the request.
    pub(crate) id: u64,
}

/// The socket the daemon listens on: the one [`SOCKET_VARIABLE`] names, otherwise `oh-my-crab/lsp/daemon.sock` in the user's runtime folder, or in the user's cache folder on a system without one; nothing when there is neither.
pub(crate) fn socket_path() -> Option<PathBuf> {
    match std::env::var_os(SOCKET_VARIABLE) {
        Some(named) if !named.is_empty() => Some(PathBuf::from(named)),
        _ => Some(default_folder()?.join("daemon.sock")),
    }
}

/// The folder the daemon's socket and lock are kept in unless [`SOCKET_VARIABLE`] names another, readable by the user alone.
pub(crate) fn default_folder() -> Option<PathBuf> {
    let base = dirs::runtime_dir().or_else(dirs::cache_dir)?;
    Some(base.join("oh-my-crab").join("lsp"))
}

/// The file beside the socket at `socket` that the one daemon of the user holds locked while it runs.
pub(crate) fn lock_path(socket: &Path) -> PathBuf {
    let mut named = socket.as_os_str().to_owned();
    named.push(".lock");
    PathBuf::from(named)
}

/// `wait` in whole milliseconds, as the messages carry it.
pub(crate) fn millis(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX)
}

/// `spot` as the messages place it.
pub(crate) fn place(spot: &Spot) -> Place {
    Place {
        path: spot.path.clone(),
        line: spot.line,
        column: spot.column,
    }
}

/// The place `place` of a message, as the tools know it.
pub(crate) fn spot(place: Place) -> Spot {
    Spot {
        path: place.path,
        line: place.line,
        column: place.column,
    }
}

/// `location` as the messages place a stretch.
pub(crate) fn stretch(location: &Location) -> Stretch {
    Stretch {
        path: location.path.clone(),
        line: location.line,
        column: location.column,
        end_line: location.end_line,
        end_column: location.end_column,
    }
}

/// The stretch `stretch` of a message, as the tools place it.
pub(crate) fn location(stretch: Stretch) -> Location {
    Location {
        path: stretch.path,
        line: stretch.line,
        column: stretch.column,
        end_line: stretch.end_line,
        end_column: stretch.end_column,
    }
}

/// `symbol` as the messages describe it.
pub(crate) fn symbol_info(symbol: Symbol) -> SymbolInfo {
    SymbolInfo {
        name: symbol.name,
        kind: symbol.kind,
        detail: symbol.detail,
        container: symbol.container,
        depth: u32::try_from(symbol.depth).unwrap_or(u32::MAX),
        location: stretch(&symbol.location),
    }
}

/// The symbol `info` of a message, as the tools know it.
pub(crate) fn symbol(info: SymbolInfo) -> Symbol {
    Symbol {
        name: info.name,
        kind: info.kind,
        detail: info.detail,
        container: info.container,
        depth: usize::try_from(info.depth).unwrap_or(usize::MAX),
        location: location(info.location),
    }
}

/// `action` as the messages describe it.
pub(crate) fn action_info(action: CodeAction) -> ActionInfo {
    ActionInfo {
        title: action.title,
        kind: action.kind,
        preferred: action.preferred,
        disabled: action.disabled,
    }
}

/// The code action `info` of a message, as the tools know it.
pub(crate) fn action(info: ActionInfo) -> CodeAction {
    CodeAction {
        title: info.title,
        kind: info.kind,
        preferred: info.preferred,
        disabled: info.disabled,
    }
}

/// `status` as the messages describe a server.
pub(crate) fn server_info(status: ServerStatus) -> ServerInfo {
    ServerInfo {
        name: status.name,
        languages: status.languages,
        state: status.state,
        detail: status.detail,
    }
}

/// The server `info` of a message, as the tools know it.
pub(crate) fn server(info: ServerInfo) -> ServerStatus {
    ServerStatus {
        name: info.name,
        languages: info.languages,
        state: info.state,
        detail: info.detail,
    }
}

/// `renamed` as the messages carry a file's new text.
pub(crate) fn rewrite(renamed: Renamed) -> Rewrite {
    Rewrite {
        path: renamed.path,
        text: renamed.text,
        edits: u64::try_from(renamed.edits).unwrap_or(u64::MAX),
    }
}

/// The new text `rewrite` of a message, as the tools know it.
pub(crate) fn renamed(rewrite: Rewrite) -> Renamed {
    Renamed {
        path: rewrite.path,
        text: rewrite.text,
        edits: usize::try_from(rewrite.edits).unwrap_or(usize::MAX),
    }
}

/// `applied` as the messages carry what a code action changes.
pub(crate) fn applied_answer(applied: Applied) -> Answer {
    Answer::Applied {
        title: applied.title,
        files: applied.files.into_iter().map(rewrite).collect(),
    }
}

/// `error` as the messages carry a failure.
pub(crate) fn failure(error: LanguageError) -> Answer {
    Answer::Language(match error {
        LanguageError::NoServer(reason) => LanguageAnswer::NoServer { reason },
        LanguageError::Unsupported(reason) => LanguageAnswer::Unsupported { reason },
        LanguageError::Failed(reason) => LanguageAnswer::Failed { reason },
    })
}

/// `answer` unless it is a failure, which is returned as the error the tools know.
///
/// # Errors
///
/// Returns the [`LanguageError`] the daemon answered with.
pub(crate) fn settled(answer: Answer) -> Result<Answer, LanguageError> {
    match answer {
        Answer::Language(LanguageAnswer::NoServer { reason }) => {
            Err(LanguageError::NoServer(reason))
        }
        Answer::Language(LanguageAnswer::Unsupported { reason }) => {
            Err(LanguageError::Unsupported(reason))
        }
        Answer::Language(LanguageAnswer::Failed { reason }) => Err(LanguageError::Failed(reason)),
        answer => Ok(answer),
    }
}

#[cfg(test)]
/// Tests for the daemon's messages.
mod tests {
    use super::*;

    /// The greeting, a request of each kind and the answers of the daemon's own read back as they were written, with snake_case tags around the shared questions.
    #[test]
    fn messages_round_trip() {
        let hello = Hello {
            version: PROTOCOL_VERSION,
            root: PathBuf::from("/w"),
            trusted: true,
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert_eq!(text, r#"{"version":1,"root":"/w","trusted":true}"#);
        assert_eq!(serde_json::from_str::<Hello>(&text).unwrap(), hello);
        assert_eq!(
            serde_json::from_str::<Welcome>(r#"{"version":7}"#).unwrap(),
            Welcome { version: 7, pid: 0 }
        );
        let asks = [
            Ask::Language(LanguageAsk::Hover {
                at: Place {
                    path: PathBuf::from("/w/a.rs"),
                    line: 3,
                    column: 8,
                },
            }),
            Ask::Patience {
                paths: vec![PathBuf::from("/w/a.rs")],
            },
            Ask::Serves {
                path: PathBuf::from("/w/a.rs"),
            },
            Ask::Reformat {
                path: PathBuf::from("/w/a.rs"),
            },
        ];
        for (id, ask) in (1..).zip(asks) {
            let request = Request { id, ask };
            let text = serde_json::to_string(&request).unwrap();
            assert_eq!(serde_json::from_str::<Request>(&text).unwrap(), request);
            assert_eq!(serde_json::from_str::<Unread>(&text).unwrap().id, id);
        }
        assert_eq!(
            serde_json::to_string(&Request {
                id: 4,
                ask: Ask::Language(LanguageAsk::Status {}),
            })
            .unwrap(),
            r#"{"id":4,"ask":{"language":{"status":{}}}}"#
        );
        let answers = [
            Answer::Language(LanguageAnswer::Done {}),
            Answer::Patience { wait_ms: 20_000 },
            Answer::Rewritten {
                files: vec![Rewrite {
                    path: PathBuf::from("/w/a.rs"),
                    text: "fn new() {}\n".into(),
                    edits: 2,
                }],
            },
            Answer::Applied {
                title: "Import `HashMap`".into(),
                files: Vec::new(),
            },
        ];
        for (id, answer) in (1..).zip(answers) {
            let response = Response { id, answer };
            let text = serde_json::to_string(&response).unwrap();
            assert_eq!(serde_json::from_str::<Response>(&text).unwrap(), response);
        }
    }

    /// A failure crosses as the answer of its kind and comes back as the same error; any other answer is left alone.
    #[test]
    fn failures_cross_as_they_are() {
        for error in [
            LanguageError::NoServer("none".into()),
            LanguageError::Unsupported("cannot".into()),
            LanguageError::Failed("broke".into()),
        ] {
            assert_eq!(settled(failure(error.clone())), Err(error));
        }
        let done = Answer::Language(LanguageAnswer::Done {});
        assert_eq!(settled(done.clone()), Ok(done));
    }

    /// The lock sits beside the socket, under its name with `.lock` added.
    #[test]
    fn the_lock_is_beside_the_socket() {
        assert_eq!(
            lock_path(Path::new("/r/daemon.sock")),
            PathBuf::from("/r/daemon.sock.lock")
        );
    }
}
