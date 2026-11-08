//! The extensions of one conversation: started, asked, restarted when they stop, and shut down.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::join_all;
use omc_tools::Tool;
use serde_json::{Map, Value, json};

use crate::config::Extension;
use crate::process::{Connection, Failure};
use crate::tool::{ExtensionTool, Offered};

/// The version of the protocol crab speaks, which `initialize` tells an extension.
pub(crate) const PROTOCOL: u64 = 1;

/// How many times an extension may stop within [`RESTART_WINDOW`] and be started again; once more and it stays off for the conversation.
const MAX_RESTARTS: usize = 3;

/// The span within which stops count against [`MAX_RESTARTS`].
const RESTART_WINDOW: Duration = Duration::from_secs(300);

/// The permission mode extensions are told, in Claude Code's words: `default`, `plan` or `bypassPermissions`.
pub type ModeNow = dyn Fn() -> &'static str + Send + Sync;

/// What every extension of a conversation is told and started with.
pub struct Setting {
    /// The conversation's id, when it has one.
    pub session: Option<String>,
    /// The folder the conversation works in, which extensions without a folder of their own start in.
    pub folder: PathBuf,
    /// The permission mode as it stands when an extension is told something.
    pub mode: Arc<ModeNow>,
}

/// The extensions of one conversation.
pub struct Extensions {
    /// What they are told and started with.
    pub(crate) setting: Arc<Setting>,
    /// The extensions started so far, in the order they were declared.
    running: Mutex<Vec<Arc<Running>>>,
}

impl Extensions {
    /// A conversation's extensions, none started yet, run with `setting`.
    pub fn new(setting: Setting) -> Arc<Self> {
        Arc::new(Self {
            setting: Arc::new(setting),
            running: Mutex::new(Vec::new()),
        })
    }

    /// Starts those of `wanted` that were not started yet, all at once, waiting for each to say what it wants; returns the tools the ones that started give the model. One that cannot start, or does not answer `initialize` in time, is reported on standard error and left out for the conversation, since crab never learned what it wants.
    pub async fn start(&self, wanted: &[Extension]) -> Vec<Arc<dyn Tool>> {
        let fresh: Vec<Arc<Running>> = {
            let mut running = self.lock();
            let fresh: Vec<Arc<Running>> = wanted
                .iter()
                .filter(|extension| !running.iter().any(|kept| kept.spec.name == extension.name))
                .map(|extension| Arc::new(Running::new(extension.clone())))
                .collect();
            running.extend(fresh.iter().cloned());
            fresh
        };
        let started = join_all(fresh.iter().map(|one| one.connection(&self.setting))).await;
        let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
        for (one, connected) in fresh.iter().zip(started) {
            if connected.is_none() {
                continue;
            }
            for offered in one.offered() {
                tools.push(Arc::new(ExtensionTool::new(
                    offered,
                    one.clone(),
                    self.setting.clone(),
                )));
            }
        }
        tools
    }

    /// The names of the extensions started so far.
    pub fn names(&self) -> Vec<String> {
        self.lock()
            .iter()
            .map(|one| one.spec.name.clone())
            .collect()
    }

    /// Whether any extension running now wants `event`.
    pub fn wants(&self, event: &str) -> bool {
        self.lock().iter().any(|one| one.wants(event))
    }

    /// The extensions started so far that want `event`.
    pub(crate) fn wanting(&self, event: &str) -> Vec<Arc<Running>> {
        self.lock()
            .iter()
            .filter(|one| one.wants(event))
            .cloned()
            .collect()
    }

    /// Tells every extension that wants `event` of it with `fields` besides the common ones, without waiting for answers.
    pub(crate) async fn tell(&self, event: &str, fields: Map<String, Value>) {
        for one in self.wanting(event) {
            if let Some(connection) = one.connection(&self.setting).await {
                connection.notify("event", self.payload(event, fields.clone()));
            }
        }
    }

    /// The parameters of an `event` message for `event`: the common fields, then `fields`.
    pub(crate) fn payload(&self, event: &str, fields: Map<String, Value>) -> Value {
        let mut payload = Map::new();
        payload.insert("event".into(), json!(event));
        payload.insert(
            "session_id".into(),
            json!(self.setting.session.clone().unwrap_or_default()),
        );
        payload.insert("cwd".into(), json!(self.setting.folder));
        payload.insert("permission_mode".into(), json!((self.setting.mode)()));
        payload.extend(fields);
        Value::Object(payload)
    }

    /// Shuts every extension down, as when the conversation ends: each is sent `shutdown`, its input is closed, and it is killed when it has not ended within a second.
    pub async fn shutdown(&self) {
        let running: Vec<Arc<Running>> = self.lock().drain(..).collect();
        join_all(running.iter().map(|one| one.shutdown())).await;
    }

    /// The extensions started so far, held.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Arc<Running>>> {
        self.running.lock().unwrap_or_else(|held| held.into_inner())
    }
}

/// One extension of a conversation, started once and again when it stops.
pub(crate) struct Running {
    /// How it is declared.
    pub(crate) spec: Extension,
    /// The program as it runs now, and when it stopped before.
    state: tokio::sync::Mutex<State>,
    /// The events it said it wants.
    events: Mutex<HashSet<String>>,
    /// The tools it said, when it first started, it gives the model.
    tools: Mutex<Vec<Offered>>,
    /// Whether it stopped too often and stays off for the conversation.
    off: AtomicBool,
}

/// The program of an extension as it runs now.
#[derive(Default)]
struct State {
    /// The running program, when it was started.
    connection: Option<Arc<Connection>>,
    /// When it stopped or failed to start, within the restart window.
    stops: Vec<Instant>,
    /// Whether it was ever started, so that a later start is a restart.
    started: bool,
}

impl Running {
    /// The extension `spec`, not started yet.
    fn new(spec: Extension) -> Self {
        Self {
            spec,
            state: tokio::sync::Mutex::new(State::default()),
            events: Mutex::new(HashSet::new()),
            tools: Mutex::new(Vec::new()),
            off: AtomicBool::new(false),
        }
    }

    /// Whether it wants `event` and is not off.
    pub(crate) fn wants(&self, event: &str) -> bool {
        !self.off.load(Ordering::SeqCst)
            && self
                .events
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .contains(event)
    }

    /// The tools it gives the model.
    fn offered(&self) -> Vec<Offered> {
        self.tools
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// The running program, started first when it is not running: the first time, or again after it stopped, unless it stopped [`MAX_RESTARTS`] times within [`RESTART_WINDOW`], which turns it off for the conversation. Nothing when it is off or cannot be started, which is reported on standard error.
    pub(crate) async fn connection(&self, setting: &Setting) -> Option<Arc<Connection>> {
        if self.off.load(Ordering::SeqCst) {
            return None;
        }
        let mut state = self.state.lock().await;
        if let Some(connection) = state.connection.as_ref().filter(|c| c.alive()) {
            return Some(connection.clone());
        }
        if state.connection.take().is_some() {
            state.stops.push(Instant::now());
        }
        state
            .stops
            .retain(|stopped| stopped.elapsed() < RESTART_WINDOW);
        if state.stops.len() > MAX_RESTARTS {
            self.off.store(true, Ordering::SeqCst);
            eprintln!(
                "extension {} stopped {} times within {} minutes and stays off for this conversation",
                self.spec.name,
                state.stops.len(),
                RESTART_WINDOW.as_secs() / 60
            );
            return None;
        }
        if state.started {
            eprintln!("extension {} stopped; starting it again", self.spec.name);
        }
        state.started = true;
        match self.launch(setting).await {
            Ok(connection) => {
                let connection = Arc::new(connection);
                state.connection = Some(connection.clone());
                Some(connection)
            }
            Err(reason) => {
                state.stops.push(Instant::now());
                eprintln!("extension {} unavailable: {reason}", self.spec.name);
                None
            }
        }
    }

    /// Starts the program and asks it `initialize`, keeping the events it wants and, the first time, the tools it gives.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program cannot be started or does not answer `initialize` with an object in time.
    async fn launch(&self, setting: &Setting) -> Result<Connection, String> {
        let connection = Connection::start(&self.spec, &setting.folder)?;
        let answer = connection
            .request(
                "initialize",
                json!({
                    "protocol": PROTOCOL,
                    "crab_version": env!("CARGO_PKG_VERSION"),
                    "name": self.spec.name,
                    "session_id": setting.session.clone().unwrap_or_default(),
                    "cwd": setting.folder,
                }),
                self.spec.timeout,
            )
            .await
            .map_err(|failure| format!("initialize failed: {failure}"))?;
        if !answer.is_object() {
            return Err("initialize must be answered with an object".to_owned());
        }
        let events: HashSet<String> = answer
            .get("events")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect();
        *self.events.lock().unwrap_or_else(|held| held.into_inner()) = events;
        let mut tools = self.tools.lock().unwrap_or_else(|held| held.into_inner());
        if tools.is_empty() {
            *tools = answer
                .get("tools")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|tool| match Offered::read(tool) {
                    Ok(offered) => Some(offered),
                    Err(reason) => {
                        eprintln!("extension {}: tool skipped: {reason}", self.spec.name);
                        None
                    }
                })
                .collect();
        }
        Ok(connection)
    }

    /// Asks the extension about `event` with `payload` and waits at most its timeout for the answer.
    ///
    /// # Errors
    ///
    /// Fails when it is off, cannot be started, does not answer in time, stops first or answers with an error; the reason is reported on standard error too.
    pub(crate) async fn ask(&self, setting: &Setting, payload: Value) -> Result<Value, Failure> {
        let Some(connection) = self.connection(setting).await else {
            return Err(Failure::Died("it is not running".to_owned()));
        };
        let answer = connection
            .request("event", payload, self.spec.timeout)
            .await;
        if let Err(failure) = &answer {
            eprintln!("extension {} gave no answer: {failure}", self.spec.name);
        }
        answer
    }

    /// Runs its tool with `params`, waiting at most its tool timeout.
    ///
    /// # Errors
    ///
    /// Fails when it is off, cannot be started, does not answer in time, stops first or answers with an error.
    pub(crate) async fn call(&self, setting: &Setting, params: Value) -> Result<Value, Failure> {
        let Some(connection) = self.connection(setting).await else {
            return Err(Failure::Died("it is not running".to_owned()));
        };
        connection
            .request("tool", params, self.spec.tool_timeout)
            .await
    }

    /// Shuts the program down when it runs.
    async fn shutdown(&self) {
        let connection = self.state.lock().await.connection.take();
        if let Some(connection) = connection {
            connection.shutdown().await;
        }
    }
}
