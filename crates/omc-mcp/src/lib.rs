//! Tool servers as tools of the agent: connects to each server, lists its tools and calls them.

mod client;
mod http;
mod stdio;
mod tool;

use std::sync::Arc;
use std::time::Duration;

use omc_tools::{Spill, Tool};
use omc_wire::ToolServer;
use serde_json::Value;

use crate::client::Client;
use crate::tool::McpTool;

/// How long a server has to connect and list its tools.
const PATIENCE: Duration = Duration::from_secs(20);

/// The tools of `servers`, each as a tool of the agent saving output too long to show in `spill`, and the reasons the servers that could not be reached give.
pub async fn tools(servers: &[ToolServer], spill: &Spill) -> (Vec<Arc<dyn Tool>>, Vec<String>) {
    let attempts = servers.iter().map(|server| async move {
        match tokio::time::timeout(PATIENCE, connected(server, spill)).await {
            Ok(result) => result.map_err(|reason| format!("{}: {reason}", server.name)),
            Err(_) => Err(format!("{}: did not answer in time", server.name)),
        }
    });
    let mut found: Vec<Arc<dyn Tool>> = Vec::new();
    let mut failed = Vec::new();
    for attempt in futures_util::future::join_all(attempts).await {
        match attempt {
            Ok(tools) => found.extend(tools),
            Err(reason) => failed.push(reason),
        }
    }
    (found, failed)
}

/// Connects to `server` and takes its tools, which save output too long to show in `spill`.
///
/// # Errors
///
/// Fails with the reason when the server cannot be reached or listed.
async fn connected(server: &ToolServer, spill: &Spill) -> Result<Vec<Arc<dyn Tool>>, String> {
    let client = Arc::new(Client::connect(server).await?);
    let offered = client.tools().await?;
    Ok(offered
        .into_iter()
        .map(|tool| {
            Arc::new(McpTool::new(
                &server.name,
                tool,
                client.clone(),
                spill.clone(),
            )) as Arc<dyn Tool>
        })
        .collect())
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
