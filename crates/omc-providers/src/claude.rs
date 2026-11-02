//! Streaming messages against the Anthropic Messages API with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::chat::DeltaSink;
use crate::chat::parse_arguments;
use crate::prompt::PROMPT;
use crate::{Message, ProviderError, Reply, ToolCall, ToolSpec, sse};

/// Messages endpoint.
const URL: &str = "https://api.anthropic.com/v1/messages";

/// Model list endpoint.
const MODELS_URL: &str = "https://api.anthropic.com/v1/models";

/// Model used when the user has not picked one.
pub const DEFAULT_MODEL: &str = "claude-haiku-5-5";

/// First system block, which Claude subscription tokens require.
const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// One server-sent event payload from the Messages stream.
#[derive(Deserialize)]
struct Payload {
    /// Event kind, such as `content_block_delta` or `error`.
    #[serde(rename = "type")]
    kind: String,
    /// Incremental content for delta events.
    delta: Option<Delta>,
    /// Error details for error events.
    error: Option<ErrorBody>,
    /// Position of the content block the event belongs to.
    index: Option<usize>,
    /// The block opened by a `content_block_start` event.
    content_block: Option<Block>,
}

/// A content block announced by a `content_block_start` event.
#[derive(Deserialize)]
struct Block {
    /// Block kind, such as `text` or `tool_use`.
    #[serde(rename = "type")]
    kind: String,
    /// Tool call id for `tool_use` blocks.
    id: Option<String>,
    /// Tool name for `tool_use` blocks.
    name: Option<String>,
}

/// A tool call whose arguments are still streaming in.
struct PendingCall {
    /// Tool call id.
    id: String,
    /// Tool name.
    name: String,
    /// Argument JSON received so far.
    json: String,
}

/// The delta carried by a `content_block_delta` event.
#[derive(Deserialize)]
struct Delta {
    /// Text appended to the reply, absent for non-text deltas.
    text: Option<String>,
    /// Fragment of a tool call's argument JSON, absent for other deltas.
    partial_json: Option<String>,
}

/// The error carried by an `error` event.
#[derive(Deserialize)]
struct ErrorBody {
    /// Human-readable error message.
    message: String,
}

/// The body of a model list response.
#[derive(Deserialize)]
struct ModelList {
    /// The listed models.
    data: Vec<ModelEntry>,
}

/// One entry of a model list response.
#[derive(Deserialize)]
struct ModelEntry {
    /// The model id to pass when sending.
    id: String,
}

/// Adds the OAuth headers Claude subscription tokens require to `request`.
fn authorized(
    request: reqwest::RequestBuilder,
    credentials: &Credentials,
) -> reqwest::RequestBuilder {
    request
        .bearer_auth(&credentials.access)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("x-app", "cli")
        .header("user-agent", "claude-cli/2.1.0 (external, cli)")
}

/// Lists the model ids the account can use, newest first.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub async fn list_models(credentials: &Credentials) -> Result<Vec<String>, ProviderError> {
    let response = authorized(reqwest::Client::new().get(MODELS_URL), credentials)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::Status {
            status: status.to_string(),
            body,
        });
    }
    let list: ModelList = response.json().await?;
    Ok(list.data.into_iter().map(|entry| entry.id).collect())
}

/// Translates `messages` into the Messages API `messages` array.
fn messages_json(messages: &[Message]) -> Vec<Value> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(text) => Some(json!({ "role": "user", "content": text })),
            Message::Assistant(reply) => {
                let mut blocks = Vec::new();
                if !reply.text.is_empty() {
                    blocks.push(json!({ "type": "text", "text": reply.text }));
                }
                for call in &reply.tool_calls {
                    let input = if call.arguments.is_object() {
                        call.arguments.clone()
                    } else {
                        json!({})
                    };
                    blocks.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input,
                    }));
                }
                (!blocks.is_empty()).then(|| json!({ "role": "assistant", "content": blocks }))
            }
            Message::ToolResults(results) => {
                let blocks: Vec<Value> = results
                    .iter()
                    .map(|result| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": result.call_id,
                            "content": result.text,
                            "is_error": result.is_error,
                        })
                    })
                    .collect();
                Some(json!({ "role": "user", "content": blocks }))
            }
        })
        .collect()
}

/// Translates `tools` into the Messages API `tools` array.
fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "input_schema": tool.schema,
            })
        })
        .collect()
}

/// Sends the conversation to Claude using `model` and returns its reply,
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
    let mut body = json!({
        "model": model,
        "max_tokens": 8192,
        "stream": true,
        "system": [
            { "type": "text", "text": IDENTITY },
            { "type": "text", "text": PROMPT },
        ],
        "messages": messages_json(messages),
    });
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools_json(tools));
    }
    let response = authorized(reqwest::Client::new().post(URL), credentials)
        .json(&body)
        .send()
        .await?;
    let mut events = sse::events(response).await?;
    let mut text = String::new();
    let mut pending: BTreeMap<usize, PendingCall> = BTreeMap::new();
    while let Some(event) = events.next().await {
        let event = event?;
        let Ok(payload) = serde_json::from_str::<Payload>(&event.data) else {
            continue;
        };
        match payload.kind.as_str() {
            "content_block_start" => {
                let block = payload.content_block;
                if let (Some(index), Some(block)) = (payload.index, block)
                    && block.kind == "tool_use"
                {
                    pending.insert(
                        index,
                        PendingCall {
                            id: block.id.unwrap_or_default(),
                            name: block.name.unwrap_or_default(),
                            json: String::new(),
                        },
                    );
                }
            }
            "content_block_delta" => {
                let Some(delta) = payload.delta else { continue };
                if let Some(chunk) = delta.text {
                    on_delta(&chunk);
                    text.push_str(&chunk);
                }
                if let (Some(fragment), Some(call)) = (
                    delta.partial_json,
                    payload.index.and_then(|i| pending.get_mut(&i)),
                ) {
                    call.json.push_str(&fragment);
                }
            }
            "error" => {
                let message = payload
                    .error
                    .map_or_else(|| event.data.clone(), |e| e.message);
                return Err(ProviderError::Stream(message));
            }
            "message_stop" => break,
            _ => {}
        }
    }
    let tool_calls = pending
        .into_values()
        .map(|call| ToolCall {
            arguments: parse_arguments(&call.json),
            id: call.id,
            name: call.name,
        })
        .collect();
    Ok(Reply { text, tool_calls })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tool calls and results become `tool_use` and `tool_result` blocks.
    #[test]
    fn translates_tool_turns() {
        let call = ToolCall {
            id: "t1".into(),
            name: "read".into(),
            arguments: json!({ "path": "a" }),
        };
        let messages = vec![
            Message::User("hi".into()),
            Message::Assistant(Reply {
                text: String::new(),
                tool_calls: vec![call],
            }),
            Message::ToolResults(vec![crate::ToolResult {
                call_id: "t1".into(),
                name: "read".into(),
                text: "ok".into(),
                is_error: false,
            }]),
        ];
        let out = messages_json(&messages);
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
        assert_eq!(out[1]["content"][0]["input"]["path"], "a");
        assert_eq!(out[2]["content"][0]["tool_use_id"], "t1");
    }

    /// Tool specs use the `input_schema` field.
    #[test]
    fn translates_tool_specs() {
        let spec = ToolSpec {
            name: "read".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        };
        assert_eq!(tools_json(&[spec])[0]["input_schema"]["type"], "object");
    }
}
