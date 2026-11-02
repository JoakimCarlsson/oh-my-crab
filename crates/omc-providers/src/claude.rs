//! Streaming messages against the Anthropic Messages API with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::json;

use crate::{ProviderError, sse};

/// Messages endpoint.
const URL: &str = "https://api.anthropic.com/v1/messages";

/// Model used for replies.
const MODEL: &str = "claude-haiku-5-5";

/// System prompt that Claude subscription tokens require.
const SYSTEM: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

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
}

/// The delta carried by a `content_block_delta` event.
#[derive(Deserialize)]
struct Delta {
    /// Text appended to the reply, absent for non-text deltas.
    text: Option<String>,
}

/// The error carried by an `error` event.
#[derive(Deserialize)]
struct ErrorBody {
    /// Human-readable error message.
    message: String,
}

/// Sends `prompt` to Claude and calls `on_delta` with each text chunk.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an
/// error event inside the stream.
pub async fn send(
    credentials: &Credentials,
    prompt: &str,
    mut on_delta: impl FnMut(&str),
) -> Result<(), ProviderError> {
    let body = json!({
        "model": MODEL,
        "max_tokens": 1024,
        "stream": true,
        "system": [{ "type": "text", "text": SYSTEM }],
        "messages": [{ "role": "user", "content": prompt }],
    });
    let response = reqwest::Client::new()
        .post(URL)
        .bearer_auth(&credentials.access)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
        .header("anthropic-dangerous-direct-browser-access", "true")
        .header("x-app", "cli")
        .header("user-agent", "claude-cli/2.1.0 (external, cli)")
        .json(&body)
        .send()
        .await?;
    let mut events = sse::events(response).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        let Ok(payload) = serde_json::from_str::<Payload>(&event.data) else {
            continue;
        };
        match payload.kind.as_str() {
            "content_block_delta" => {
                if let Some(text) = payload.delta.and_then(|d| d.text) {
                    on_delta(&text);
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
    Ok(())
}
