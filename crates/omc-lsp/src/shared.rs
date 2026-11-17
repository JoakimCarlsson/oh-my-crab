//! Language services asked of the language server daemon, so that every conversation of the user in one project shares one set of servers.
//!
//! [`DaemonLanguages`] connects to the daemon the first time it is asked something, starting it when nothing listens, and from then on sends each question over the connection. When the connection drops it connects once more; when that fails too, or the daemon cannot be reached, speaks another version of the protocol or stops answering, the conversation's questions go to servers of its own for the rest of its life, so a broken daemon never costs it its language services.

use std::collections::HashMap;
use std::io::{BufRead as _, Write as _};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{
    Applied, Chosen, CodeAction, DIAGNOSTICS_WAIT, Diagnostic, FORMAT_WAIT, LanguageError,
    LanguageService, Location, Lookup, Renamed, ServerStatus, Spot, Symbol,
};
use omc_wire::{LanguageAnswer, LanguageAsk};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::sync::oneshot;

use crate::LOADING_WAIT;
use crate::protocol::{
    Answer, Ask, Hello, PROTOCOL_VERSION, Request, Response, SOCKET_VARIABLE, Welcome, action,
    location, millis, place, renamed, server, settled, socket_path, stretch, symbol,
};

/// How long the daemon is given to take a connection.
const CONNECT_WAIT: Duration = Duration::from_secs(2);

/// How long the daemon is given to answer a hello.
const WELCOME_WAIT: Duration = Duration::from_secs(3);

/// How many times a daemon just started is tried before it is given up on.
const START_ATTEMPTS: u32 = 40;

/// How long to wait between two tries to reach a daemon just started.
const START_PAUSE: Duration = Duration::from_millis(50);

/// How long the daemon is given to answer a question about code, which may need a server to start and load the project first, before it is taken to be stuck.
const NAVIGATION_WAIT: Duration = Duration::from_secs(180);

/// How much longer than a question lets the servers wait the daemon is given to answer it.
const SLACK: Duration = Duration::from_secs(30);

/// How long the daemon is given to say, on the side connection, how long diagnostics should be waited for; the answer is asked for where nothing can be awaited, so it must come at once.
const SIDE_WAIT: Duration = Duration::from_millis(250);

/// Makes the servers a conversation falls back to when the daemon cannot be used.
type Fallback = Box<dyn Fn() -> Arc<dyn LanguageService> + Send + Sync>;

/// Why a question to the daemon got no answer.
enum Lost {
    /// The connection is gone.
    Dropped,
    /// The connection stands, but the daemon did not answer in time.
    TimedOut,
}

/// The questions of a connection that still wait for their answers.
struct Pending {
    /// Where each answer goes, by the id of its request.
    waiting: HashMap<u64, oneshot::Sender<Answer>>,
    /// Whether the connection is gone, so that nothing more can be asked on it.
    closed: bool,
}

/// One connection to the daemon, greeted and ready for questions.
struct Connection {
    /// Where the lines to send go.
    out: UnboundedSender<String>,
    /// The questions waiting for answers.
    pending: Arc<Mutex<Pending>>,
    /// The id the next request is sent under.
    next: AtomicU64,
}

/// What trying to open a connection came to.
enum Opened {
    /// The daemon greeted the conversation.
    Connected(Connection),
    /// A daemon answered, but speaks another version of the protocol.
    Mismatched,
    /// Nothing listens on the socket, or what does said nothing readable.
    Absent,
}

/// The pending questions of a connection, locked; a lock a panicking task left poisoned is taken all the same.
fn pending(pending: &Mutex<Pending>) -> MutexGuard<'_, Pending> {
    pending.lock().unwrap_or_else(|held| held.into_inner())
}

impl Connection {
    /// Connects to the daemon listening on `socket` and greets it with `hello`.
    async fn open(socket: &Path, hello: &Hello) -> Opened {
        let Ok(Ok(stream)) = tokio::time::timeout(CONNECT_WAIT, UnixStream::connect(socket)).await
        else {
            return Opened::Absent;
        };
        let (read, mut write) = stream.into_split();
        let Ok(mut line) = serde_json::to_string(hello) else {
            return Opened::Absent;
        };
        line.push('\n');
        if write.write_all(line.as_bytes()).await.is_err() {
            return Opened::Absent;
        }
        let mut lines = BufReader::new(read).lines();
        let Ok(Ok(Some(line))) = tokio::time::timeout(WELCOME_WAIT, lines.next_line()).await else {
            return Opened::Absent;
        };
        match serde_json::from_str::<Welcome>(&line) {
            Ok(welcome) if welcome.version == PROTOCOL_VERSION => {}
            Ok(_) => return Opened::Mismatched,
            Err(_) => return Opened::Absent,
        }
        let waiting = Arc::new(Mutex::new(Pending {
            waiting: HashMap::new(),
            closed: false,
        }));
        let (out, outgoing) = unbounded_channel();
        tokio::spawn(write_lines(write, outgoing));
        tokio::spawn(read_answers(lines, waiting.clone()));
        Opened::Connected(Connection {
            out,
            pending: waiting,
            next: AtomicU64::new(1),
        })
    }

    /// Asks the daemon `ask` and waits up to `patience` for its answer.
    ///
    /// # Errors
    ///
    /// Returns [`Lost::Dropped`] when the connection is gone, before or while waiting, and [`Lost::TimedOut`] when the daemon does not answer in time.
    async fn ask(&self, ask: &Ask, patience: Duration) -> Result<Answer, Lost> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let request = Request {
            id,
            ask: ask.clone(),
        };
        let line = serde_json::to_string(&request).map_err(|_| Lost::Dropped)?;
        let (answer, answered) = oneshot::channel();
        {
            let mut pending = pending(&self.pending);
            if pending.closed {
                return Err(Lost::Dropped);
            }
            pending.waiting.insert(id, answer);
        }
        if self.out.send(line).is_err() {
            pending(&self.pending).waiting.remove(&id);
            return Err(Lost::Dropped);
        }
        match tokio::time::timeout(patience, answered).await {
            Ok(Ok(answer)) => Ok(answer),
            Ok(Err(_)) => Err(Lost::Dropped),
            Err(_) => {
                pending(&self.pending).waiting.remove(&id);
                Err(Lost::TimedOut)
            }
        }
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

/// Hands each answer read from `lines` to the question in `waiting` it belongs to, until the connection ends; the questions still waiting then learn that it is gone.
async fn read_answers(mut lines: Lines<BufReader<OwnedReadHalf>>, waiting: Arc<Mutex<Pending>>) {
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(response) = serde_json::from_str::<Response>(&line) else {
            continue;
        };
        let asker = pending(&waiting).waiting.remove(&response.id);
        if let Some(asker) = asker {
            let _ = asker.send(response.answer);
        }
    }
    let mut pending = pending(&waiting);
    pending.closed = true;
    pending.waiting.clear();
}

/// Who answers the conversation's questions now.
enum Link {
    /// Nobody yet: the daemon is connected to on the next question.
    Unconnected,
    /// The daemon, over this connection.
    Daemon(Arc<Connection>),
    /// Servers of the conversation's own, for the rest of its life.
    Own(Arc<dyn LanguageService>),
}

/// Who answers now, and whether the one reconnection a conversation gets is spent.
struct State {
    /// Who answers now.
    link: Link,
    /// Whether the connection dropped once already.
    reconnected: bool,
}

/// A second, blocking connection to the daemon, for the one question that is asked where nothing can be awaited.
struct Side {
    /// The connection, read by line.
    stream: std::io::BufReader<std::os::unix::net::UnixStream>,
    /// The id the next request is sent under.
    next: u64,
}

/// Language services the user's language server daemon provides, shared with every other conversation in the same project root, with servers of the conversation's own to fall back on.
pub struct DaemonLanguages {
    /// What the daemon is told on connecting: the protocol version, the project root and whether to start its servers at once.
    hello: Hello,
    /// The socket the daemon listens on, when there is a place for one.
    socket: Option<PathBuf>,
    /// The program that runs the daemon with the argument `lsp-daemon`, started when nothing listens; nothing is started without one.
    program: Option<PathBuf>,
    /// Makes the servers to fall back to.
    fallback: Fallback,
    /// Who answers now.
    state: Mutex<State>,
    /// Held while connecting, so that questions asked at once make one connection between them.
    connecting: tokio::sync::Mutex<()>,
    /// The blocking connection [`LanguageService::patience`] asks on, opened when first needed.
    side: Mutex<Option<Side>>,
}

impl DaemonLanguages {
    /// Language services for the project in `root` asked of the user's daemon, which `program` is run to start, as `program lsp-daemon`, when it is not running; the harness's own servers are fallen back to. When `trusted`, the daemon is connected to at once, in the background, and starts the servers of the project's languages right away.
    pub fn shared(root: &Path, trusted: bool, program: PathBuf) -> Arc<Self> {
        let folder = root.to_path_buf();
        let languages = Arc::new(Self::new(
            root,
            trusted,
            socket_path(),
            Some(program),
            Box::new(move || crate::daemon::own_servers(&folder, trusted)),
        ));
        if trusted && let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let connecting = languages.clone();
            runtime.spawn(async move {
                let _ = connecting.link().await;
            });
        }
        languages
    }

    /// Language services for the project in `root` asked of the daemon listening on `socket`, started by running `program` when there is one, with the servers `fallback` makes to fall back to.
    fn new(
        root: &Path,
        trusted: bool,
        socket: Option<PathBuf>,
        program: Option<PathBuf>,
        fallback: Fallback,
    ) -> Self {
        Self {
            hello: Hello {
                version: PROTOCOL_VERSION,
                root: root.to_path_buf(),
                trusted,
            },
            socket,
            program,
            fallback,
            state: Mutex::new(State {
                link: Link::Unconnected,
                reconnected: false,
            }),
            connecting: tokio::sync::Mutex::new(()),
            side: Mutex::new(None),
        }
    }

    /// Who answers now, locked; a lock a panicking task left poisoned is taken all the same.
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|held| held.into_inner())
    }

    /// Who answers now, when that is settled: the connection to the daemon, or, as the error, the conversation's own servers; nothing while unconnected.
    fn settled_link(&self) -> Option<Result<Arc<Connection>, Arc<dyn LanguageService>>> {
        match &self.state().link {
            Link::Unconnected => None,
            Link::Daemon(connection) => Some(Ok(connection.clone())),
            Link::Own(own) => Some(Err(own.clone())),
        }
    }

    /// The connection to the daemon, made now when there is none yet.
    ///
    /// # Errors
    ///
    /// Returns the conversation's own servers when the daemon cannot be used, now or since earlier.
    async fn link(&self) -> Result<Arc<Connection>, Arc<dyn LanguageService>> {
        let _connecting = self.connecting.lock().await;
        if let Some(settled) = self.settled_link() {
            return settled;
        }
        match self.connect().await {
            Some(connection) => {
                let connection = Arc::new(connection);
                self.state().link = Link::Daemon(connection.clone());
                Ok(connection)
            }
            None => Err(self.fall_back()),
        }
    }

    /// Has the conversation's own servers answer from now on, and returns them; they are made the first time only.
    fn fall_back(&self) -> Arc<dyn LanguageService> {
        let mut state = self.state();
        if let Link::Own(own) = &state.link {
            return own.clone();
        }
        let own = (self.fallback)();
        state.link = Link::Own(own.clone());
        own
    }

    /// Notes that `connection` dropped: the first time, the next question connects again; after that the conversation's own servers answer. A connection that is no longer the one in use changes nothing.
    fn dropped(&self, connection: &Arc<Connection>) {
        let spent = {
            let mut state = self.state();
            match &state.link {
                Link::Daemon(current) if Arc::ptr_eq(current, connection) => {}
                _ => return,
            }
            if state.reconnected {
                true
            } else {
                state.reconnected = true;
                state.link = Link::Unconnected;
                false
            }
        };
        if spent {
            self.fall_back();
        }
    }

    /// Connects to the daemon, starting it first when nothing listens and there is a program to start it with; nothing when it cannot be reached or speaks another version of the protocol.
    async fn connect(&self) -> Option<Connection> {
        let socket = self.socket.as_deref()?;
        match Connection::open(socket, &self.hello).await {
            Opened::Connected(connection) => return Some(connection),
            Opened::Mismatched => return None,
            Opened::Absent => {}
        }
        start_daemon(self.program.as_deref()?, socket).ok()?;
        for _ in 0..START_ATTEMPTS {
            tokio::time::sleep(START_PAUSE).await;
            match Connection::open(socket, &self.hello).await {
                Opened::Connected(connection) => return Some(connection),
                Opened::Mismatched => return None,
                Opened::Absent => {}
            }
        }
        None
    }

    /// Asks the daemon `ask`, giving it `patience` to answer, connecting first when needed and once more when the connection drops.
    ///
    /// # Errors
    ///
    /// Returns the conversation's own servers, which answer from now on, when the daemon cannot be reached, dropped the connection a second time or did not answer in time; the caller asks them instead.
    async fn ask(&self, ask: Ask, patience: Duration) -> Result<Answer, Arc<dyn LanguageService>> {
        loop {
            let connection = self.link().await?;
            match connection.ask(&ask, patience).await {
                Ok(answer) => return Ok(answer),
                Err(Lost::Dropped) => self.dropped(&connection),
                Err(Lost::TimedOut) => return Err(self.fall_back()),
            }
        }
    }

    /// Asks the daemon the question about code `ask`.
    ///
    /// # Errors
    ///
    /// As [`DaemonLanguages::ask`].
    async fn navigate(&self, ask: LanguageAsk) -> Result<Answer, Arc<dyn LanguageService>> {
        self.ask(Ask::Language(ask), NAVIGATION_WAIT).await
    }

    /// How long the daemon says to wait for the diagnostics of the files at `paths`, asked on the blocking side connection; nothing when it cannot be asked or does not answer at once.
    fn side_patience(&self, paths: &[PathBuf]) -> Option<Duration> {
        let mut side = self.side.lock().unwrap_or_else(|held| held.into_inner());
        if side.is_none() {
            *side = self.open_side();
        }
        let answered = side.as_mut().and_then(|side| side.patience(paths));
        if answered.is_none() {
            *side = None;
        }
        answered
    }

    /// Opens the blocking side connection and greets the daemon on it; nothing when the daemon is not there or speaks another version of the protocol.
    fn open_side(&self) -> Option<Side> {
        let stream = std::os::unix::net::UnixStream::connect(self.socket.as_deref()?).ok()?;
        stream.set_read_timeout(Some(SIDE_WAIT)).ok()?;
        stream.set_write_timeout(Some(SIDE_WAIT)).ok()?;
        let mut side = Side {
            stream: std::io::BufReader::new(stream),
            next: 1,
        };
        side.write(&self.hello)?;
        let welcome: Welcome = side.read()?;
        (welcome.version == PROTOCOL_VERSION).then_some(side)
    }
}

impl Side {
    /// Sends `message` as one JSON line; nothing when it cannot be sent.
    fn write<T: serde::Serialize>(&mut self, message: &T) -> Option<()> {
        let mut line = serde_json::to_string(message).ok()?;
        line.push('\n');
        self.stream.get_mut().write_all(line.as_bytes()).ok()
    }

    /// Reads the next line as a message; nothing when none comes in time or it is not one.
    fn read<T: serde::de::DeserializeOwned>(&mut self) -> Option<T> {
        let mut line = String::new();
        match self.stream.read_line(&mut line) {
            Ok(read) if read > 0 => serde_json::from_str(&line).ok(),
            _ => None,
        }
    }

    /// Asks the daemon how long to wait for the diagnostics of the files at `paths`; nothing when it does not say in time.
    fn patience(&mut self, paths: &[PathBuf]) -> Option<Duration> {
        let id = self.next;
        self.next += 1;
        self.write(&Request {
            id,
            ask: Ask::Patience {
                paths: paths.to_vec(),
            },
        })?;
        let response: Response = self.read()?;
        match response.answer {
            Answer::Patience { wait_ms } if response.id == id => {
                Some(Duration::from_millis(wait_ms))
            }
            _ => None,
        }
    }
}

/// Starts the daemon by running `program lsp-daemon`, listening on `socket`, in a process group of its own with no standard streams, so that it outlives the conversation and is not interrupted with it.
///
/// # Errors
///
/// Fails when the program cannot be started.
fn start_daemon(program: &Path, socket: &Path) -> std::io::Result<()> {
    let mut command = std::process::Command::new(program);
    command
        .arg("lsp-daemon")
        .env(SOCKET_VARIABLE, socket)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0);
    if let Some(folder) = socket.parent().filter(|folder| folder.is_dir()) {
        command.current_dir(folder);
    }
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// What the model is told when the daemon answers a question about `what` with an answer to another kind of question.
fn answered_otherwise(what: &str) -> LanguageError {
    LanguageError::Failed(format!(
        "the language server daemon answered the {what} question with something else"
    ))
}

#[async_trait]
impl LanguageService for DaemonLanguages {
    /// Asks the daemon to format the file at `path`.
    async fn format(&self, path: &Path) -> Option<String> {
        let ask = LanguageAsk::Format {
            path: path.to_path_buf(),
        };
        match self.ask(Ask::Language(ask), FORMAT_WAIT + SLACK).await {
            Err(own) => own.format(path).await,
            Ok(Answer::Language(LanguageAnswer::Formatted { text })) => text,
            Ok(_) => None,
        }
    }

    /// Asks the daemon what is wrong with the files at `paths`, letting its servers wait up to `wait` for fresh diagnostics.
    async fn diagnostics(&self, paths: &[PathBuf], wait: Duration) -> Vec<Diagnostic> {
        let ask = LanguageAsk::Diagnostics {
            paths: paths.to_vec(),
            wait_ms: millis(wait),
        };
        match self.ask(Ask::Language(ask), wait + SLACK).await {
            Err(own) => own.diagnostics(paths, wait).await,
            Ok(Answer::Language(LanguageAnswer::Diagnostics { diagnostics })) => diagnostics,
            Ok(_) => Vec::new(),
        }
    }

    /// How long the daemon's servers say to wait for the diagnostics of the files at `paths`. Before the daemon was ever reached this is as long as servers not started yet ask for; when it does not say at once, the usual wait.
    fn patience(&self, paths: &[PathBuf]) -> Duration {
        let connected = match self.settled_link() {
            Some(Err(own)) => return own.patience(paths),
            Some(Ok(_)) => true,
            None => false,
        };
        match self.side_patience(paths) {
            Some(wait) => wait,
            None if connected => DIAGNOSTICS_WAIT,
            None => LOADING_WAIT,
        }
    }

    /// Asks the daemon whether a server reads the file at `path`.
    async fn serves(&self, path: &Path) -> Result<(), LanguageError> {
        let ask = Ask::Serves {
            path: path.to_path_buf(),
        };
        match self.ask(ask, NAVIGATION_WAIT).await {
            Err(own) => own.serves(path).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Done {}) => Ok(()),
                _ => Err(answered_otherwise("serves")),
            },
        }
    }

    /// Asks the daemon where `lookup` leads from the symbol at `at`.
    async fn locate(&self, lookup: Lookup, at: &Spot) -> Result<Vec<Location>, LanguageError> {
        let spot = place(at);
        let ask = match lookup {
            Lookup::Definition => LanguageAsk::Definition { at: spot },
            Lookup::TypeDefinition => LanguageAsk::TypeDefinition { at: spot },
            Lookup::Implementation => LanguageAsk::Implementation { at: spot },
            Lookup::References => LanguageAsk::References { at: spot },
        };
        match self.navigate(ask).await {
            Err(own) => own.locate(lookup, at).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Locations { locations }) => {
                    Ok(locations.into_iter().map(location).collect())
                }
                _ => Err(answered_otherwise(lookup.noun())),
            },
        }
    }

    /// Asks the daemon what the symbol at `at` is.
    async fn hover(&self, at: &Spot) -> Result<Option<String>, LanguageError> {
        match self.navigate(LanguageAsk::Hover { at: place(at) }).await {
            Err(own) => own.hover(at).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Hover { text }) => Ok(text),
                _ => Err(answered_otherwise("hover")),
            },
        }
    }

    /// Asks the daemon for the symbols of the file at `path`.
    async fn document_symbols(&self, path: &Path) -> Result<Vec<Symbol>, LanguageError> {
        let ask = LanguageAsk::DocumentSymbols {
            path: path.to_path_buf(),
        };
        match self.navigate(ask).await {
            Err(own) => own.document_symbols(path).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Symbols { symbols }) => {
                    Ok(symbols.into_iter().map(symbol).collect())
                }
                _ => Err(answered_otherwise("document symbols")),
            },
        }
    }

    /// Asks the daemon for the project's symbols matching `query`, of the server of the file at `near` when it is given.
    async fn workspace_symbols(
        &self,
        query: &str,
        near: Option<&Path>,
    ) -> Result<Vec<Symbol>, LanguageError> {
        let ask = LanguageAsk::WorkspaceSymbols {
            query: query.to_owned(),
            near: near.map(Path::to_path_buf),
        };
        match self.navigate(ask).await {
            Err(own) => own.workspace_symbols(query, near).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Symbols { symbols }) => {
                    Ok(symbols.into_iter().map(symbol).collect())
                }
                _ => Err(answered_otherwise("workspace symbols")),
            },
        }
    }

    /// Asks the daemon for the files renaming the symbol at `at` to `new_name` changes, each with its whole new text.
    async fn rename(&self, at: &Spot, new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
        let ask = LanguageAsk::Rename {
            at: place(at),
            new_name: new_name.to_owned(),
        };
        match self.navigate(ask).await {
            Err(own) => own.rename(at, new_name).await,
            Ok(answer) => match settled(answer)? {
                Answer::Rewritten { files } => Ok(files.into_iter().map(renamed).collect()),
                _ => Err(answered_otherwise("rename")),
            },
        }
    }

    /// Asks the daemon everything its servers have said is wrong with the project, letting them wait up to `wait`.
    async fn all_diagnostics(&self, wait: Duration) -> Result<Vec<Diagnostic>, LanguageError> {
        let ask = LanguageAsk::WorkspaceDiagnostics {
            wait_ms: millis(wait),
        };
        match self.ask(Ask::Language(ask), wait + SLACK).await {
            Err(own) => own.all_diagnostics(wait).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Diagnostics { diagnostics }) => Ok(diagnostics),
                _ => Err(answered_otherwise("workspace diagnostics")),
            },
        }
    }

    /// Asks the daemon to format the whole file at `path`, or only the stretch `range` of it.
    async fn reformat(
        &self,
        path: &Path,
        range: Option<&Location>,
    ) -> Result<Option<String>, LanguageError> {
        let ask = match range {
            Some(range) => Ask::Language(LanguageAsk::FormatRange {
                range: stretch(range),
            }),
            None => Ask::Reformat {
                path: path.to_path_buf(),
            },
        };
        match self.ask(ask, NAVIGATION_WAIT).await {
            Err(own) => own.reformat(path, range).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Formatted { text }) => Ok(text),
                _ => Err(answered_otherwise("format")),
            },
        }
    }

    /// Asks the daemon for the code actions offered for `range`.
    async fn code_actions(
        &self,
        range: &Location,
        only: &[String],
    ) -> Result<Vec<CodeAction>, LanguageError> {
        let ask = LanguageAsk::CodeActions {
            range: stretch(range),
            only: only.to_vec(),
        };
        match self.navigate(ask).await {
            Err(own) => own.code_actions(range, only).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Actions { actions }) => {
                    Ok(actions.into_iter().map(action).collect())
                }
                _ => Err(answered_otherwise("code actions")),
            },
        }
    }

    /// Asks the daemon for the files applying the chosen code action changes, each with its whole new text.
    async fn apply_code_action(
        &self,
        range: &Location,
        only: &[String],
        chosen: &Chosen,
    ) -> Result<Applied, LanguageError> {
        let ask = LanguageAsk::ApplyCodeAction {
            range: stretch(range),
            only: only.to_vec(),
            index: u32::try_from(chosen.index).unwrap_or(u32::MAX),
            title: chosen.title.clone(),
        };
        match self.navigate(ask).await {
            Err(own) => own.apply_code_action(range, only, chosen).await,
            Ok(answer) => match settled(answer)? {
                Answer::Applied { title, files } => Ok(Applied {
                    title,
                    files: files.into_iter().map(renamed).collect(),
                }),
                _ => Err(answered_otherwise("apply code action")),
            },
        }
    }

    /// Asks the daemon for the files moving the file at `from` to `to` changes, each with its whole new text.
    async fn rename_file(&self, from: &Path, to: &Path) -> Result<Vec<Renamed>, LanguageError> {
        let ask = LanguageAsk::WillRenameFile {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        };
        match self.navigate(ask).await {
            Err(own) => own.rename_file(from, to).await,
            Ok(answer) => match settled(answer)? {
                Answer::Rewritten { files } => Ok(files.into_iter().map(renamed).collect()),
                _ => Err(answered_otherwise("rename file")),
            },
        }
    }

    /// Tells the daemon's servers that the file at `from` is now at `to`.
    async fn file_renamed(&self, from: &Path, to: &Path) {
        let ask = LanguageAsk::FileRenamed {
            from: from.to_path_buf(),
            to: to.to_path_buf(),
        };
        if let Err(own) = self.navigate(ask).await {
            own.file_renamed(from, to).await;
        }
    }

    /// Asks the daemon which servers run for the project and how they stand.
    async fn status(&self) -> Result<Vec<ServerStatus>, LanguageError> {
        match self.navigate(LanguageAsk::Status {}).await {
            Err(own) => own.status().await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Servers { servers }) => {
                    Ok(servers.into_iter().map(server).collect())
                }
                _ => Err(answered_otherwise("status")),
            },
        }
    }

    /// Asks the daemon to restart the server named `name`, or that of the file at `near`, or all of the project's; every conversation sharing them sees the restart.
    async fn reload(
        &self,
        name: Option<&str>,
        near: Option<&Path>,
    ) -> Result<Vec<ServerStatus>, LanguageError> {
        let ask = LanguageAsk::Reload {
            server: name.map(str::to_owned),
            near: near.map(Path::to_path_buf),
        };
        match self.navigate(ask).await {
            Err(own) => own.reload(name, near).await,
            Ok(answer) => match settled(answer)? {
                Answer::Language(LanguageAnswer::Servers { servers }) => {
                    Ok(servers.into_iter().map(server).collect())
                }
                _ => Err(answered_otherwise("reload")),
            },
        }
    }
}

#[cfg(test)]
/// Tests for sharing language services through a daemon, with made-up services in place of real servers.
mod tests {
    use std::sync::atomic::AtomicUsize;

    use tokio::net::UnixListener;

    use super::*;
    use crate::daemon::{Daemon, Factory};

    /// A language service that only says which one it is: its hover names it, and it asks for a wait of its own for diagnostics.
    struct Named {
        /// What its hover says.
        name: String,
    }

    #[async_trait]
    impl LanguageService for Named {
        /// Formats nothing.
        async fn format(&self, _path: &Path) -> Option<String> {
            None
        }

        /// Finds nothing wrong.
        async fn diagnostics(&self, _paths: &[PathBuf], _wait: Duration) -> Vec<Diagnostic> {
            Vec::new()
        }

        /// A wait no other service asks for.
        fn patience(&self, _paths: &[PathBuf]) -> Duration {
            Duration::from_millis(4321)
        }

        /// Its name.
        async fn hover(&self, _at: &Spot) -> Result<Option<String>, LanguageError> {
            Ok(Some(self.name.clone()))
        }

        /// One file with a whole new text and two edits.
        async fn rename(&self, at: &Spot, new_name: &str) -> Result<Vec<Renamed>, LanguageError> {
            Ok(vec![Renamed {
                path: at.path.clone(),
                text: format!("fn {new_name}() {{}}\n"),
                edits: 2,
            }])
        }
    }

    /// An empty folder for the test `name` under the workspace's git-ignored `build/tests`, or under the user's scratch folder when the workspace's path is too long for a socket's.
    fn scratch(name: &str) -> PathBuf {
        let tests = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-lsp/d");
        std::fs::create_dir_all(&tests).unwrap();
        let mut dir = tests.canonicalize().unwrap().join(name);
        if dir.as_os_str().len() > 90 {
            dir = dirs::cache_dir()
                .unwrap()
                .join("scratch/oh-my-crab")
                .join(name);
        }
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A factory whose services are named `daemon 1`, `daemon 2` and so on, in the order they are made, and the count of those made.
    fn counting() -> (Factory, Arc<AtomicUsize>) {
        let made = Arc::new(AtomicUsize::new(0));
        let count = made.clone();
        let factory: Factory = Box::new(move |_root, _warm| {
            let number = count.fetch_add(1, Ordering::SeqCst) + 1;
            Arc::new(Named {
                name: format!("daemon {number}"),
            })
        });
        (factory, made)
    }

    /// Serves a daemon of counted services on the socket `s` in `dir`, releasing roots after `idle`; returns the socket, the count of services made and the task serving.
    fn served(
        dir: &Path,
        idle: Duration,
    ) -> (PathBuf, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
        let socket = dir.join("s");
        let (factory, made) = counting();
        let listener = UnixListener::bind(&socket).unwrap();
        let daemon = Arc::new(Daemon::new(factory, idle));
        (socket, made, tokio::spawn(daemon.serve(listener)))
    }

    /// A conversation in `root` that asks the daemon on `socket`, starts none, and falls back to a service named `own`.
    fn conversation(root: &Path, socket: &Path) -> DaemonLanguages {
        DaemonLanguages::new(
            root,
            false,
            Some(socket.to_path_buf()),
            None,
            Box::new(|| Arc::new(Named { name: "own".into() })),
        )
    }

    /// What the service answering `languages` says it is.
    async fn who(languages: &DaemonLanguages) -> String {
        let at = Spot {
            path: PathBuf::from("/w/a.rs"),
            line: 1,
            column: 1,
        };
        languages.hover(&at).await.unwrap().unwrap()
    }

    /// Two conversations in one root are answered by one service, and a conversation in another root by another.
    #[tokio::test]
    async fn conversations_in_one_root_share_a_service() {
        let dir = scratch("share");
        let (socket, made, serving) = served(&dir, Duration::from_secs(60));
        let (one, other) = (dir.join("one"), dir.join("other"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let first = conversation(&one, &socket);
        let second = conversation(&one.join("../one"), &socket);
        let third = conversation(&other, &socket);
        assert_eq!(who(&first).await, "daemon 1");
        assert_eq!(who(&second).await, "daemon 1");
        assert_eq!(who(&third).await, "daemon 2");
        assert_eq!(who(&first).await, "daemon 1");
        assert_eq!(made.load(Ordering::SeqCst), 2);
        serving.abort();
    }

    /// Whole new texts cross as they are, and questions asked at once on one connection each get their own answer.
    #[tokio::test]
    async fn answers_come_back_to_their_questions() {
        let dir = scratch("answers");
        let (socket, _, serving) = served(&dir, Duration::from_secs(60));
        let languages = conversation(&dir, &socket);
        let at = Spot {
            path: dir.join("a.rs"),
            line: 1,
            column: 4,
        };
        let (renamed, hover, formatted) = tokio::join!(
            languages.rename(&at, "new"),
            languages.hover(&at),
            languages.format(&at.path)
        );
        assert_eq!(
            renamed.unwrap(),
            vec![Renamed {
                path: at.path.clone(),
                text: "fn new() {}\n".into(),
                edits: 2,
            }]
        );
        assert_eq!(hover.unwrap().as_deref(), Some("daemon 1"));
        assert_eq!(formatted, None);
        assert!(languages.status().await.is_err());
        serving.abort();
    }

    /// The wait for diagnostics is asked of the daemon's service, where nothing can be awaited.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn patience_is_the_daemons() {
        let dir = scratch("patience");
        let (socket, _, serving) = served(&dir, Duration::from_secs(60));
        let languages = conversation(&dir, &socket);
        assert_eq!(who(&languages).await, "daemon 1");
        assert_eq!(languages.patience(&[]), Duration::from_millis(4321));
        assert_eq!(languages.patience(&[]), Duration::from_millis(4321));
        serving.abort();
    }

    /// A root nobody asked about for the idle time gets a new service the next time it is asked about, and the daemon ends once nobody has been connected for as long.
    #[tokio::test]
    async fn idle_roots_are_released_and_an_idle_daemon_ends() {
        let dir = scratch("idle");
        let (socket, made, serving) = served(&dir, Duration::from_millis(200));
        let languages = conversation(&dir, &socket);
        assert_eq!(who(&languages).await, "daemon 1");
        assert_eq!(who(&languages).await, "daemon 1");
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(!serving.is_finished());
        assert_eq!(who(&languages).await, "daemon 2");
        assert_eq!(made.load(Ordering::SeqCst), 2);
        drop(languages);
        tokio::time::timeout(Duration::from_secs(10), serving)
            .await
            .expect("the daemon ends when nobody is connected")
            .unwrap();
    }

    /// A daemon that speaks another version of the protocol is not asked: the conversation's own servers answer.
    #[tokio::test]
    async fn another_version_falls_back_to_own_servers() {
        let dir = scratch("version");
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let other = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let _ = lines.next_line().await;
                let welcome = Welcome {
                    version: PROTOCOL_VERSION + 1,
                    pid: 0,
                };
                let line = serde_json::to_string(&welcome).unwrap() + "\n";
                let _ = write.write_all(line.as_bytes()).await;
            }
        });
        let languages = conversation(&dir, &socket);
        assert_eq!(who(&languages).await, "own");
        assert_eq!(who(&languages).await, "own");
        assert_eq!(languages.patience(&[]), Duration::from_millis(4321));
        other.abort();
    }

    /// The daemon hangs up on a conversation that speaks another version, after naming its own.
    #[tokio::test]
    async fn the_daemon_refuses_another_version() {
        let dir = scratch("refuse");
        let (socket, made, serving) = served(&dir, Duration::from_secs(60));
        let hello = Hello {
            version: PROTOCOL_VERSION + 1,
            root: dir.clone(),
            trusted: true,
        };
        let stream = UnixStream::connect(&socket).await.unwrap();
        let (read, mut write) = stream.into_split();
        let line = serde_json::to_string(&hello).unwrap() + "\n";
        write.write_all(line.as_bytes()).await.unwrap();
        let mut lines = BufReader::new(read).lines();
        let welcome: Welcome =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(welcome.version, PROTOCOL_VERSION);
        assert_eq!(lines.next_line().await.unwrap(), None);
        assert_eq!(made.load(Ordering::SeqCst), 0);
        serving.abort();
    }

    /// A socket nothing listens on, missing or left behind by a daemon that died, falls back to the conversation's own servers, since there is no program to start a daemon with.
    #[tokio::test]
    async fn a_dead_socket_falls_back_to_own_servers() {
        let dir = scratch("dead");
        let missing = conversation(&dir, &dir.join("none"));
        assert_eq!(who(&missing).await, "own");
        let socket = dir.join("s");
        drop(UnixListener::bind(&socket).unwrap());
        assert!(socket.exists());
        let stale = conversation(&dir, &socket);
        assert_eq!(who(&stale).await, "own");
        assert_eq!(stale.patience(&[]), Duration::from_millis(4321));
    }

    /// A connection that drops is made again once; when it drops a second time the conversation's own servers answer, even though the daemon is back.
    #[tokio::test]
    async fn a_dropped_connection_is_made_again_once() {
        let dir = scratch("drop");
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let hanging_up = tokio::spawn(async move {
            for _ in 0..2 {
                let (stream, _) = listener.accept().await.unwrap();
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                let _ = lines.next_line().await;
                let welcome = Welcome {
                    version: PROTOCOL_VERSION,
                    pid: 0,
                };
                let line = serde_json::to_string(&welcome).unwrap() + "\n";
                let _ = write.write_all(line.as_bytes()).await;
                let _ = lines.next_line().await;
            }
            listener
        });
        let languages = conversation(&dir, &socket);
        assert_eq!(who(&languages).await, "own");
        let listener = hanging_up.await.unwrap();
        let (factory, made) = counting();
        let serving =
            tokio::spawn(Arc::new(Daemon::new(factory, Duration::from_secs(60))).serve(listener));
        assert_eq!(who(&languages).await, "own");
        assert_eq!(made.load(Ordering::SeqCst), 0);
        serving.abort();
    }

    /// A conversation whose daemon stopped listening for good goes on with its own servers after one more try.
    #[tokio::test]
    async fn a_daemon_that_dies_is_replaced_by_own_servers() {
        let dir = scratch("dies");
        let socket = dir.join("s");
        let listener = UnixListener::bind(&socket).unwrap();
        let once = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read, mut write) = stream.into_split();
            let mut lines = BufReader::new(read).lines();
            let _ = lines.next_line().await;
            let welcome = Welcome {
                version: PROTOCOL_VERSION,
                pid: 0,
            };
            let line = serde_json::to_string(&welcome).unwrap() + "\n";
            let _ = write.write_all(line.as_bytes()).await;
            let _ = lines.next_line().await;
        });
        let languages = conversation(&dir, &socket);
        assert_eq!(who(&languages).await, "own");
        once.await.unwrap();
    }
}
