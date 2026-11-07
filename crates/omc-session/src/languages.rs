//! The language services a conversation's tools use: the client's, when it offers them, otherwise the harness's own servers, or one a program embedding the harness gives it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{
    Diagnostic, FORMAT_WAIT, LanguageError, LanguageService, Location, Lookup, Renamed, Spot,
    Symbol,
};
use omc_wire::{Event, LanguageAnswer, LanguageAsk, LanguageRequest, Place, Stretch, SymbolInfo};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

/// How much longer than the client was asked to wait it is given to answer, for the time the messages take.
const GRACE: Duration = Duration::from_millis(250);

/// How long the client is given to answer a question about code, which may need its server to start and load the project first.
pub(crate) const NAVIGATION_WAIT: Duration = Duration::from_secs(60);

/// Language services the client provides: each question goes to it as an [`Event::Language`] and is answered by a [`omc_wire::Command::Language`] under the same ticket.
pub(crate) struct ClientLanguages {
    /// Where the questions go.
    out: UnboundedSender<Event>,
    /// The questions still waiting for an answer, by ticket.
    pending: Mutex<HashMap<u64, oneshot::Sender<LanguageAnswer>>>,
    /// The ticket the next question is put under.
    next: AtomicU64,
    /// Whether the client said, when it opened the conversation, that it answers questions about code; it is never asked them otherwise.
    navigates: AtomicBool,
}

impl ClientLanguages {
    /// Language services asked of the client through `out`.
    pub(crate) fn new(out: UnboundedSender<Event>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            navigates: AtomicBool::new(false),
        }
    }

    /// Has questions about code go to the client from now on when `navigates`, or be refused without asking it, as for a client that did not say it answers them.
    pub(crate) fn set_navigation(&self, navigates: bool) {
        self.navigates.store(navigates, Ordering::Relaxed);
    }

    /// Asks the client the question about code `ask`, waiting [`NAVIGATION_WAIT`] for its answer.
    ///
    /// # Errors
    ///
    /// Returns [`LanguageError::Unsupported`] without asking when the client did not say it answers questions about code, the failure the client answers with, and [`LanguageError::Failed`] when it does not answer in time.
    async fn navigate(&self, ask: LanguageAsk) -> Result<LanguageAnswer, LanguageError> {
        if !self.navigates.load(Ordering::Relaxed) {
            return Err(client_cannot_navigate());
        }
        match self.ask(ask, NAVIGATION_WAIT + GRACE).await {
            None => Err(LanguageError::Failed(
                "the client's language services did not answer in time".to_owned(),
            )),
            Some(LanguageAnswer::Unsupported { reason }) => Err(LanguageError::Unsupported(reason)),
            Some(LanguageAnswer::NoServer { reason }) => Err(LanguageError::NoServer(reason)),
            Some(LanguageAnswer::Failed { reason }) => Err(LanguageError::Failed(reason)),
            Some(answer) => Ok(answer),
        }
    }

    /// Takes the client's `answer` to the question under `ticket`; an answer nobody waits for any more is dropped.
    pub(crate) fn answer(&self, ticket: u64, answer: LanguageAnswer) {
        let waiting = self
            .pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .remove(&ticket);
        if let Some(waiting) = waiting {
            let _ = waiting.send(answer);
        }
    }

    /// Asks the client `ask` and waits up to `patience` for its answer; nothing when it does not answer in time or is gone.
    async fn ask(&self, ask: LanguageAsk, patience: Duration) -> Option<LanguageAnswer> {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let (answer, answered) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .insert(ticket, answer);
        let sent = self
            .out
            .send(Event::Language(LanguageRequest { ticket, ask }))
            .is_ok();
        let answer = if sent {
            tokio::time::timeout(patience, answered)
                .await
                .ok()
                .and_then(Result::ok)
        } else {
            None
        };
        self.pending
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .remove(&ticket);
        answer
    }
}

#[async_trait]
impl LanguageService for ClientLanguages {
    /// Asks the client to format the file at `path`.
    async fn format(&self, path: &Path) -> Option<String> {
        let ask = LanguageAsk::Format {
            path: path.to_path_buf(),
        };
        match self.ask(ask, FORMAT_WAIT + GRACE).await? {
            LanguageAnswer::Formatted { text } => text,
            _ => None,
        }
    }

    /// Asks the client what is wrong with the files at `paths`, letting it wait up to `wait` for fresh diagnostics.
    async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> Vec<Diagnostic> {
        let ask = LanguageAsk::Diagnostics {
            paths: paths.to_vec(),
            wait_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        };
        match self.ask(ask, wait + GRACE).await {
            Some(LanguageAnswer::Diagnostics { diagnostics }) => diagnostics,
            _ => Vec::new(),
        }
    }

    /// Asks the client where `lookup` leads from the symbol at `at`, or refuses when it does not navigate code.
    async fn locate(&self, lookup: Lookup, at: &Spot) -> Result<Vec<Location>, LanguageError> {
        let at = place(at);
        let ask = match lookup {
            Lookup::Definition => LanguageAsk::Definition { at },
            Lookup::TypeDefinition => LanguageAsk::TypeDefinition { at },
            Lookup::Implementation => LanguageAsk::Implementation { at },
            Lookup::References => LanguageAsk::References { at },
        };
        match self.navigate(ask).await? {
            LanguageAnswer::Locations { locations } => {
                Ok(locations.into_iter().map(location).collect())
            }
            _ => Err(answered_otherwise(lookup.noun())),
        }
    }

    /// Asks the client what the symbol at `at` is, or refuses when it does not navigate code.
    async fn hover(&self, at: &Spot) -> Result<Option<String>, LanguageError> {
        match self.navigate(LanguageAsk::Hover { at: place(at) }).await? {
            LanguageAnswer::Hover { text } => Ok(text),
            _ => Err(answered_otherwise("hover")),
        }
    }

    /// Asks the client for the symbols of the file at `path`, or refuses when it does not navigate code.
    async fn document_symbols(&self, path: &Path) -> Result<Vec<Symbol>, LanguageError> {
        let ask = LanguageAsk::DocumentSymbols {
            path: path.to_path_buf(),
        };
        match self.navigate(ask).await? {
            LanguageAnswer::Symbols { symbols } => Ok(symbols.into_iter().map(symbol).collect()),
            _ => Err(answered_otherwise("document symbols")),
        }
    }

    /// Asks the client for the project's symbols matching `query`, of the server of the file at `near` when it is given, or refuses when it does not navigate code.
    async fn workspace_symbols(
        &self,
        query: &str,
        near: Option<&Path>,
    ) -> Result<Vec<Symbol>, LanguageError> {
        let ask = LanguageAsk::WorkspaceSymbols {
            query: query.to_owned(),
            near: near.map(Path::to_path_buf),
        };
        match self.navigate(ask).await? {
            LanguageAnswer::Symbols { symbols } => Ok(symbols.into_iter().map(symbol).collect()),
            _ => Err(answered_otherwise("workspace symbols")),
        }
    }

    /// Asks the client for the edits renaming the symbol at `at` to `new_name`, and makes them to the texts on disk without writing them; refuses when the client does not navigate code.
    async fn rename(&self, at: &Spot, new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
        let ask = LanguageAsk::Rename {
            at: place(at),
            new_name: new_name.to_owned(),
        };
        match self.navigate(ask).await? {
            LanguageAnswer::Edits { files } => {
                crate::edits::renamed(files).map_err(LanguageError::Failed)
            }
            _ => Err(answered_otherwise("rename")),
        }
    }

    /// Asks the client everything its servers have said is wrong with the project, letting it wait up to `wait` for servers still loading or checking it; refuses when it does not navigate code.
    async fn all_diagnostics(&self, wait: Duration) -> Result<Vec<Diagnostic>, LanguageError> {
        if !self.navigates.load(Ordering::Relaxed) {
            return Err(client_cannot_navigate());
        }
        let ask = LanguageAsk::WorkspaceDiagnostics {
            wait_ms: u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
        };
        match self.ask(ask, wait + GRACE).await {
            Some(LanguageAnswer::Diagnostics { diagnostics }) => Ok(diagnostics),
            Some(LanguageAnswer::Unsupported { reason }) => Err(LanguageError::Unsupported(reason)),
            Some(LanguageAnswer::NoServer { reason }) => Err(LanguageError::NoServer(reason)),
            Some(LanguageAnswer::Failed { reason }) => Err(LanguageError::Failed(reason)),
            Some(_) => Err(answered_otherwise("workspace diagnostics")),
            None => Err(LanguageError::Failed(
                "the client's language services did not answer in time".to_owned(),
            )),
        }
    }
}

/// `spot` as the wire places it.
fn place(spot: &Spot) -> Place {
    Place {
        path: spot.path.clone(),
        line: spot.line,
        column: spot.column,
    }
}

/// The stretch `stretch` the client pointed at, as the tools place it.
fn location(stretch: Stretch) -> Location {
    Location {
        path: stretch.path,
        line: stretch.line,
        column: stretch.column,
        end_line: stretch.end_line,
        end_column: stretch.end_column,
    }
}

/// The symbol `info` the client named, as the tools know it.
fn symbol(info: SymbolInfo) -> Symbol {
    Symbol {
        name: info.name,
        kind: info.kind,
        detail: info.detail,
        container: info.container,
        depth: usize::try_from(info.depth).unwrap_or(usize::MAX),
        location: location(info.location),
    }
}

/// What the model is told when the client answers a question about `what` with an answer to another kind of question.
fn answered_otherwise(what: &str) -> LanguageError {
    LanguageError::Failed(format!(
        "the client's language services answered the {what} question with something else"
    ))
}

/// What the model is told when it asks about code the language services of a client that did not say it answers such questions: only formatting and the diagnostics of named files can be asked of it.
fn client_cannot_navigate() -> LanguageError {
    LanguageError::Unsupported(
        "the client runs this conversation's language servers and is asked only to format files and report the diagnostics of named files, so it cannot answer this; `lsp` with action diagnostics and a file path still works"
            .to_owned(),
    )
}

/// The language servers the harness runs itself for the project in `directory`, when it was built with them.
///
/// A language server loads the project, which for some languages runs the project's own build scripts. So the servers start only once the agent writes a file, which the person running it allowed, unless they trust the agent with everything (`trusted`): then the servers of the languages the project is written in start now, so that they are ready when the agent first writes a file.
#[cfg(feature = "lsp")]
pub(crate) fn own_servers(directory: &Path, trusted: bool) -> Option<Arc<dyn LanguageService>> {
    let servers = omc_lsp::Servers::new(directory);
    if trusted {
        servers.warm();
    }
    Some(Arc::new(servers))
}

/// No language servers of the harness's own, since it was built without them.
#[cfg(not(feature = "lsp"))]
pub(crate) fn own_servers(_directory: &Path, _trusted: bool) -> Option<Arc<dyn LanguageService>> {
    None
}

#[cfg(test)]
/// Tests for asking the client.
mod tests {
    use omc_wire::Severity;

    use super::*;

    /// A question goes out as an event under a ticket, and the answer under that ticket comes back.
    #[tokio::test]
    async fn the_client_answers_under_the_ticket() {
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let languages = Arc::new(ClientLanguages::new(out));
        let answering = languages.clone();
        let client = tokio::spawn(async move {
            let Some(Event::Language(request)) = events.recv().await else {
                panic!("no language request");
            };
            let LanguageAsk::Diagnostics { paths, wait_ms } = request.ask else {
                panic!("not a diagnostics request");
            };
            assert_eq!(wait_ms, 3000);
            answering.answer(
                request.ticket,
                LanguageAnswer::Diagnostics {
                    diagnostics: vec![Diagnostic {
                        path: paths[0].clone(),
                        line: 1,
                        column: 1,
                        severity: Severity::Error,
                        message: "bad".into(),
                        source: None,
                        code: None,
                    }],
                },
            );
        });
        let found = languages
            .diagnostics(&[PathBuf::from("/w/a.rs")], Duration::from_secs(3))
            .await;
        client.await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "bad");
    }

    /// Questions about code are refused at once without asking the client, since the wire does not carry them, saying that the diagnostics of a file still work.
    #[tokio::test]
    async fn navigation_is_refused_without_asking_the_client() {
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let languages = ClientLanguages::new(out);
        let at = Spot {
            path: PathBuf::from("/w/a.rs"),
            line: 1,
            column: 1,
        };
        let Err(LanguageError::Unsupported(reason)) =
            languages.locate(Lookup::References, &at).await
        else {
            panic!("navigation was not refused");
        };
        assert!(
            reason.contains("diagnostics and a file path still works"),
            "{reason}"
        );
        assert!(languages.rename(&at, "b").await.is_err());
        assert!(
            languages
                .all_diagnostics(Duration::from_secs(1))
                .await
                .is_err()
        );
        assert!(events.try_recv().is_err());
    }

    /// A client that said it navigates code is asked about it under a ticket, and its places, hovers and refusals come back as the tools know them.
    #[tokio::test]
    async fn a_navigating_client_is_asked_about_code() {
        let (out, mut events) = tokio::sync::mpsc::unbounded_channel();
        let languages = Arc::new(ClientLanguages::new(out));
        languages.set_navigation(true);
        let answering = languages.clone();
        let client = tokio::spawn(async move {
            let Some(Event::Language(request)) = events.recv().await else {
                panic!("no references request");
            };
            let LanguageAsk::References { at } = request.ask else {
                panic!("not a references request: {:?}", request.ask);
            };
            assert_eq!((at.line, at.column), (2, 5));
            answering.answer(
                request.ticket,
                LanguageAnswer::Locations {
                    locations: vec![Stretch {
                        path: at.path.clone(),
                        line: 7,
                        column: 3,
                        end_line: 7,
                        end_column: 8,
                    }],
                },
            );
            let Some(Event::Language(request)) = events.recv().await else {
                panic!("no hover request");
            };
            answering.answer(
                request.ticket,
                LanguageAnswer::Unsupported {
                    reason: "no hovers here".into(),
                },
            );
        });
        let at = Spot {
            path: PathBuf::from("/w/a.rs"),
            line: 2,
            column: 5,
        };
        let found = languages.locate(Lookup::References, &at).await.unwrap();
        assert_eq!(
            (found[0].line, found[0].column, found[0].end_column),
            (7, 3, 8)
        );
        assert_eq!(
            languages.hover(&at).await,
            Err(LanguageError::Unsupported("no hovers here".into()))
        );
        client.await.unwrap();
    }

    /// A client that does not answer in time is given up on, with no diagnostics and no formatting, and a late answer is dropped.
    #[tokio::test(start_paused = true)]
    async fn a_silent_client_is_given_up_on() {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let languages = ClientLanguages::new(out);
        let found = languages
            .diagnostics(&[PathBuf::from("/w/a.rs")], Duration::from_secs(3))
            .await;
        assert!(found.is_empty());
        assert_eq!(languages.format(Path::new("/w/a.rs")).await, None);
        languages.answer(1, LanguageAnswer::Formatted { text: None });
        assert!(languages.pending.lock().unwrap().is_empty());
    }
}
