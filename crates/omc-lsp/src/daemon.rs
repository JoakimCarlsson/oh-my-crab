//! The language server daemon: one process per user that runs the language servers of every project its conversations work in.
//!
//! Without it each conversation starts servers of its own, so ten conversations on one project load the project ten times. The daemon keeps one language service per project root instead, and every conversation in that root asks it over a Unix socket, in the messages of [`crate::protocol`]. The servers of a root nobody has asked about for a while are stopped, and a daemon nobody is connected to for as long ends.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use omc_tools::{Chosen, LanguageError, LanguageService, Lookup};
use omc_wire::{LanguageAnswer, LanguageAsk};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::Servers;
use crate::protocol::{
    Answer, Ask, Hello, IDLE, PROTOCOL_VERSION, Request, Response, Unread, Welcome, action_info,
    applied_answer, default_folder, failure, location, lock_path, millis, rewrite, server_info,
    socket_path, spot, symbol_info,
};

/// How long a connection is given to say hello before it is hung up on.
const HELLO_WAIT: Duration = Duration::from_secs(10);

/// The shortest time between two looks for roots to release, however short the idle time.
const SHORTEST_TICK: Duration = Duration::from_millis(10);

/// Makes the language service of a project: called with the project's root and whether its servers should start at once.
pub(crate) type Factory = Box<dyn Fn(&Path, bool) -> Arc<dyn LanguageService> + Send + Sync>;

/// What tells the services of the daemon apart: the canonical root of the project and whether its servers were started at once.
type Key = (PathBuf, bool);

/// The language service of one root and how recently it was asked.
struct Root {
    /// The service every conversation in the root asks.
    service: Arc<dyn LanguageService>,
    /// When a question to it last started or ended.
    used: Instant,
    /// How many questions to it are being answered now; it is never released meanwhile.
    busy: usize,
}

/// What the daemon keeps track of.
struct State {
    /// The services running, by root and trust.
    roots: HashMap<Key, Root>,
    /// How many connections are open.
    connections: usize,
    /// When the daemon started or its last connection closed.
    quiet_since: Instant,
}

/// The language services of every project the user's conversations work in, shared by root.
pub(crate) struct Daemon {
    /// Makes the service of a root the first time it is asked about, and again after it was released.
    factory: Factory,
    /// How long a root nobody asks about keeps its service, and how long the daemon stays with no connection.
    idle: Duration,
    /// The services running and the connections open.
    state: Mutex<State>,
}

/// A question being answered by the service of a root, which keeps the root from being released until it is dropped.
struct Claim {
    /// The daemon the root belongs to.
    daemon: Arc<Daemon>,
    /// The root asked about.
    key: Key,
    /// The service of the root.
    service: Arc<dyn LanguageService>,
}

impl Drop for Claim {
    /// Counts the question as answered and the root as used just now.
    fn drop(&mut self) {
        let mut state = self.daemon.state();
        if let Some(root) = state.roots.get_mut(&self.key) {
            root.busy = root.busy.saturating_sub(1);
            root.used = Instant::now();
        }
    }
}

impl Daemon {
    /// A daemon whose services `factory` makes, releasing a root after `idle` without a question and ending after `idle` without a connection.
    pub(crate) fn new(factory: Factory, idle: Duration) -> Self {
        Self {
            factory,
            idle,
            state: Mutex::new(State {
                roots: HashMap::new(),
                connections: 0,
                quiet_since: Instant::now(),
            }),
        }
    }

    /// The daemon's state, locked; a lock a panicking task left poisoned is taken all the same.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Answers the conversations that connect to `listener` until none has been connected for the idle time.
    pub(crate) async fn serve(self: Arc<Self>, listener: UnixListener) {
        let mut tick = tokio::time::interval((self.idle / 4).max(SHORTEST_TICK));
        loop {
            tokio::select! {
                accepted = listener.accept() => match accepted {
                    Ok((stream, _)) => {
                        self.state().connections += 1;
                        tokio::spawn(self.clone().converse(stream));
                    }
                    Err(_) => tokio::time::sleep(SHORTEST_TICK).await,
                },
                _ = tick.tick() => {
                    if self.reap() {
                        return;
                    }
                }
            }
        }
    }

    /// Releases the services of the roots nobody asked about for the idle time, and says whether the daemon has been without a connection for as long and should end.
    fn reap(&self) -> bool {
        let mut released = Vec::new();
        let ending = {
            let mut state = self.state();
            let idle = self.idle;
            let stale: Vec<Key> = state
                .roots
                .iter()
                .filter(|(_, root)| root.busy == 0 && root.used.elapsed() >= idle)
                .map(|(key, _)| key.clone())
                .collect();
            for key in stale {
                released.extend(state.roots.remove(&key));
            }
            state.connections == 0 && state.quiet_since.elapsed() >= idle
        };
        drop(released);
        ending
    }

    /// The service of the root `key`, made for the folder `root` when it is not running, claimed for one question.
    fn claim(self: &Arc<Self>, key: &Key, root: &Path) -> Claim {
        let mut state = self.state();
        let entry = state.roots.entry(key.clone()).or_insert_with(|| Root {
            service: (self.factory)(root, key.1),
            used: Instant::now(),
            busy: 0,
        });
        entry.busy += 1;
        entry.used = Instant::now();
        Claim {
            daemon: self.clone(),
            key: key.clone(),
            service: entry.service.clone(),
        }
    }

    /// Holds the conversation on `stream`: greets it, then answers its requests, each in a task of its own, until it hangs up.
    async fn converse(self: Arc<Self>, stream: UnixStream) {
        let (read, write) = stream.into_split();
        let mut lines = BufReader::new(read).lines();
        let (out, outgoing) = unbounded_channel();
        tokio::spawn(write_lines(write, outgoing));
        if let Some(hello) = greet(&mut lines, &out).await {
            let key = (
                std::fs::canonicalize(&hello.root).unwrap_or_else(|_| hello.root.clone()),
                hello.trusted,
            );
            if hello.trusted {
                drop(self.claim(&key, &hello.root));
            }
            while let Ok(Some(line)) = lines.next_line().await {
                self.take(&line, &key, &hello.root, &out);
            }
        }
        let mut state = self.state();
        state.connections = state.connections.saturating_sub(1);
        state.quiet_since = Instant::now();
    }

    /// Starts answering the request on `line`, asked about the root `key` in the folder `root`, and sends the answer to `out` when it is ready; a line that is no request is answered with a failure when it names an id, and dropped otherwise.
    fn take(self: &Arc<Self>, line: &str, key: &Key, root: &Path, out: &UnboundedSender<String>) {
        match serde_json::from_str::<Request>(line) {
            Ok(request) => {
                let claim = self.claim(key, root);
                let out = out.clone();
                tokio::spawn(async move {
                    let answer = answer(claim.service.as_ref(), request.ask).await;
                    drop(claim);
                    send(
                        &out,
                        &Response {
                            id: request.id,
                            answer,
                        },
                    );
                });
            }
            Err(error) => {
                if let Ok(unread) = serde_json::from_str::<Unread>(line) {
                    let reason =
                        format!("the language server daemon cannot read the question: {error}");
                    send(
                        out,
                        &Response {
                            id: unread.id,
                            answer: failure(LanguageError::Failed(reason)),
                        },
                    );
                }
            }
        }
    }
}

/// Reads the hello of a conversation from `lines` and answers it on `out` with the daemon's version; the hello when the conversation speaks that version, nothing when it speaks another, says nothing readable or takes longer than [`HELLO_WAIT`].
async fn greet(
    lines: &mut Lines<BufReader<OwnedReadHalf>>,
    out: &UnboundedSender<String>,
) -> Option<Hello> {
    let line = tokio::time::timeout(HELLO_WAIT, lines.next_line())
        .await
        .ok()?
        .ok()??;
    let hello = serde_json::from_str::<Hello>(&line).ok();
    send(
        out,
        &Welcome {
            version: PROTOCOL_VERSION,
            pid: std::process::id(),
        },
    );
    hello.filter(|hello| hello.version == PROTOCOL_VERSION)
}

/// Queues `message` as one JSON line on `out`; a message that cannot be written as JSON, or whose connection is gone, is dropped.
fn send<T: serde::Serialize>(out: &UnboundedSender<String>, message: &T) {
    if let Ok(line) = serde_json::to_string(message) {
        let _ = out.send(line);
    }
}

/// Writes the lines queued on `outgoing` to `write`, until the queue is closed or the connection is gone.
async fn write_lines(mut write: OwnedWriteHalf, mut outgoing: UnboundedReceiver<String>) {
    while let Some(mut line) = outgoing.recv().await {
        line.push('\n');
        if write.write_all(line.as_bytes()).await.is_err() {
            return;
        }
    }
}

/// What `service` answers to `ask`.
pub(crate) async fn answer(service: &dyn LanguageService, ask: Ask) -> Answer {
    match ask {
        Ask::Language(ask) => language(service, ask).await,
        Ask::Patience { paths } => Answer::Patience {
            wait_ms: millis(service.patience(&paths)),
        },
        Ask::Serves { path } => match service.serves(&path).await {
            Ok(()) => Answer::Language(LanguageAnswer::Done {}),
            Err(error) => failure(error),
        },
        Ask::Reformat { path } => match service.reformat(&path, None).await {
            Ok(text) => Answer::Language(LanguageAnswer::Formatted { text }),
            Err(error) => failure(error),
        },
    }
}

/// What `service` answers to the question `ask` that a client's language services could be asked as well; renames, moves and code actions are answered with whole new texts instead of edits.
async fn language(service: &dyn LanguageService, ask: LanguageAsk) -> Answer {
    let answered = match ask {
        LanguageAsk::Format { path } => Ok(LanguageAnswer::Formatted {
            text: service.format(&path).await,
        }),
        LanguageAsk::Diagnostics { paths, wait_ms } => Ok(LanguageAnswer::Diagnostics {
            diagnostics: service
                .diagnostics(&paths, Duration::from_millis(wait_ms))
                .await,
        }),
        LanguageAsk::Definition { at } => locations(service, Lookup::Definition, at).await,
        LanguageAsk::TypeDefinition { at } => locations(service, Lookup::TypeDefinition, at).await,
        LanguageAsk::Implementation { at } => locations(service, Lookup::Implementation, at).await,
        LanguageAsk::References { at } => locations(service, Lookup::References, at).await,
        LanguageAsk::Hover { at } => service
            .hover(&spot(at))
            .await
            .map(|text| LanguageAnswer::Hover { text }),
        LanguageAsk::DocumentSymbols { path } => {
            service
                .document_symbols(&path)
                .await
                .map(|symbols| LanguageAnswer::Symbols {
                    symbols: symbols.into_iter().map(symbol_info).collect(),
                })
        }
        LanguageAsk::WorkspaceSymbols { query, near } => service
            .workspace_symbols(&query, near.as_deref())
            .await
            .map(|symbols| LanguageAnswer::Symbols {
                symbols: symbols.into_iter().map(symbol_info).collect(),
            }),
        LanguageAsk::WorkspaceDiagnostics { wait_ms } => service
            .all_diagnostics(Duration::from_millis(wait_ms))
            .await
            .map(|diagnostics| LanguageAnswer::Diagnostics { diagnostics }),
        LanguageAsk::Rename { at, new_name } => {
            return match service.rename(&spot(at), &new_name).await {
                Ok(files) => Answer::Rewritten {
                    files: files.into_iter().map(rewrite).collect(),
                },
                Err(error) => failure(error),
            };
        }
        LanguageAsk::FormatRange { range } => {
            let range = location(range);
            service
                .reformat(&range.path, Some(&range))
                .await
                .map(|text| LanguageAnswer::Formatted { text })
        }
        LanguageAsk::CodeActions { range, only } => service
            .code_actions(&location(range), &only)
            .await
            .map(|actions| LanguageAnswer::Actions {
                actions: actions.into_iter().map(action_info).collect(),
            }),
        LanguageAsk::ApplyCodeAction {
            range,
            only,
            index,
            title,
        } => {
            let chosen = Chosen {
                index: usize::try_from(index).unwrap_or(usize::MAX),
                title,
            };
            return match service
                .apply_code_action(&location(range), &only, &chosen)
                .await
            {
                Ok(applied) => applied_answer(applied),
                Err(error) => failure(error),
            };
        }
        LanguageAsk::WillRenameFile { from, to } => {
            return match service.rename_file(&from, &to).await {
                Ok(files) => Answer::Rewritten {
                    files: files.into_iter().map(rewrite).collect(),
                },
                Err(error) => failure(error),
            };
        }
        LanguageAsk::FileRenamed { from, to } => {
            service.file_renamed(&from, &to).await;
            Ok(LanguageAnswer::Done {})
        }
        LanguageAsk::Status {} => service
            .status()
            .await
            .map(|servers| LanguageAnswer::Servers {
                servers: servers.into_iter().map(server_info).collect(),
            }),
        LanguageAsk::Reload { server, near } => service
            .reload(server.as_deref(), near.as_deref())
            .await
            .map(|servers| LanguageAnswer::Servers {
                servers: servers.into_iter().map(server_info).collect(),
            }),
    };
    match answered {
        Ok(answer) => Answer::Language(answer),
        Err(error) => failure(error),
    }
}

/// The places `lookup` finds from the symbol at `at`, as `service` finds them, as an answer.
///
/// # Errors
///
/// Returns the failure of `service`.
async fn locations(
    service: &dyn LanguageService,
    lookup: Lookup,
    at: omc_wire::Place,
) -> Result<LanguageAnswer, LanguageError> {
    let found = service.locate(lookup, &spot(at)).await?;
    Ok(LanguageAnswer::Locations {
        locations: found.iter().map(crate::protocol::stretch).collect(),
    })
}

/// The harness's own language servers for the project in `root`, started at once when `warm`.
pub(crate) fn own_servers(root: &Path, warm: bool) -> Arc<dyn LanguageService> {
    let servers = Servers::new(root);
    if warm {
        servers.warm();
    }
    Arc::new(servers)
}

/// Makes the folder the socket at `socket` is kept in; the daemon's usual folder is made, or made again, readable by the user alone.
///
/// # Errors
///
/// Fails when the folder cannot be made or its permissions cannot be set.
fn prepare_folder(socket: &Path) -> io::Result<()> {
    let Some(folder) = socket
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    else {
        return Ok(());
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(folder)?;
    if default_folder().as_deref() == Some(folder) {
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Runs the language server daemon of the user: listens on the user's socket and answers every conversation that connects, with the harness's own servers, until no conversation has been connected for ten minutes.
///
/// Only one daemon runs per user: it holds a lock file beside the socket, and a second one started while the first runs returns at once. A socket file a daemon that died left behind is replaced.
///
/// # Errors
///
/// Fails when the user has no folder to keep the socket in, or the folder, the lock file or the socket cannot be made.
pub async fn run_daemon() -> io::Result<()> {
    let socket = socket_path().ok_or_else(|| {
        io::Error::other("there is no runtime or cache folder to keep the daemon's socket in")
    })?;
    prepare_folder(&socket)?;
    let lock = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(&socket))?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(std::fs::TryLockError::Error(error)) => return Err(error),
    }
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket)?;
    std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
    let daemon = Arc::new(Daemon::new(Box::new(own_servers), IDLE));
    daemon.serve(listener).await;
    let _ = std::fs::remove_file(&socket);
    drop(lock);
    Ok(())
}
