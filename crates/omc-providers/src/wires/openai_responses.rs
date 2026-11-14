//! Streaming responses over the OpenAI Responses API: the ChatGPT Codex backend
//! with an OAuth token, the OpenAI API and OpenAI-compatible servers with a
//! bearer key, and Azure OpenAI with its `api-key` header.

use base64::Engine;
use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::{Blocks, parse_arguments};
use crate::identity;
use crate::models::codex_efforts;
use crate::wires::chat_completions;
use crate::{
    Effort, Message, Part, Provider, ProviderError, Reply, Request, Sink, Stop, Tokens, ToolCall,
    ToolSpec, sse,
};

/// Responses endpoint of the Codex backend.
const URL: &str = "https://chatgpt.com/backend-api/codex/responses";

/// Model list endpoint of the Codex backend.
const MODELS_URL: &str = "https://chatgpt.com/backend-api/codex/models";

/// Client version the model list is filtered for.
const CLIENT_VERSION: &str = "0.159.0";

/// Claim of the access token that holds the ChatGPT account details.
const JWT_CLAIM: &str = "https://api.openai.com/auth";

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

/// Reads the workspace residency the access token declares, when it is region-pinned.
fn residency(token: &str) -> Option<String> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: Value = serde_json::from_slice(&bytes).ok()?;
    let auth = claims.get(JWT_CLAIM)?;
    for key in ["chatgpt_data_residency", "chatgpt_compute_residency"] {
        let Some(claim) = auth.get(key).and_then(Value::as_str) else {
            continue;
        };
        let claim = claim.trim();
        if claim.is_empty() {
            continue;
        }
        return (!claim.eq_ignore_ascii_case("no_constraint")).then(|| claim.to_string());
    }
    None
}

/// The headers of a Codex request for `model`, as name and value pairs; `model` is empty for non-chat calls.
pub(crate) fn headers(credentials: &Credentials, model: &str) -> Vec<(String, String)> {
    let session = identity::session_id().to_string();
    let mut headers = vec![
        (
            "Authorization".to_string(),
            format!("Bearer {}", credentials.access),
        ),
        (
            "OpenAI-Beta".to_string(),
            "responses=experimental".to_string(),
        ),
        ("originator".to_string(), "omc".to_string()),
        ("version".to_string(), CLIENT_VERSION.to_string()),
        (
            "User-Agent".to_string(),
            format!("omc/{}", env!("CARGO_PKG_VERSION")),
        ),
        ("conversation_id".to_string(), session.clone()),
        ("session_id".to_string(), session.clone()),
        ("x-client-request-id".to_string(), session),
    ];
    if !model.is_empty() {
        headers.push(("x-codex-routing-hint".to_string(), format!("model={model}")));
    }
    if let Some(account) = &credentials.account_id {
        headers.push(("chatgpt-account-id".to_string(), account.clone()));
    }
    if let Some(region) = residency(&credentials.access) {
        headers.push(("x-openai-internal-codex-residency".to_string(), region));
    }
    headers
}

/// Adds the headers the Codex backend requires to `request`.
fn authorized(
    mut request: reqwest::RequestBuilder,
    credentials: &Credentials,
    model: &str,
) -> reqwest::RequestBuilder {
    for (name, value) in headers(credentials, model) {
        request = request.header(name, value);
    }
    request
}

/// Lists the model ids the Codex account can use, in the backend's priority order.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
async fn list_codex_models(credentials: &Credentials) -> Result<Vec<String>, ProviderError> {
    let url = format!("{MODELS_URL}?client_version={CLIENT_VERSION}");
    let request = reqwest::Client::new().get(url);
    let response = authorized(request, credentials, "").send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::from_response(response).await);
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

/// Whether `item` is a Codex reasoning item.
fn is_reasoning(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
}

/// Translates `messages` into the Responses API `input` array.
///
/// Reasoning items kept in a reply's `opaque` go back, without their ids,
/// ahead of the output they accompanied; items that are not Codex reasoning
/// are dropped. A compaction is left out: the agent gives the model its
/// summary as a user message in its place. So is a pruning, which the agent
/// applies to the tool results it gives.
pub(crate) fn input_json(messages: &[Message]) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        match message {
            Message::User(text) => items.push(json!({
                "type": "message",
                "role": "user",
                "content": [{ "type": "input_text", "text": text }],
            })),
            Message::Assistant(reply) => {
                for item in reply.opaque.iter().filter(|item| is_reasoning(item)) {
                    let mut item = item.clone();
                    if let Some(object) = item.as_object_mut() {
                        object.remove("id");
                    }
                    items.push(item);
                }
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
                        "output": result_output(result),
                    }));
                }
            }
            Message::Images(images) if !images.is_empty() => items.push(json!({
                "type": "message",
                "role": "user",
                "content": images
                    .iter()
                    .map(|image| json!({ "type": "input_image", "image_url": image.data_url() }))
                    .collect::<Vec<_>>(),
            })),
            Message::Compaction(_) | Message::Pruning(_) | Message::Images(_) => {}
        }
    }
    items
}

/// The `output` of the `function_call_output` item for `result`: its text
/// alone, or an `input_text` part followed by one `input_image` part per image
/// when it carries images.
fn result_output(result: &crate::ToolResult) -> Value {
    if result.images.is_empty() {
        return Value::String(result.text.clone());
    }
    let text =
        (!result.text.is_empty()).then(|| json!({ "type": "input_text", "text": result.text }));
    let images = result
        .images
        .iter()
        .map(|image| json!({ "type": "input_image", "image_url": image.data_url() }));
    Value::Array(text.into_iter().chain(images).collect())
}

/// Translates `tools` into the Responses API `tools` array.
pub(crate) fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
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

/// Picks the effort among `levels` that is closest to `wanted` without going over, or the lightest when all are heavier.
fn clamp_effort(levels: &[Effort], wanted: Effort) -> Option<Effort> {
    let rank = |effort: &Effort| Effort::ALL.iter().position(|e| e == effort);
    if levels.contains(&wanted) {
        return Some(wanted);
    }
    levels
        .iter()
        .rev()
        .find(|level| rank(level) <= rank(&wanted))
        .or(levels.first())
        .copied()
}

/// The `reasoning` field of a request at `effort` to a model that serves `levels`, when it asks for reasoning; the summary comes back as `auto` picks it.
pub(crate) fn reasoning_param(levels: &[Effort], effort: Option<Effort>) -> Option<Value> {
    let effort = clamp_effort(levels, effort?)?;
    Some(json!({ "effort": effort.as_str(), "summary": "auto" }))
}

/// Whether `model`, outside the Codex backend, is one that reasons and takes a `reasoning` field: the fifth GPT generation on, the `o` series, Codex models and the open-weight GPT models. An Azure deployment or a server's model is judged by its name, whatever it is prefixed with.
fn reasons(model: &str) -> bool {
    let id = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    let o_series = id
        .strip_prefix('o')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    o_series
        || id.contains("gpt-5")
        || id.contains("gpt-6")
        || id.contains("gpt-oss")
        || id.contains("codex")
}

/// The effort levels `model` takes over the Responses API outside the Codex backend, lightest first; none when it does not reason.
///
/// The `o` series and the open-weight GPT models take low, medium and high; the GPT generations take what Codex offers for the same id.
pub(crate) fn api_efforts(model: &str) -> Vec<Effort> {
    if !reasons(model) {
        return Vec::new();
    }
    let id = model
        .rsplit('/')
        .next()
        .unwrap_or(model)
        .to_ascii_lowercase();
    if id.starts_with('o') || id.contains("gpt-oss") {
        return vec![Effort::Low, Effort::Medium, Effort::High];
    }
    codex_efforts(&id)
}

/// The effort levels `model` of `provider` takes, lightest first, as the host it is sent to serves them.
pub(crate) fn efforts(provider: Provider, model: &str) -> Vec<Effort> {
    match provider {
        Provider::Codex => codex_efforts(model),
        _ => api_efforts(model),
    }
}

/// Where a Responses request goes and how it is signed, which also decides what its body may hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Host {
    /// The ChatGPT Codex backend, signed in with ChatGPT OAuth.
    Codex,
    /// The OpenAI API or an OpenAI-compatible server, sent the key as a bearer token.
    Api {
        /// The API address `/responses` and `/models` are appended to, without a trailing slash.
        base_url: String,
        /// Headers sent on every request.
        headers: &'static [(&'static str, &'static str)],
        /// Whether it is a server the user declared, which may not know the OpenAI API's caching and encrypted reasoning, and so is not sent them unless it reasons.
        server: bool,
    },
    /// Azure OpenAI, sent the key in `api-key` and the API version in the query.
    Azure {
        /// The address of the resource's OpenAI API, such as `https://name.openai.azure.com/openai/v1`.
        base_url: String,
        /// The `api-version` every request names.
        api_version: String,
    },
}

/// The variable that names the whole Azure OpenAI API address, such as `https://name.openai.azure.com/openai/v1`.
pub(crate) const AZURE_BASE_URL_ENV: &str = "AZURE_OPENAI_BASE_URL";

/// The variable that names the Azure OpenAI resource endpoint, such as `https://name.openai.azure.com`, which `/openai/v1` is appended to.
pub(crate) const AZURE_ENDPOINT_ENV: &str = "AZURE_OPENAI_ENDPOINT";

/// The variable that names the Azure OpenAI resource, which the address is made from.
pub(crate) const AZURE_RESOURCE_ENV: &str = "AZURE_OPENAI_RESOURCE_NAME";

/// The variable that names the Azure OpenAI API version.
pub(crate) const AZURE_API_VERSION_ENV: &str = "AZURE_OPENAI_API_VERSION";

/// The variable that names the Azure OpenAI deployments to offer as models, separated by commas, the first the default.
pub(crate) const AZURE_DEPLOYMENT_ENV: &str = "AZURE_OPENAI_DEPLOYMENT";

/// The API version Azure OpenAI is asked for when none is set: the version-less v1 API.
const AZURE_DEFAULT_API_VERSION: &str = "v1";

/// Reads the Azure OpenAI address and API version through `lookup`, which gives a variable's value when it is set and not blank.
///
/// The address is `AZURE_OPENAI_BASE_URL` as it is, else `AZURE_OPENAI_ENDPOINT` with `/openai/v1` appended unless it already names a path under `/openai`, else the one `AZURE_OPENAI_RESOURCE_NAME` names.
///
/// # Errors
///
/// Returns [`ProviderError::Credentials`] naming the variables to set when none of them is.
pub(crate) fn azure_host(lookup: impl Fn(&str) -> Option<String>) -> Result<Host, ProviderError> {
    let base_url = if let Some(base) = lookup(AZURE_BASE_URL_ENV) {
        base
    } else if let Some(endpoint) = lookup(AZURE_ENDPOINT_ENV) {
        let endpoint = endpoint.trim_end_matches('/').to_owned();
        if endpoint.contains("/openai") {
            endpoint
        } else {
            format!("{endpoint}/openai/v1")
        }
    } else if let Some(resource) = lookup(AZURE_RESOURCE_ENV) {
        format!("https://{resource}.openai.azure.com/openai/v1")
    } else {
        return Err(ProviderError::Credentials(format!(
            "Azure OpenAI needs an address; set {AZURE_ENDPOINT_ENV}, {AZURE_RESOURCE_ENV} or {AZURE_BASE_URL_ENV}"
        )));
    };
    Ok(Host::Azure {
        base_url: base_url.trim_end_matches('/').to_owned(),
        api_version: lookup(AZURE_API_VERSION_ENV)
            .unwrap_or_else(|| AZURE_DEFAULT_API_VERSION.to_owned()),
    })
}

/// The value of the environment variable `name`, trimmed, when it is set and not blank.
fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

impl Host {
    /// The host requests of `provider`, signed in with `credentials`, go to.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Credentials`] when Azure OpenAI's address is not set.
    pub(crate) fn of(provider: Provider, credentials: &Credentials) -> Result<Host, ProviderError> {
        match provider {
            Provider::Codex => Ok(Host::Codex),
            Provider::Azure => azure_host(env_value),
            _ => Ok(Host::Api {
                base_url: chat_completions::base_url(provider, credentials),
                headers: provider.def().headers,
                server: matches!(provider, Provider::Custom(_)),
            }),
        }
    }

    /// The address responses are asked of.
    pub(crate) fn responses_url(&self) -> String {
        match self {
            Host::Codex => URL.to_owned(),
            Host::Api { base_url, .. } => format!("{base_url}/responses"),
            Host::Azure {
                base_url,
                api_version,
            } => format!("{base_url}/responses?api-version={api_version}"),
        }
    }

    /// The address models are listed at.
    pub(crate) fn models_url(&self) -> String {
        match self {
            Host::Codex => format!("{MODELS_URL}?client_version={CLIENT_VERSION}"),
            Host::Api { base_url, .. } => format!("{base_url}/models"),
            Host::Azure {
                base_url,
                api_version,
            } => format!("{base_url}/models?api-version={api_version}"),
        }
    }

    /// Adds the headers that sign `request` for `model` with `credentials`; `model` is empty for calls that are not chats.
    fn sign(
        &self,
        request: reqwest::RequestBuilder,
        credentials: &Credentials,
        model: &str,
    ) -> reqwest::RequestBuilder {
        match self {
            Host::Codex => authorized(request, credentials, model),
            Host::Api {
                base_url, headers, ..
            } => chat_completions::authorized(
                request,
                &chat_completions::Target {
                    credentials,
                    base_url,
                    headers,
                    replay_reasoning: false,
                },
            ),
            Host::Azure { .. } if credentials.access.is_empty() => request,
            Host::Azure { .. } => request.header("api-key", &credentials.access),
        }
    }

    /// The effort levels `model` takes on this host, lightest first.
    fn efforts(&self, model: &str) -> Vec<Effort> {
        match self {
            Host::Codex => codex_efforts(model),
            _ => api_efforts(model),
        }
    }
}

/// Builds the request body for `model` on `host`.
///
/// Every host is asked not to store the response and to stream it. Encrypted
/// reasoning, which carries the model's reasoning to later requests without
/// storing it, and the prompt cache key are asked of every host but a server
/// the user declared, which is asked for encrypted reasoning only when it is
/// sent a `reasoning` field.
pub(crate) fn build_body(host: &Host, model: &str, request: &Request<'_>) -> Value {
    let mut body = json!({
        "model": model,
        "instructions": request.system,
        "input": input_json(request.messages),
        "tools": tools_json(request.tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "stream": true,
        "store": false,
    });
    let reasoning = reasoning_param(&host.efforts(model), request.effort);
    let server = matches!(host, Host::Api { server: true, .. });
    if !server || reasoning.is_some() {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }
    if !server {
        body["prompt_cache_key"] = json!(identity::session_id());
    }
    if let Some(reasoning) = reasoning {
        body["reasoning"] = reasoning;
    }
    body
}

/// Whether `id`, listed by the OpenAI API, names a model that holds a conversation over the Responses API, rather than an older completion, embedding or media model.
fn openai_chat_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    let o_series = id
        .strip_prefix('o')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    (id.starts_with("gpt-") || id.starts_with("chatgpt-") || id.contains("codex") || o_series)
        && !id.contains("instruct")
        && !id.contains("audio")
        && !id.contains("search")
}

/// The Azure OpenAI deployments `AZURE_OPENAI_DEPLOYMENT` names, in order, through `lookup`.
pub(crate) fn azure_deployments(lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    lookup(AZURE_DEPLOYMENT_ENV)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Lists the model ids `provider` offers with `credentials`.
///
/// Codex lists the account's models in priority order. Azure OpenAI offers the
/// deployments `AZURE_OPENAI_DEPLOYMENT` names, and lists the resource's
/// models when it names none. The OpenAI API lists its conversation models,
/// and a server the user declared lists what it serves.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status, or an Azure address that is not set.
pub async fn list_models(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Vec<String>, ProviderError> {
    let host = Host::of(provider, credentials)?;
    match &host {
        Host::Codex => list_codex_models(credentials).await,
        Host::Azure { .. } => {
            let deployments = azure_deployments(env_value);
            if !deployments.is_empty() {
                return Ok(deployments);
            }
            fetch_models(&host, credentials).await
        }
        Host::Api { server: false, .. } => Ok(fetch_models(&host, credentials)
            .await?
            .into_iter()
            .filter(|id| openai_chat_model(id))
            .collect()),
        Host::Api { server: true, .. } => fetch_models(&host, credentials).await,
    }
}

/// Asks `host` for the models it serves, at its models address.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
async fn fetch_models(
    host: &Host,
    credentials: &Credentials,
) -> Result<Vec<String>, ProviderError> {
    let request = reqwest::Client::new().get(host.models_url());
    let response = host.sign(request, credentials, "").send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::from_response(response).await);
    }
    Ok(chat_completions::model_ids(&response.json().await?))
}

/// Turns a `response.usage` object into token counts.
///
/// Cached input and cache writes are carved out of the reported input.
pub(crate) fn tokens_from(usage: &Value) -> Option<Tokens> {
    let count = |value: Option<&Value>| value.and_then(Value::as_u64);
    let input = count(usage.get("input_tokens"))?;
    let details = usage.get("input_tokens_details");
    let cache_read = count(details.and_then(|d| d.get("cached_tokens"))).unwrap_or(0);
    let cache_write = count(details.and_then(|d| d.get("cache_write_tokens"))).unwrap_or(0);
    Some(Tokens {
        input: input.saturating_sub(cache_read + cache_write),
        output: count(usage.get("output_tokens")).unwrap_or(0),
        cache_read,
        cache_write,
    })
}

/// The position of the output item a stream event `event` belongs to, or nought when it names none.
fn item(event: &Value) -> usize {
    event
        .get("output_index")
        .and_then(Value::as_u64)
        .map_or(0, |position| position as usize)
}

/// Builds a [`Reply`] from the events of one Responses stream.
#[derive(Default)]
pub(crate) struct Accumulator {
    /// Reply text so far.
    text: String,
    /// Reasoning summary text so far.
    thinking: String,
    /// Tool calls finished so far.
    tool_calls: Vec<ToolCall>,
    /// Reasoning items finished so far, kept for replay.
    opaque: Vec<Value>,
    /// Usage from the completed response.
    tokens: Option<Tokens>,
    /// The block chunks go to now.
    blocks: Blocks,
    /// Whether a new summary part began since the last reasoning chunk, so the next one is set apart from the text before it.
    parted: bool,
    /// Why the model stopped, once the stream said.
    stop: Stop,
    /// Whether the stream reached the event that ends the response.
    ended: bool,
}

impl Accumulator {
    /// Handles one event's JSON `data`, forwarding chunks to `sink`.
    ///
    /// Returns true once the response has ended.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] for an `error` or `response.failed` event, and for a response the provider's content filter cut, which would otherwise pass for a short or empty answer.
    pub(crate) fn feed(&mut self, data: &str, sink: &mut dyn Sink) -> Result<bool, ProviderError> {
        let Ok(event) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        let kind = event.get("type").and_then(Value::as_str).unwrap_or("");
        match kind {
            "response.output_text.delta" => {
                if let Some(chunk) = event.get("delta").and_then(Value::as_str)
                    && self.blocks.send(sink, Part::Text, item(&event), chunk)
                {
                    self.text.push_str(chunk);
                }
            }
            "response.reasoning_summary_part.added" => {
                if !self.thinking.is_empty() {
                    self.thinking.push_str("\n\n");
                    self.parted = true;
                }
            }
            "response.reasoning_summary_text.delta" => {
                if let Some(chunk) = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .filter(|chunk| !chunk.is_empty())
                {
                    let position = item(&event);
                    let apart = std::mem::take(&mut self.parted)
                        && self.blocks.is_open(Part::Thinking, position);
                    let shown = match apart {
                        true => format!("\n\n{chunk}"),
                        false => chunk.to_owned(),
                    };
                    self.blocks.send(sink, Part::Thinking, position, &shown);
                    self.thinking.push_str(chunk);
                }
            }
            "response.output_item.done" => {
                if let Some(item) = event.get("item") {
                    self.item_done(item);
                }
            }
            "error" => {
                let message = event
                    .get("message")
                    .and_then(Value::as_str)
                    .map_or_else(|| data.to_string(), str::to_string);
                return Err(ProviderError::Stream(message));
            }
            "response.failed" => {
                let message = event
                    .pointer("/response/error/message")
                    .and_then(Value::as_str)
                    .map_or_else(|| data.to_string(), str::to_string);
                return Err(ProviderError::Stream(message));
            }
            "response.completed" | "response.incomplete" => {
                self.tokens = event.pointer("/response/usage").and_then(tokens_from);
                match event
                    .pointer("/response/incomplete_details/reason")
                    .and_then(Value::as_str)
                {
                    Some("max_output_tokens") => self.stop = Stop::Length,
                    Some("content_filter") => {
                        return Err(ProviderError::Stream(
                            "the provider's content filter stopped the answer".to_owned(),
                        ));
                    }
                    _ => {}
                }
                self.ended = true;
                return Ok(true);
            }
            _ => {}
        }
        Ok(false)
    }

    /// Records a finished output item: a tool call or a reasoning item.
    fn item_done(&mut self, item: &Value) {
        let text = |key: &str| {
            item.get(key)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        match item.get("type").and_then(Value::as_str) {
            Some("function_call") => self.tool_calls.push(ToolCall {
                id: text("call_id"),
                name: text("name"),
                arguments: parse_arguments(&text("arguments")),
            }),
            Some("reasoning") if item.get("encrypted_content").is_some() => {
                self.opaque.push(item.clone());
            }
            _ => {}
        }
    }

    /// Ends the stream and returns the reply it described.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Unfinished`] when the stream ended before the event that ends the response, since the reply may then be cut short anywhere.
    pub(crate) fn finish(self) -> Result<Reply, ProviderError> {
        if !self.ended {
            return Err(ProviderError::Unfinished);
        }
        Ok(Reply {
            text: self.text,
            thinking: self.thinking,
            tool_calls: self.tool_calls,
            tokens: self.tokens,
            opaque: self.opaque,
            stop: self.stop,
        })
    }
}

/// Sends the conversation to `provider` using `model` and returns its reply,
/// streaming text and reasoning summary chunks to `sink`.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status, an
/// error event inside the stream, or an Azure address that is not set.
pub async fn complete(
    provider: Provider,
    credentials: &Credentials,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
) -> Result<Reply, ProviderError> {
    let host = Host::of(provider, credentials)?;
    send(&host, credentials, model, request, sink).await
}

/// Sends the conversation to `host` using `model` and returns its reply, streaming chunks to `sink`.
///
/// # Errors
///
/// Same as [`complete`].
async fn send(
    host: &Host,
    credentials: &Credentials,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
) -> Result<Reply, ProviderError> {
    let body = build_body(host, model, &request);
    let post = reqwest::Client::new().post(host.responses_url());
    let response = host
        .sign(post, credentials, model)
        .header("accept", "text/event-stream")
        .json(&body)
        .send()
        .await?;
    let mut events = sse::events(response).await?;
    let mut accumulator = Accumulator::default();
    while let Some(event) = events.next().await {
        if accumulator.feed(&event?.data, sink)? {
            break;
        }
    }
    accumulator.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ToolResult;

    /// A sink that records what it is given.
    #[derive(Default)]
    struct Recorder {
        /// Text chunks received.
        text: Vec<String>,
        /// Thinking chunks received.
        thinking: Vec<String>,
        /// The kinds of the blocks started, in order.
        starts: Vec<Part>,
    }

    impl Sink for Recorder {
        /// Records the start of a block.
        fn start(&mut self, part: Part) {
            self.starts.push(part);
        }

        /// Records a text chunk.
        fn text(&mut self, chunk: &str) {
            self.text.push(chunk.to_string());
        }

        /// Records a thinking chunk.
        fn thinking(&mut self, chunk: &str) {
            self.thinking.push(chunk.to_string());
        }
    }

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
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: "ok".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        ];
        let items = input_json(&messages);
        assert_eq!(items[0]["role"], "assistant");
        assert_eq!(items[1]["type"], "function_call");
        assert_eq!(items[1]["arguments"], "{\"path\":\"a\"}");
        assert_eq!(items[2]["call_id"], "c1");
    }

    /// A tool result that carries one PNG image.
    fn pictured() -> ToolResult {
        ToolResult {
            call_id: "c1".into(),
            name: "read".into(),
            text: "[image]".into(),
            is_error: false,
            images: vec![crate::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            }],
        }
    }

    /// A result with images becomes an output list of text and data-URL image parts.
    #[test]
    fn translates_tool_result_images() {
        let items = input_json(&[Message::ToolResults(vec![pictured()])]);
        let output = &items[0]["output"];
        assert_eq!(output[0]["type"], "input_text");
        assert_eq!(output[1]["type"], "input_image");
        assert_eq!(output[1]["image_url"], "data:image/png;base64,AAAA");
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

    /// Reasoning items replay first and without ids; Claude blocks are dropped.
    #[test]
    fn replays_encrypted_reasoning() {
        let reply = Reply {
            text: "hi".into(),
            opaque: vec![
                json!({ "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "ENC" }),
                json!({ "type": "thinking", "thinking": "x", "signature": "s" }),
            ],
            ..Reply::default()
        };
        let items = input_json(&[Message::Assistant(reply)]);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["encrypted_content"], "ENC");
        assert!(items[0].get("id").is_none());
        assert_eq!(items[1]["role"], "assistant");
    }

    /// The body asks for reasoning with a summary, encrypted content and the system prompt.
    #[test]
    fn body_carries_reasoning_and_system() {
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &[],
            effort: Some(Effort::XHigh),
        };
        let body = build_body(&Host::Codex, "gpt-5.5", &request);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(
            body["reasoning"],
            json!({ "effort": "xhigh", "summary": "auto" })
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["store"], false);
        let quiet = build_body(
            &Host::Codex,
            "gpt-5.5",
            &Request {
                effort: None,
                ..request
            },
        );
        assert!(quiet.get("reasoning").is_none());
    }

    /// An effort the model lacks is clamped down to the nearest served level.
    #[test]
    fn effort_is_clamped_to_the_model() {
        let sent = |model, effort| {
            reasoning_param(&codex_efforts(model), Some(effort)).unwrap()["effort"].clone()
        };
        assert_eq!(sent("gpt-5.5", Effort::Max), "xhigh");
        assert_eq!(sent("gpt-6-sol", Effort::Max), "max");
        assert_eq!(sent("gpt-5.1-codex-mini", Effort::Low), "medium");
    }

    /// Headers carry the routing hint, account and session ids.
    #[test]
    fn headers_identify_the_client() {
        let creds: Credentials = serde_json::from_value(json!({
            "access": "tok", "refresh": "r", "expires_at_ms": 0, "account_id": "acct",
        }))
        .unwrap();
        let headers = headers(&creds, "gpt-5.5");
        let get = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(get("authorization"), Some("Bearer tok"));
        assert_eq!(get("chatgpt-account-id"), Some("acct"));
        assert_eq!(get("x-codex-routing-hint"), Some("model=gpt-5.5"));
        assert_eq!(get("openai-beta"), Some("responses=experimental"));
        assert_eq!(get("session_id"), Some(identity::session_id()));
        assert_eq!(get("version"), Some(CLIENT_VERSION));
    }

    /// A region-pinned token declares its residency; an unconstrained one does not.
    #[test]
    fn residency_comes_from_the_token() {
        let token = |claims: Value| {
            let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode(json!({ JWT_CLAIM: claims }).to_string());
            format!("h.{payload}.s")
        };
        assert_eq!(
            residency(&token(json!({ "chatgpt_data_residency": "eu" }))).as_deref(),
            Some("eu")
        );
        assert_eq!(
            residency(&token(json!({ "chatgpt_data_residency": "no_constraint" }))),
            None
        );
        assert_eq!(residency("opaque"), None);
    }

    /// Feeds each of `events` into a fresh accumulator and returns how the stream ended.
    fn ended(events: &[Value], sink: &mut Recorder) -> Result<Reply, ProviderError> {
        let mut accumulator = Accumulator::default();
        for event in events {
            if accumulator.feed(&event.to_string(), sink).unwrap() {
                break;
            }
        }
        accumulator.finish()
    }

    /// Feeds each of `events` into a fresh accumulator and returns the reply.
    fn run(events: &[Value], sink: &mut Recorder) -> Reply {
        ended(events, sink).unwrap()
    }

    /// Reasoning summary deltas stream to the sink and encrypted items are kept.
    #[test]
    fn parses_reasoning_stream() {
        let events = [
            json!({ "type": "response.reasoning_summary_part.added", "summary_index": 0 }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Looking " }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "at it" }),
            json!({ "type": "response.reasoning_summary_part.added", "summary_index": 1 }),
            json!({ "type": "response.reasoning_summary_text.delta", "delta": "Then more" }),
            json!({ "type": "response.output_item.done", "item": {
                "type": "reasoning", "id": "rs", "summary": [], "encrypted_content": "ENC" } }),
            json!({ "type": "response.output_text.delta", "delta": "done" }),
            json!({ "type": "response.output_item.done", "item": {
                "type": "function_call", "call_id": "c", "name": "read", "arguments": "{\"p\":1}" } }),
            json!({ "type": "response.completed", "response": { "end_turn": false, "usage": {
                "input_tokens": 100, "output_tokens": 30,
                "input_tokens_details": { "cached_tokens": 60 } } } }),
        ];
        let mut sink = Recorder::default();
        let reply = run(&events, &mut sink);
        assert_eq!(sink.thinking, ["Looking ", "at it", "\n\nThen more"]);
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        assert_eq!(reply.thinking, "Looking at it\n\nThen more");
        assert_eq!(sink.text, ["done"]);
        assert_eq!(reply.opaque.len(), 1);
        assert_eq!(reply.opaque[0]["encrypted_content"], "ENC");
        assert_eq!(reply.tool_calls[0].arguments["p"], 1);
        assert_eq!(
            reply.tokens,
            Some(Tokens {
                input: 40,
                output: 30,
                cache_read: 60,
                cache_write: 0,
            })
        );
    }

    /// Failures and error events end the stream with an error.
    #[test]
    fn failures_are_errors() {
        let mut accumulator = Accumulator::default();
        let failed =
            json!({ "type": "response.failed", "response": { "error": { "message": "nope" } } });
        let error = accumulator
            .feed(&failed.to_string(), &mut Recorder::default())
            .unwrap_err();
        assert!(error.to_string().contains("nope"));
    }

    /// A response without usage leaves the tokens unset.
    #[test]
    fn missing_usage_is_none() {
        let events = [json!({ "type": "response.completed", "response": {} })];
        assert_eq!(run(&events, &mut Recorder::default()).tokens, None);
    }

    /// A response the output limit cut off says so, and one that completes does not.
    #[test]
    fn the_output_limit_is_reported() {
        let cut = [json!({ "type": "response.incomplete", "response": {
            "incomplete_details": { "reason": "max_output_tokens" } } })];
        assert_eq!(run(&cut, &mut Recorder::default()).stop, Stop::Length);
        let whole = [json!({ "type": "response.completed", "response": {} })];
        assert_eq!(run(&whole, &mut Recorder::default()).stop, Stop::Finished);
    }

    /// A response the provider's content filter cut is an error, not a short answer.
    #[test]
    fn content_filter_is_an_error() {
        let mut accumulator = Accumulator::default();
        let filtered = json!({ "type": "response.incomplete", "response": {
            "incomplete_details": { "reason": "content_filter" } } });
        let error = accumulator
            .feed(&filtered.to_string(), &mut Recorder::default())
            .unwrap_err();
        assert!(error.to_string().contains("content filter"), "{error}");
        assert!(!error.is_transient());
    }

    /// A stream that ends before the response does is unfinished rather than a short reply.
    #[test]
    fn a_stream_without_its_end_is_unfinished() {
        let events = [json!({ "type": "response.output_text.delta", "delta": "half" })];
        let error = ended(&events, &mut Recorder::default()).unwrap_err();
        assert!(matches!(error, ProviderError::Unfinished), "{error}");
    }

    /// A host for the OpenAI API at `base_url`, or a declared server there when `server` is set.
    fn api(base_url: &str, server: bool) -> Host {
        Host::Api {
            base_url: base_url.to_owned(),
            headers: &[("X-Team", "infra")],
            server,
        }
    }

    /// A request saying `hi` at `effort`, with one tool.
    fn hello(messages: &[Message], effort: Option<Effort>) -> Request<'_> {
        static TOOLS: std::sync::OnceLock<Vec<ToolSpec>> = std::sync::OnceLock::new();
        Request {
            system: "sys",
            messages,
            tools: TOOLS.get_or_init(|| {
                vec![ToolSpec {
                    name: "read".into(),
                    description: "d".into(),
                    schema: json!({ "type": "object" }),
                }]
            }),
            effort,
        }
    }

    /// The OpenAI API is sent reasoning only for a model that reasons, clamped to its levels, and a declared server is sent no cache key and encrypted reasoning only with reasoning.
    #[test]
    fn api_bodies_follow_the_model() {
        let messages = [
            Message::User("hi".into()),
            Message::Images(vec![crate::Image {
                media_type: "image/png".into(),
                data: "AAAA".into(),
            }]),
        ];
        let openai = api("https://api.openai.com/v1", false);
        let body = build_body(&openai, "gpt-5", &hello(&messages, Some(Effort::Max)));
        assert_eq!(
            body["reasoning"],
            json!({ "effort": "high", "summary": "auto" })
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert!(body.get("prompt_cache_key").is_some());
        assert_eq!(body["tools"][0]["name"], "read");
        assert_eq!(body["input"][1]["content"][0]["type"], "input_image");
        let plain = build_body(&openai, "gpt-4.1", &hello(&messages, Some(Effort::High)));
        assert!(plain.get("reasoning").is_none());
        let o3 = build_body(&openai, "o3", &hello(&messages, Some(Effort::XHigh)));
        assert_eq!(o3["reasoning"]["effort"], "high");
        let server = api("http://localhost:4000/v1", true);
        let quiet = build_body(&server, "qwen3", &hello(&messages, Some(Effort::High)));
        assert!(quiet.get("reasoning").is_none());
        assert!(quiet.get("include").is_none() && quiet.get("prompt_cache_key").is_none());
        let thinking = build_body(
            &server,
            "openai/gpt-oss-120b",
            &hello(&messages, Some(Effort::Low)),
        );
        assert_eq!(thinking["reasoning"]["effort"], "low");
        assert!(thinking.get("include").is_some());
    }

    /// The models that reason are told apart by their ids, wherever they are prefixed.
    #[test]
    fn api_efforts_follow_the_id() {
        assert_eq!(
            api_efforts("gpt-5"),
            [Effort::Low, Effort::Medium, Effort::High]
        );
        assert_eq!(
            api_efforts("o4-mini"),
            [Effort::Low, Effort::Medium, Effort::High]
        );
        assert!(api_efforts("gpt-4o").is_empty());
        assert!(api_efforts("llama-3.3-70b").is_empty());
        assert!(!api_efforts("my-gpt-5.5-deployment").is_empty());
        assert_eq!(efforts(Provider::Codex, "gpt-6-sol"), Effort::ALL);
        assert!(efforts(Provider::OpenAi, "gpt-4.1").is_empty());
    }

    /// The Azure address comes from the base URL, the endpoint or the resource name, in that order, with the API version or `v1`.
    #[test]
    fn azure_address_comes_from_the_environment() {
        let lookup = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        };
        let host = azure_host(lookup(&[(AZURE_RESOURCE_ENV, "res")])).unwrap();
        assert_eq!(
            host.responses_url(),
            "https://res.openai.azure.com/openai/v1/responses?api-version=v1"
        );
        let host = azure_host(lookup(&[
            (AZURE_ENDPOINT_ENV, "https://e.openai.azure.com/"),
            (AZURE_RESOURCE_ENV, "res"),
            (AZURE_API_VERSION_ENV, "2025-04-01-preview"),
        ]))
        .unwrap();
        assert_eq!(
            host.models_url(),
            "https://e.openai.azure.com/openai/v1/models?api-version=2025-04-01-preview"
        );
        let host = azure_host(lookup(&[
            (AZURE_BASE_URL_ENV, "https://proxy.example.com/openai"),
            (AZURE_ENDPOINT_ENV, "https://e.openai.azure.com"),
        ]))
        .unwrap();
        assert_eq!(
            host.responses_url(),
            "https://proxy.example.com/openai/responses?api-version=v1"
        );
        let missing = azure_host(lookup(&[])).unwrap_err();
        assert!(
            missing.to_string().contains(AZURE_ENDPOINT_ENV),
            "{missing}"
        );
        assert_eq!(
            azure_deployments(lookup(&[(AZURE_DEPLOYMENT_ENV, "gpt-5-prod, mini ,")])),
            ["gpt-5-prod", "mini"]
        );
    }

    /// Serves `body` with `kind` as its content type once on a local port and returns the address and the request it received.
    fn serve_once(
        kind: &'static str,
        body: &'static str,
    ) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let read = stream.read(&mut buffer).unwrap();
                received.extend_from_slice(&buffer[..read]);
                let text = String::from_utf8_lossy(&received).to_string();
                if let Some(split) = text.find("\r\n\r\n") {
                    let length = text
                        .lines()
                        .find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("content-length: ")
                                .map(str::to_string)
                        })
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if received.len() >= split + 4 + length {
                        break;
                    }
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (address, handle)
    }

    /// A streamed Responses reply, with reasoning, text and a tool call.
    const STREAM: &str = concat!(
        "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"hmm\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"hel\"}\n\n",
        "data: {\"type\":\"response.output_text.delta\",\"output_index\":1,\"delta\":\"lo\"}\n\n",
        "data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\",\"call_id\":\"c1\",\"name\":\"read\",\"arguments\":\"{}\"}}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n"
    );

    /// A request to the OpenAI API goes to `/responses` with the bearer key and the host's headers, and the stream comes back as a reply.
    #[tokio::test]
    async fn completes_against_the_api() {
        let (address, server) = serve_once("text/event-stream", STREAM);
        let credentials = Credentials::static_key("sk-test".to_owned());
        let messages = [Message::User("hi".into())];
        let mut sink = Recorder::default();
        let host = api(&format!("{address}/v1"), false);
        let reply = send(
            &host,
            &credentials,
            "gpt-5",
            hello(&messages, Some(Effort::Medium)),
            &mut sink,
        )
        .await
        .unwrap();
        assert_eq!(reply.text, "hello");
        assert_eq!(reply.thinking, "hmm");
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(reply.tokens.map(|tokens| tokens.output), Some(2));
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        let received = server.join().unwrap();
        let lower = received.to_ascii_lowercase();
        assert!(lower.starts_with("post /v1/responses "), "{received}");
        assert!(lower.contains("authorization: bearer sk-test"));
        assert!(lower.contains("x-team: infra"));
        assert!(!lower.contains("chatgpt-account-id") && !lower.contains("originator"));
        let body: Value =
            serde_json::from_str(&received[received.find("\r\n\r\n").unwrap() + 4..]).unwrap();
        assert_eq!(body["model"], "gpt-5");
        assert_eq!(body["reasoning"]["effort"], "medium");
    }

    /// A request to Azure OpenAI names the API version in the query and sends the key in `api-key`, not as a bearer token.
    #[tokio::test]
    async fn completes_against_azure() {
        let (address, server) = serve_once("text/event-stream", STREAM);
        let credentials = Credentials::static_key("az-key".to_owned());
        let host = Host::Azure {
            base_url: format!("{address}/openai/v1"),
            api_version: "preview".to_owned(),
        };
        let messages = [Message::User("hi".into())];
        let reply = send(
            &host,
            &credentials,
            "gpt-5-prod",
            hello(&messages, None),
            &mut Recorder::default(),
        )
        .await
        .unwrap();
        assert_eq!(reply.text, "hello");
        let received = server.join().unwrap().to_ascii_lowercase();
        assert!(
            received.starts_with("post /openai/v1/responses?api-version=preview "),
            "{received}"
        );
        assert!(received.contains("api-key: az-key"));
        assert!(!received.contains("authorization:"));
    }

    /// The OpenAI API's model list keeps the models that hold a conversation, and a declared server's keeps all but embedding and media models.
    #[tokio::test]
    async fn lists_api_models() {
        const LIST: &str = r#"{"object":"list","data":[{"id":"gpt-5"},{"id":"o3"},{"id":"text-embedding-3-large"},{"id":"davinci-002"},{"id":"gpt-3.5-turbo-instruct"},{"id":"gpt-4o-audio-preview"}]}"#;
        let credentials = Credentials::static_key("sk".to_owned());
        let (address, server) = serve_once("application/json", LIST);
        let host = api(&address, false);
        let listed: Vec<String> = fetch_models(&host, &credentials)
            .await
            .unwrap()
            .into_iter()
            .filter(|id| openai_chat_model(id))
            .collect();
        assert_eq!(listed, ["gpt-5", "o3"]);
        assert!(
            server
                .join()
                .unwrap()
                .to_ascii_lowercase()
                .starts_with("get /models ")
        );
        let (address, _server) = serve_once("application/json", LIST);
        let all = fetch_models(&api(&address, true), &credentials)
            .await
            .unwrap();
        assert!(all.contains(&"davinci-002".to_owned()));
        assert!(!all.iter().any(|id| id.contains("embedding")));
    }
}
