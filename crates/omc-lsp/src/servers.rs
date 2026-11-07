//! The language servers the harness runs itself for one project, started the first time a file of theirs is asked about, or before when asked to warm them, and started again when one stops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{Diagnostic, LanguageService};
use tokio::sync::OnceCell;
use tokio::time::Instant;

use crate::client::Client;
use crate::spec::{Spec, default_specs};

/// A server once it has been tried: the running server, or nothing when it could not be started.
type Started = Arc<OnceCell<Option<Arc<Client>>>>;

/// How long to wait for the diagnostics of a file whose server is still starting or loading the project, which for a large project takes far longer than the usual wait.
pub const LOADING_WAIT: Duration = Duration::from_secs(20);

/// The most times a server that stopped is started again, so that one that keeps stopping is given up on.
const MOST_RESTARTS: u32 = 3;

/// One server's place: its latest start, and how many times it was started again after stopping.
#[derive(Default)]
struct Slot {
    /// The latest start.
    started: Started,
    /// How many times it was started again.
    restarts: u32,
}

/// The language servers of one project, each started the first time a file it reads is formatted or checked, and stopped when this is dropped.
pub struct Servers {
    /// The folder the project is in, which the servers work on.
    root: PathBuf,
    /// The servers that may be started.
    specs: Vec<Spec>,
    /// The servers tried so far, by name.
    started: Mutex<HashMap<String, Slot>>,
}

impl Servers {
    /// The usual language servers of each common language, for the project in `root`; none is started yet.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self::with_specs(root, default_specs())
    }

    /// The language servers `specs` describes, for the project in `root`; none is started yet.
    pub fn with_specs(root: impl Into<PathBuf>, specs: Vec<Spec>) -> Self {
        Self {
            root: root.into(),
            specs,
            started: Mutex::new(HashMap::new()),
        }
    }

    /// Starts, in the background, the servers of the languages the project is written in, as the files at its root say, and whose programs are installed, so that they have loaded the project by the time the first file is written.
    pub fn warm(&self) {
        for spec in &self.specs {
            if spec.is_used_in(&self.root) && spec.is_installed() {
                drop(self.starting(spec));
            }
        }
    }

    /// Whether the server that reads the file at `path` is still starting or loading the project: it has not been started, is being started, or has not said anything about any file yet. A server that could not be started, or no server at all, is not loading.
    fn is_loading(&self, path: &Path) -> bool {
        let Some(spec) = self
            .specs
            .iter()
            .find(|spec| spec.language_of(path).is_some())
        else {
            return false;
        };
        if !spec.is_installed() {
            return false;
        }
        let started = self.started.lock().unwrap_or_else(|held| held.into_inner());
        match started.get(&spec.id).map(|slot| slot.started.get()) {
            None | Some(None) => true,
            Some(Some(None)) => false,
            Some(Some(Some(client))) => !client.has_spoken(),
        }
    }

    /// The running server that reads the file at `path`, with the language id it reads it in, starting the server the first time; nothing when no server reads such files or it could not be started.
    async fn server_for(&self, path: &Path) -> Option<(Arc<Client>, String)> {
        let (spec, language) = self.specs.iter().find_map(|spec| {
            spec.language_of(path)
                .map(|language| (spec, language.to_owned()))
        })?;
        let client = self.starting(spec).await.ok().flatten()?;
        Some((client, language))
    }

    /// The server `spec` describes, running or being started on a task of its own, so that a caller that stops waiting does not stop it starting; it is started once, started again when it has stopped, up to [`MOST_RESTARTS`] times, and one that could not be started is not tried again.
    fn starting(&self, spec: &Spec) -> tokio::task::JoinHandle<Option<Arc<Client>>> {
        let cell = {
            let mut started = self.started.lock().unwrap_or_else(|held| held.into_inner());
            let slot = started.entry(spec.id.clone()).or_default();
            let stopped = slot
                .started
                .get()
                .and_then(Option::as_ref)
                .is_some_and(|client| !client.is_running());
            if stopped && slot.restarts < MOST_RESTARTS {
                slot.restarts += 1;
                slot.started = Started::default();
            }
            slot.started.clone()
        };
        let spec = spec.clone();
        let root = self.root.clone();
        tokio::spawn(async move {
            cell.get_or_init(|| async {
                match Client::start(&spec, &root).await {
                    Ok(client) => Some(Arc::new(client)),
                    Err(reason) => {
                        eprintln!("language server unavailable: {reason}");
                        None
                    }
                }
            })
            .await
            .clone()
        })
    }
}

#[async_trait]
impl LanguageService for Servers {
    /// The file at `path` formatted by the server that reads it, when one does and it formats.
    async fn format(&self, path: &Path) -> Option<String> {
        let (client, language) = self.server_for(path).await?;
        let text = tokio::fs::read_to_string(path).await.ok()?;
        client
            .format(path, &language, &text, omc_tools::FORMAT_WAIT)
            .await
    }

    /// [`LOADING_WAIT`] when a server of one of the files at `paths` is still starting or loading the project, otherwise the usual wait.
    fn patience(&self, paths: &[PathBuf]) -> Duration {
        if paths.iter().any(|path| self.is_loading(path)) {
            LOADING_WAIT
        } else {
            omc_tools::DIAGNOSTICS_WAIT
        }
    }

    /// What the servers that read the files at `paths` say is wrong with them, waiting up to `wait` in all.
    async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> Vec<Diagnostic> {
        let deadline = Instant::now() + wait;
        let mut found = Vec::new();
        for path in paths {
            let Ok(Some((client, language))) =
                tokio::time::timeout_at(deadline, self.server_for(path)).await
            else {
                continue;
            };
            let Ok(text) = tokio::fs::read_to_string(path).await else {
                continue;
            };
            found.extend(client.diagnostics(path, &language, &text, deadline).await);
        }
        found
    }
}

#[cfg(all(test, unix))]
/// Tests for starting servers, whose stand-in server is a POSIX shell script.
mod tests {
    use super::*;

    /// A server, run by the shell, that answers the starting request, stays up for a moment and then exits.
    const SHORT_LIVED: &str = r#"R='{"jsonrpc":"2.0","id":1,"result":{"capabilities":{}}}'; head -c 1 >/dev/null; printf 'Content-Length: %d\r\n\r\n%s' "${#R}" "$R"; sleep 1"#;

    /// Waits until `client` has stopped, for up to ten seconds.
    async fn stopped(client: &Client) {
        let until = Instant::now() + Duration::from_secs(10);
        while client.is_running() {
            assert!(Instant::now() < until, "the server did not stop");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// A server that stopped is started again on the next question, until it has stopped too many times.
    #[tokio::test]
    async fn a_stopped_server_is_started_again() {
        let spec = Spec::new("short", &[("x", "x")], &[&["sh", "-c", SHORT_LIVED]]);
        let servers = Servers::with_specs(".", vec![spec]);
        let file = Path::new("a.x");
        let (mut client, _) = servers.server_for(file).await.expect("the server starts");
        for _ in 0..MOST_RESTARTS {
            stopped(&client).await;
            let (again, _) = servers.server_for(file).await.expect("it starts again");
            assert!(!Arc::ptr_eq(&again, &client));
            client = again;
        }
        stopped(&client).await;
        let (last, _) = servers
            .server_for(file)
            .await
            .expect("the last one is kept");
        assert!(Arc::ptr_eq(&last, &client));
    }

    /// A server not started yet, or started and silent, is waited for longer; files no server reads, or whose server is missing, are not.
    #[tokio::test]
    async fn a_loading_server_is_waited_for_longer() {
        let short = Spec::new("short", &[("x", "x")], &[&["sh", "-c", SHORT_LIVED]]);
        let missing = Spec::new("missing", &[("y", "y")], &[&["no-such-language-server"]]);
        let servers = Servers::with_specs(".", vec![short, missing]);
        let file = PathBuf::from("a.x");
        assert_eq!(servers.patience(std::slice::from_ref(&file)), LOADING_WAIT);
        servers.server_for(&file).await.expect("the server starts");
        assert_eq!(servers.patience(std::slice::from_ref(&file)), LOADING_WAIT);
        let others = [PathBuf::from("a.y"), PathBuf::from("a.z")];
        assert_eq!(servers.patience(&others), omc_tools::DIAGNOSTICS_WAIT);
    }
}
