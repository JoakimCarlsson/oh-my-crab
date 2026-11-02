//! Streaming responses against the ChatGPT Codex backend with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::json;

use crate::{ProviderError, sse};

/// Responses endpoint of the Codex backend.
const URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Model used for replies.
const MODEL: &str = "gpt-5.5";

/// Instructions sent with every request.
const INSTRUCTIONS: &str = "You are a friendly assistant. Keep replies short.";

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

/// Sends `prompt` to Codex and calls `on_delta` with each text chunk.
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
        "instructions": INSTRUCTIONS,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt }],
        }],
        "stream": true,
        "store": false,
    });
    let mut request = reqwest::Client::new()
        .post(URL)
        .bearer_auth(&credentials.access)
        .header("OpenAI-Beta", "responses=experimental")
        .header("originator", "omc")
        .header("accept", "text/event-stream");
    if let Some(account) = &credentials.account_id {
        request = request.header("chatgpt-account-id", account);
    }
    let response = request.json(&body).send().await?;
    let mut events = sse::events(response).await?;
    while let Some(event) = events.next().await {
        let event = event?;
        let Ok(payload) = serde_json::from_str::<Payload>(&event.data) else {
            continue;
        };
        match payload.kind.as_str() {
            "response.output_text.delta" => {
                if let Some(text) = payload.delta {
                    on_delta(&text);
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
    Ok(())
}
