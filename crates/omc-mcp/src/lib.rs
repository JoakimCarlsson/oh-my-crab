//! Tool servers as tools of the agent: finds the servers set up in files, connects to each, lists its tools and resources, signs in to addresses that want OAuth, and calls them.

mod client;
pub mod config;
mod http;
mod oauth;
mod resources;
mod stdio;
mod store;
mod tool;

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use omc_tools::{Spill, Tool};
use omc_wire::{Reach, ToolServer};
use serde_json::Value;

pub use config::{Origin, Server};
pub use resources::RESOURCES_TOOL;
pub use store::{Grant, MCP_AUTH_FILE_VARIABLE, Tokens};

use crate::client::Client;
use crate::http::Bearer;
use crate::oauth::Challenge;
use crate::resources::{Offering, Resources};
use crate::tool::McpTool;

/// How long a server has to connect and list its tools.
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a sign-in waits for the reader to finish it in the browser.
const SIGN_IN_PATIENCE: Duration = Duration::from_secs(600);

/// Why a server could not be reached.
pub(crate) enum Failure {
    /// The server wants a token crab does not have, and asked for one like this.
    Unauthorized(Challenge),
    /// Anything else, with the reason.
    Other(String),
}

impl Failure {
    /// The failure as a reason to report.
    pub(crate) fn into_reason(self) -> String {
        match self {
            Self::Unauthorized(_) => "the tool server wants signing in again".to_owned(),
            Self::Other(reason) => reason,
        }
    }
}

/// What the hub tells the conversation about signing in to servers, as it happens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    /// The server `server` wants the reader to sign in at `url`, which a browser opens.
    SignIn {
        /// The server's name.
        server: String,
        /// The sign-in page.
        url: String,
    },
    /// The reader signed in to `server`, whose tools are ready for the next turn.
    SignedIn {
        /// The server's name.
        server: String,
    },
    /// Signing in to `server`, or reaching it afterwards, failed.
    Failed {
        /// The server's name.
        server: String,
        /// Why.
        reason: String,
    },
}

/// What the hub tells notices to.
pub type Listener = Arc<dyn Fn(Notice) + Send + Sync>;

/// The tools of the servers that were reached, and the reasons the others give.
#[derive(Default)]
pub struct Reached {
    /// The tools, the servers' and the resources tool.
    pub tools: Vec<Arc<dyn Tool>>,
    /// One line for each server that could not be reached: its name and why.
    pub failed: Vec<String>,
}

/// The tool servers of one conversation: which were reached, which wait for a sign-in, and the tools that arrived since.
pub struct Hub {
    /// Where text too long to show is saved whole.
    spill: Spill,
    /// Where sign-ins are kept, when they can be.
    tokens: Option<Tokens>,
    /// What notices are told to.
    listener: Listener,
    /// The servers that offer resources.
    offering: Offering,
    /// Whether the resources tool was handed out.
    resources_given: AtomicBool,
    /// The names of the servers tried, reached or not.
    tried: std::sync::Mutex<HashSet<String>>,
    /// The tools of servers reached after a sign-in, not yet taken.
    arrived: std::sync::Mutex<Vec<Arc<dyn Tool>>>,
    /// The sign-ins running, stopped when the hub is dropped.
    signing: std::sync::Mutex<Vec<tokio::task::AbortHandle>>,
}

impl Drop for Hub {
    /// Stops the sign-ins still waiting.
    fn drop(&mut self) {
        for task in self
            .signing
            .get_mut()
            .unwrap_or_else(|held| held.into_inner())
            .drain(..)
        {
            task.abort();
        }
    }
}

impl Hub {
    /// A hub saving text too long to show in `spill`, keeping sign-ins in `tokens` when there is a place for them, and telling `listener` about sign-ins.
    pub fn new(spill: Spill, tokens: Option<Tokens>, listener: Listener) -> Arc<Self> {
        Arc::new(Self {
            spill,
            tokens,
            listener,
            offering: Arc::new(std::sync::Mutex::new(Vec::new())),
            resources_given: AtomicBool::new(false),
            tried: std::sync::Mutex::new(HashSet::new()),
            arrived: std::sync::Mutex::new(Vec::new()),
            signing: std::sync::Mutex::new(Vec::new()),
        })
    }

    /// Whether the server called `name` was tried already.
    pub fn tried(&self, name: &str) -> bool {
        self.tried
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .contains(name)
    }

    /// Connects to those of `servers` not tried yet, all at once, each given [`PATIENCE`] to answer, and returns their tools; the resources tool comes with the first server that offers resources. A server that wants a sign-in starts one, which the listener is told the page of, and is reported as waiting; its tools arrive once the reader signed in, for [`Hub::arrived`] to take.
    pub async fn connect(self: &Arc<Self>, servers: &[Server]) -> Reached {
        let fresh: Vec<&Server> = {
            let mut tried = self.tried.lock().unwrap_or_else(|held| held.into_inner());
            servers
                .iter()
                .filter(|server| tried.insert(server.name.clone()))
                .collect()
        };
        let attempts = fresh.into_iter().map(|server| async move {
            match tokio::time::timeout(PATIENCE, self.reach(server)).await {
                Ok(result) => result.map_err(|reason| format!("{}: {reason}", server.name)),
                Err(_) => Err(format!("{}: did not answer in time", server.name)),
            }
        });
        let mut reached = Reached::default();
        for attempt in futures_util::future::join_all(attempts).await {
            match attempt {
                Ok(tools) => reached.tools.extend(tools),
                Err(reason) => reached.failed.push(reason),
            }
        }
        reached
    }

    /// The tools of servers reached after a sign-in since this was last asked.
    pub fn arrived(&self) -> Vec<Arc<dyn Tool>> {
        std::mem::take(&mut *self.arrived.lock().unwrap_or_else(|held| held.into_inner()))
    }

    /// The token that `server` is signed in with, when it is an address and sign-ins can be kept.
    fn bearer(&self, server: &Server) -> Option<Arc<Bearer>> {
        match (&server.reach, &self.tokens) {
            (Reach::Http { url, .. }, Some(tokens)) => {
                Some(Arc::new(Bearer::new(tokens.clone(), url)))
            }
            _ => None,
        }
    }

    /// Connects to `server` and returns its tools, starting a sign-in when it wants one.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot be reached, or with a note of the sign-in it waits for.
    async fn reach(self: &Arc<Self>, server: &Server) -> Result<Vec<Arc<dyn Tool>>, String> {
        match self.attach(server).await {
            Ok(tools) => Ok(tools),
            Err(Failure::Unauthorized(challenge)) => self.sign_in(server, &challenge).await,
            Err(Failure::Other(reason)) => Err(reason),
        }
    }

    /// Connects to `server` and returns its tools, with the resources tool when it is the first server that offers resources.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, wants a token, or cannot list its tools.
    async fn attach(&self, server: &Server) -> Result<Vec<Arc<dyn Tool>>, Failure> {
        let client = Arc::new(Client::connect(server, self.bearer(server)).await?);
        let offered = if client.offers_tools() {
            match client.tools().await {
                Ok(offered) => offered,
                Err(_) if client.offers_resources() => Vec::new(),
                Err(reason) => return Err(Failure::Other(reason)),
            }
        } else {
            Vec::new()
        };
        let mut tools: Vec<Arc<dyn Tool>> = offered
            .into_iter()
            .map(|tool| {
                Arc::new(McpTool::new(
                    &server.name,
                    tool,
                    client.clone(),
                    self.spill.clone(),
                )) as Arc<dyn Tool>
            })
            .collect();
        if client.offers_resources() {
            self.offering
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .push(client.clone());
            if !self.resources_given.swap(true, Ordering::Relaxed) {
                tools.push(Arc::new(Resources::new(
                    self.offering.clone(),
                    self.spill.clone(),
                )));
            }
        }
        Ok(tools)
    }

    /// Starts signing in to `server`, which answered with `challenge`: tells the listener the page to open, and waits in the background for the reader to finish, then connects to the server and keeps its tools for [`Hub::arrived`].
    ///
    /// # Errors
    ///
    /// Always fails, with a note of the sign-in waited for, or with the reason the sign-in could not start.
    async fn sign_in(
        self: &Arc<Self>,
        server: &Server,
        challenge: &Challenge,
    ) -> Result<Vec<Arc<dyn Tool>>, String> {
        let (Some(tokens), Reach::Http { url, .. }) = (&self.tokens, &server.reach) else {
            return Err("the tool server wants signing in, and sign-ins cannot be kept".to_owned());
        };
        let http = reqwest::Client::new();
        let started = oauth::start(&http, url, challenge, server.oauth.as_ref())
            .await
            .map_err(|reason| format!("cannot sign in: {reason}"))?;
        (self.listener)(Notice::SignIn {
            server: server.name.clone(),
            url: started.url.clone(),
        });
        let hub: Weak<Self> = Arc::downgrade(self);
        let tokens = tokens.clone();
        let server = server.clone();
        let key = oauth::resource_of(url);
        let task = tokio::spawn(async move {
            let finished = started.waiting.finish(&http, SIGN_IN_PATIENCE).await;
            let Some(hub) = hub.upgrade() else {
                return;
            };
            let name = server.name.clone();
            let outcome = match finished {
                Ok(grant) => match tokens.set(&key, grant) {
                    Ok(()) => hub.attach(&server).await.map_err(Failure::into_reason),
                    Err(error) => Err(format!("cannot keep the sign-in: {error}")),
                },
                Err(reason) => Err(reason),
            };
            match outcome {
                Ok(tools) => {
                    hub.arrived
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .extend(tools);
                    (hub.listener)(Notice::SignedIn { server: name });
                }
                Err(reason) => (hub.listener)(Notice::Failed {
                    server: name,
                    reason,
                }),
            }
        });
        self.signing
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .push(task.abort_handle());
        Err("waiting for the reader to sign in".to_owned())
    }
}

/// The tools of `servers`, each as a tool of the agent saving output too long to show in `spill`, and the reasons the servers that could not be reached give. Nothing is signed in to.
pub async fn tools(servers: &[ToolServer], spill: &Spill) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let hub = Hub::new(spill.clone(), None, Arc::new(|_| {}));
    let servers: Vec<Server> = servers.iter().map(Server::from_client).collect();
    let reached = hub.connect(&servers).await;
    (reached.tools, reached.failed)
}

/// The result of a JSON-RPC answer: its `result`, or its error message.
///
/// # Errors
///
/// Fails with the error message when the answer is an error.
pub(crate) fn answer(message: &Value) -> Result<Value, String> {
    match message.get("error") {
        Some(error) => Err(error["message"].as_str().unwrap_or("failed").to_owned()),
        None => Ok(message["result"].clone()),
    }
}
