//! One connection to a tool server: its tools and calling them.

use std::sync::atomic::{AtomicU64, Ordering};

use omc_wire::{Reach, ToolServer};
use serde_json::{Value, json};

use crate::http::Http;
use crate::stdio::Program;

/// The protocol version spoken.
const VERSION: &str = "2025-06-18";

/// How a server is reached once connected.
enum Link {
    /// A program on its standard streams.
    Program(Box<Program>),
    /// An HTTP address.
    Address(Http),
}

/// A tool server the handshake is done with.
pub(crate) struct Client {
    /// The way to it.
    link: Link,
    /// The id the next request goes under.
    next: AtomicU64,
}

/// One tool a server offers.
pub(crate) struct Offered {
    /// What the server calls it.
    pub(crate) name: String,
    /// What it does.
    pub(crate) description: String,
    /// The schema of its arguments.
    pub(crate) schema: Value,
}

impl Client {
    /// Connects to `server` and does the handshake.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot be reached or does not answer the handshake.
    pub(crate) async fn connect(server: &ToolServer) -> Result<Self, String> {
        let link = match &server.reach {
            reach @ Reach::Command { .. } => Link::Program(Box::new(Program::start(reach)?)),
            reach @ Reach::Http { .. } => Link::Address(Http::new(reach)?),
        };
        let client = Self {
            link,
            next: AtomicU64::new(1),
        };
        client
            .request(
                "initialize",
                json!({
                    "protocolVersion": VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": "crab", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        let initialized = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        match &client.link {
            Link::Program(program) => program.send(&initialized).await?,
            Link::Address(address) => address.notify(&initialized).await?,
        }
        Ok(client)
    }

    /// Sends the request `method` and returns its result.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server stops, cannot be reached or answers with an error.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        match &self.link {
            Link::Program(program) => program.request(id, method, params).await,
            Link::Address(address) => address.request(id, method, params).await,
        }
    }

    /// The tools the server offers.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot list them.
    pub(crate) async fn tools(&self) -> Result<Vec<Offered>, String> {
        let result = self.request("tools/list", json!({})).await?;
        Ok(result["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| {
                Some(Offered {
                    name: tool["name"].as_str()?.to_owned(),
                    description: tool["description"].as_str().unwrap_or_default().to_owned(),
                    schema: tool["inputSchema"].clone(),
                })
            })
            .collect())
    }

    /// Calls the tool `name` with `arguments` and returns what it printed.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the call fails or the tool reports an error.
    pub(crate) async fn call(&self, name: &str, arguments: Value) -> Result<String, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let text = result["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if result["isError"] == json!(true) {
            Err(text)
        } else {
            Ok(text)
        }
    }
}
