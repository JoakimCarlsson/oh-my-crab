//! One connection to a tool server: the handshake, its tools and resources, and starting a program again that stopped.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};
use tokio::sync::Mutex;

use crate::Failure;
use crate::config::{Server, Transport};
use crate::http::{Bearer, Http, Sse};
use crate::stdio::{Program, STOPPED};
use omc_wire::Reach;

/// The protocol version spoken.
pub(crate) const VERSION: &str = "2025-06-18";

/// The most pages of a list that are read.
const PAGES: usize = 20;

/// How a server is reached once connected.
enum Link {
    /// A program on its standard streams, while it runs; it is started again by the request after it stopped.
    Program(Mutex<Option<Arc<Program>>>),
    /// An HTTP address that answers POST requests.
    Address(Http),
    /// An HTTP address of the older transport, with its event stream.
    Stream(Sse),
}

/// A tool server the handshake is done with.
pub(crate) struct Client {
    /// The server, for starting its program again.
    server: Server,
    /// The way to it.
    link: Link,
    /// The id the next request goes under.
    next: AtomicU64,
    /// What the server said it offers, in its answer to the handshake.
    capabilities: Value,
}

/// One tool a server offers.
pub(crate) struct Offered {
    /// What the server calls it.
    pub(crate) name: String,
    /// What it does.
    pub(crate) description: String,
    /// The schema of its arguments.
    pub(crate) schema: Value,
    /// Whether the server says the tool changes nothing, with the `readOnlyHint` annotation.
    pub(crate) read_only_hint: bool,
}

/// The parameters of the handshake.
fn introduction() -> Value {
    json!({
        "protocolVersion": VERSION,
        "capabilities": {},
        "clientInfo": { "name": "crab", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The notification that ends the handshake.
fn initialized() -> Value {
    json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })
}

/// Does the handshake with `program`, the request going under `id`, and returns what the program offers.
///
/// # Errors
///
/// Fails with the reason when the program stops or answers with an error.
async fn introduce(program: &Program, id: u64) -> Result<Value, String> {
    let answer = program.request(id, "initialize", introduction()).await?;
    program.send(&initialized()).await?;
    Ok(answer["capabilities"].clone())
}

impl Client {
    /// Connects to `server` and does the handshake, signing in to an address with `bearer` when it wants a token.
    ///
    /// # Errors
    ///
    /// Fails with [`Failure::Unauthorized`] when an address wants a token crab does not have, or with the reason when the server cannot be reached or does not answer the handshake.
    pub(crate) async fn connect(
        server: &Server,
        bearer: Option<Arc<Bearer>>,
    ) -> Result<Self, Failure> {
        let next = AtomicU64::new(1);
        let id = || next.fetch_add(1, Ordering::Relaxed);
        let (link, capabilities) = match (&server.reach, server.transport) {
            (reach @ Reach::Command { .. }, _) => {
                let program =
                    Program::start(reach, server.folder.as_deref()).map_err(Failure::Other)?;
                let capabilities = introduce(&program, id()).await.map_err(Failure::Other)?;
                (
                    Link::Program(Mutex::new(Some(Arc::new(program)))),
                    capabilities,
                )
            }
            (reach @ Reach::Http { .. }, Transport::Streamable) => {
                let address = Http::new(reach, bearer).map_err(Failure::Other)?;
                let answer = address.request(id(), "initialize", introduction()).await?;
                address.notify(&initialized()).await?;
                (Link::Address(address), answer["capabilities"].clone())
            }
            (reach @ Reach::Http { .. }, Transport::Sse) => {
                let stream = Sse::connect(reach, bearer).await?;
                let answer = stream.request(id(), "initialize", introduction()).await?;
                stream.notify(&initialized()).await?;
                (Link::Stream(stream), answer["capabilities"].clone())
            }
        };
        Ok(Self {
            server: server.clone(),
            link,
            next,
            capabilities,
        })
    }

    /// The name of the server.
    pub(crate) fn name(&self) -> &str {
        &self.server.name
    }

    /// Whether the server offers resources.
    pub(crate) fn offers_resources(&self) -> bool {
        self.capabilities.get("resources").is_some()
    }

    /// Whether the server may offer tools: it says so, or says nothing of what it offers.
    pub(crate) fn offers_tools(&self) -> bool {
        self.capabilities.get("tools").is_some() || !self.capabilities.is_object()
    }

    /// The id the next request goes under.
    fn id(&self) -> u64 {
        self.next.fetch_add(1, Ordering::Relaxed)
    }

    /// Sends the request `method` and returns its result. A program found stopped is started again first, with a new handshake; a request it stops during fails, and is not sent again, since part of it may have been done.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server stops, cannot be reached or started again, wants signing in again, or answers with an error.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        match &self.link {
            Link::Program(running) => {
                let program = self.running(running).await?;
                program
                    .request(self.id(), method, params)
                    .await
                    .map_err(|reason| {
                        if program.stopped() {
                            format!("{reason}; it is started again for the next call")
                        } else {
                            reason
                        }
                    })
            }
            Link::Address(address) => address
                .request(self.id(), method, params)
                .await
                .map_err(Failure::into_reason),
            Link::Stream(stream) => stream
                .request(self.id(), method, params)
                .await
                .map_err(Failure::into_reason),
        }
    }

    /// The program of `running`, started again with a new handshake when it stopped.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the program cannot be started again or does not answer the handshake.
    async fn running(&self, running: &Mutex<Option<Arc<Program>>>) -> Result<Arc<Program>, String> {
        let mut held = running.lock().await;
        if let Some(program) = held.as_ref().filter(|program| !program.stopped()) {
            return Ok(program.clone());
        }
        *held = None;
        let program = Program::start(&self.server.reach, self.server.folder.as_deref())
            .map_err(|reason| format!("{STOPPED}, and starting it again failed: {reason}"))?;
        introduce(&program, self.id())
            .await
            .map_err(|reason| format!("{STOPPED}, and starting it again failed: {reason}"))?;
        let program = Arc::new(program);
        *held = Some(program.clone());
        Ok(program)
    }

    /// Every item under `key` of the pages of the list `method` returns, following its cursors.
    ///
    /// # Errors
    ///
    /// Fails with the reason when a page cannot be read.
    async fn list(&self, method: &str, key: &str) -> Result<Vec<Value>, String> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..PAGES {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request(method, params).await?;
            items.extend(page[key].as_array().into_iter().flatten().cloned());
            cursor = page["nextCursor"].as_str().map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(items)
    }

    /// The tools the server offers.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot list them.
    pub(crate) async fn tools(&self) -> Result<Vec<Offered>, String> {
        Ok(self
            .list("tools/list", "tools")
            .await?
            .into_iter()
            .filter_map(|tool| {
                Some(Offered {
                    name: tool["name"].as_str()?.to_owned(),
                    description: tool["description"].as_str().unwrap_or_default().to_owned(),
                    schema: tool["inputSchema"].clone(),
                    read_only_hint: tool["annotations"]["readOnlyHint"].as_bool() == Some(true),
                })
            })
            .collect())
    }

    /// Calls the tool `name` with `arguments` and returns what it printed, with the image parts of the answer as `(base64 data, media type)`.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the call fails or the tool reports an error.
    pub(crate) async fn call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<(String, Vec<(String, String)>), String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let parts = result["content"].as_array().cloned().unwrap_or_default();
        let text = parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result["isError"] == json!(true) {
            return Err(text);
        }
        let images = parts
            .iter()
            .filter(|part| part["type"] == "image")
            .filter_map(|part| {
                Some((
                    part["data"].as_str()?.to_owned(),
                    part["mimeType"].as_str().unwrap_or("image/png").to_owned(),
                ))
            })
            .collect();
        Ok((text, images))
    }

    /// The resources the server lists.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot list them.
    pub(crate) async fn resources(&self) -> Result<Vec<Value>, String> {
        self.list("resources/list", "resources").await
    }

    /// The resource templates the server lists.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot list them.
    pub(crate) async fn templates(&self) -> Result<Vec<Value>, String> {
        self.list("resources/templates/list", "resourceTemplates")
            .await
    }

    /// The contents of the resource at `uri`.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot read it.
    pub(crate) async fn read(&self, uri: &str) -> Result<Vec<Value>, String> {
        let result = self
            .request("resources/read", json!({ "uri": uri }))
            .await?;
        Ok(result["contents"].as_array().cloned().unwrap_or_default())
    }
}
