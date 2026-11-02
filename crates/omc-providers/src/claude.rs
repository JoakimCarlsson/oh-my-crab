//! Streaming messages against the Anthropic Messages API with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::json;

use crate::prompt::PROMPT;
use crate::{ProviderError, sse};

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

/// Sends `prompt` to Claude using `model` and calls `on_delta` with each text chunk.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an
/// error event inside the stream.
pub async fn send(
    credentials: &Credentials,
    model: &str,
    prompt: &str,
    mut on_delta: impl FnMut(&str),
) -> Result<(), ProviderError> {
    let body = json!({
        "model": model,
        "max_tokens": 1024,
        "stream": true,
        "system": [
            { "type": "text", "text": IDENTITY },
            { "type": "text", "text": PROMPT },
        ],
        "messages": [{ "role": "user", "content": prompt }],
    });
    let response = authorized(reqwest::Client::new().post(URL), credentials)
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
