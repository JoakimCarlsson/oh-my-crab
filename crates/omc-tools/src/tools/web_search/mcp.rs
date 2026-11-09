//! Calling a search tool on a service's public MCP server, as Parallel and
//! Exa are searched without a key: one JSON-RPC `tools/call` request,
//! answered as JSON or as a server-sent event stream.

use serde_json::{Value, json};

use super::backend::{Call, text};

/// The POST to `url` calling the tool `tool` with `arguments`.
pub(super) fn call(url: reqwest::Url, tool: &str, arguments: Value) -> Call {
    Call::post_json(
        url,
        vec![("Accept", "application/json, text/event-stream".to_owned())],
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": tool, "arguments": arguments },
        }),
    )
}

/// The text parts of the result of the tool call answered with `body`, read
/// from the JSON-RPC response, sent alone or as the `data` of a server-sent
/// event.
///
/// # Errors
///
/// Returns the reason when the answer holds no JSON-RPC response, when the
/// response is an error, or when the tool reports one.
pub(super) fn texts(body: &[u8]) -> Result<Vec<String>, String> {
    let body = String::from_utf8_lossy(body);
    let response: Value = match serde_json::from_str(body.trim()) {
        Ok(value) => value,
        Err(_) => body
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|data| serde_json::from_str::<Value>(data.trim()).ok())
            .find(|message| message.get("result").is_some() || message.get("error").is_some())
            .ok_or("the answer holds no MCP response")?,
    };
    if let Some(error) = response.get("error") {
        return Err(format!(
            "the MCP server refused the search: {}",
            text(&error["message"]).unwrap_or_else(|| error.to_string())
        ));
    }
    let parts: Vec<String> = response["result"]["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|part| text(&part["text"]))
        .collect();
    if response["result"]["isError"] == json!(true) {
        return Err(format!("the search tool failed: {}", parts.join(" ")));
    }
    Ok(parts)
}
