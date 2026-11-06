//! The language services a conversation's tools use: the client's, when it offers them, otherwise the harness's own servers, or one a program embedding the harness gives it.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{Diagnostic, FORMAT_WAIT, LanguageService};
use omc_wire::{Event, LanguageAnswer, LanguageAsk, LanguageRequest};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

/// How much longer than the client was asked to wait it is given to answer, for the time the messages take.
const GRACE: Duration = Duration::from_millis(250);

/// Language services the client provides: each question goes to it as an [`Event::Language`] and is answered by a [`omc_wire::Command::Language`] under the same ticket.
pub(crate) struct ClientLanguages {
    /// Where the questions go.
    out: UnboundedSender<Event>,
    /// The questions still waiting for an answer, by ticket.
    pending: Mutex<HashMap<u64, oneshot::Sender<LanguageAnswer>>>,
    /// The ticket the next question is put under.
    next: AtomicU64,
}

impl ClientLanguages {
    /// Language services asked of the client through `out`.
    pub(crate) fn new(out: UnboundedSender<Event>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
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
            LanguageAnswer::Diagnostics { .. } => None,
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
