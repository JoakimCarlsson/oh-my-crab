//! Streaming responses against the ChatGPT Codex backend with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::DeltaSink;
use crate::chat::parse_arguments;
use crate::prompt::PROMPT;
use crate::{Message, ProviderError, Reply, ToolCall, ToolSpec, sse};

/// Responses endpoint of the Codex backend.
const URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Model list endpoint of the Codex backend.
const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

/// Client version the model list is filtered for.
const CLIENT_VERSION: &str = "0.159.0";

/// One server-sent event payload from the Responses stream.
#[derive(Deserialize)]
struct Payload {
    /// Event kind, such as `response.output_text.delta` or `error`.
    #[serde(rename = "type")]
    kind: String,
    /// Text chunk for output-text delta events.
    delta: Option<String>,
    /// Error message for `error` events.
    message: Option<String>,
    /// Final response object for `response.failed` events.
    response: Option<FailedResponse>,
    /// The finished output item of `response.output_item.done` events.
    item: Option<Item>,
}

/// An output item of the response.
#[derive(Deserialize)]
struct Item {
    /// Item kind, such as `message` or `function_call`.
    #[serde(rename = "type")]
    kind: String,
    /// Call id of a function call.
    call_id: Option<String>,
    /// Tool name of a function call.
    name: Option<String>,
    /// Argument JSON of a function call.
    arguments: Option<String>,
}

/// The response object carried by a failed-response event.
#[derive(Deserialize)]
struct FailedResponse {
    /// Failure details, when the backend supplies them.
    error: Option<FailureBody>,
}

/// The failure details of a failed response.
#[derive(Deserialize)]
struct FailureBody {
    /// Human-readable error message.
    message: Option<String>,
}

/// The body of a model list response.
#[derive(Deserialize)]
struct ModelList {
    /// The listed models.
    models: Vec<ModelEntry>,
}

/// One entry of a model list response.
#[derive(Deserialize)]
struct ModelEntry {
    /// The model id to pass when sending.
    slug: String,
    /// Whether the backend lists the model; `hide` entries are skipped.
    visibility: Option<String>,
    /// Sort key; lower comes first.
    priority: Option<f64>,
}

/// Adds the headers the Codex backend requires to `request`.
fn authorized(
    request: reqwest::RequestBuilder,
    credentials: &Credentials,
) -> reqwest::RequestBuilder {
    let request = request
        .bearer_auth(&credentials.access)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "omc")
        .header("version", CLIENT_VERSION);
    match &credentials.account_id {
        Some(account) => request.header("chatgpt-account-id", account),
        None => request,
    }
}

/// Lists the model ids the account can use, in the backend's priority order.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub async fn list_models(credentials: &Credentials) -> Result<Vec<String>, ProviderError> {
    let url = format!("{MODELS_URL}?client_version={CLIENT_VERSION}");
    let request = reqwest::Client::new().get(url);
    let response = authorized(request, credentials).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::Status {
            status: status.to_string(),
            body,
        });
    }
    let list: ModelList = response.json().await?;
    let mut entries: Vec<ModelEntry> = list
        .models
        .into_iter()
        .filter(|entry| !matches!(entry.visibility.as_deref(), Some("hide" | "hidden")))
        .collect();
    entries.sort_by(|a, b| {
        let a = a.priority.unwrap_or(f64::MAX);
        let b = b.priority.unwrap_or(f64::MAX);
        a.total_cmp(&b)
    });
    Ok(entries.into_iter().map(|entry| entry.slug).collect())
}

/// Translates `messages` into the Responses API `input` array.
fn input_json(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message {
            Message::User(text) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            Message::Assistant(reply) => {
                if !reply.text.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "status": "completed",
                        "content": [{ "type": "output_text", "text": reply.text }],
                    }));
                }
                for call in &reply.tool_calls {
                    let arguments = if call.arguments.is_object() {
                        call.arguments.to_string()
                    } else {
                        "{}".to_string()
                    };
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": arguments,
                    }));
                }
            }
            Message::ToolResults(results) => {
                for result in results {
                    items.push(json!({
                        "type": "function_call_output",
                        "call_id": result.call_id,
                        "output": result.text,
                    }));
                }
            }
        }
    }
    items
}

/// Translates `tools` into the Responses API `tools` array.
fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "name": tool.name,
                "description": tool.description,
                "parameters": tool.schema,
                "strict": false,
            })
        })
        .collect()
}

/// Sends the conversation to Codex using `model` and returns its reply,
/// calling `on_delta` with each text chunk as it streams in.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an
/// error event inside the stream.
pub async fn complete(
    credentials: &Credentials,
    model: &str,
    messages: &[Message],
    tools: &[ToolSpec],
    on_delta: DeltaSink<'_>,
) -> Result<Reply, ProviderError> {
    let body = json!({
        "model": model,
        "instructions": PROMPT,
        "input": input_json(messages),
        "tools": tools_json(tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "stream": true,
        "store": false,
    });
    let request = authorized(reqwest::Client::new().post(URL), credentials)
        .header("accept", "text/event-stream");
    let response = request.json(&body).send().await?;
    let mut events = sse::events(response).await?;
    let mut text = String::new();
    let mut tool_calls = Vec::new();
    while let Some(event) = events.next().await {
        let event = event?;
        let Ok(payload) = serde_json::from_str::<Payload>(&event.data) else {
            continue;
        };
        match payload.kind.as_str() {
            "response.output_text.delta" => {
                if let Some(chunk) = payload.delta {
                    on_delta(&chunk);
                    text.push_str(&chunk);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = payload.item
                    && item.kind == "function_call"
                {
                    tool_calls.push(ToolCall {
                        id: item.call_id.unwrap_or_default(),
                        name: item.name.unwrap_or_default(),
                        arguments: parse_arguments(&item.arguments.unwrap_or_default()),
                    });
                }
            }
            "error" => {
                let message = payload.message.unwrap_or_else(|| event.data.clone());
                return Err(ProviderError::Stream(message));
            }
            "response.failed" => {
                let message = payload
                    .response
                    .and_then(|r| r.error)
                    .and_then(|e| e.message)
                    .unwrap_or_else(|| event.data.clone());
                return Err(ProviderError::Stream(message));
            }
            "response.completed" => break,
            _ => {}
        }
    }
    Ok(Reply { text, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool calls and results become `function_call` and `function_call_output` items.
    #[test]
    fn translates_tool_turns() {
        let call = ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: json!({ "path": "a" }),
        };
        let messages = vec![
            Message::Assistant(Reply {
                text: "looking".into(),
                tool_calls: vec![call],
            }),
            Message::ToolResults(vec![crate::ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: "ok".into(),
                is_error: false,
            }]),
        ];
        let items = input_json(&messages);
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(items[2]["call_id"], "c1");
    }

    /// Tool specs become function tools with a `parameters` schema.
    #[test]
    fn translates_tool_specs() {
        let spec = ToolSpec {
            name: "read".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        };
        let tool = &tools_json(&[spec])[0];
        assert_eq!(tool["type"], "function");
        assert_eq!(tool["parameters"]["type"], "object");
    }
}
