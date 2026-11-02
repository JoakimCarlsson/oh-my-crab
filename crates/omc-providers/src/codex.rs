//! Streaming responses against the ChatGPT Codex backend with an OAuth token.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::json;

use crate::prompt::PROMPT;
use crate::{ProviderError, sse};

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

/// Sends `prompt` to Codex using `model` and calls `on_delta` with each text chunk.
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
        "instructions": PROMPT,
        "input": [{
            "type": "message",
            "role": "user",
            "content": [{ "type": "input_text", "text": prompt }],
        }],
        "stream": true,
        "store": false,
    });
    let request = authorized(reqwest::Client::new().post(URL), credentials)
        .header("accept", "text/event-stream");
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
