//! Streaming responses against the ChatGPT Codex backend with an OAuth token.

use base64::Engine;
use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::chat::{Blocks, parse_arguments};
use crate::identity;
use crate::models::codex_efforts;
use crate::{
    Effort, Message, Part, ProviderError, Reply, Request, Sink, Tokens, ToolCall, ToolSpec, sse,
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

/// Lists the model ids the account can use, in the backend's priority order.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub async fn list_models(credentials: &Credentials) -> Result<Vec<String>, ProviderError> {
    let url = format!("{MODELS_URL}?client_version={CLIENT_VERSION}");
    let request = reqwest::Client::new().get(url);
    let response = authorized(request, credentials, "").send().await?;
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
            Message::Compaction(_) | Message::Pruning(_) => {}
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

/// Picks the effort `model` serves that is closest to `wanted` without going over.
fn clamp_effort(model: &str, wanted: Effort) -> Option<Effort> {
    let levels = codex_efforts(model);
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

/// The `reasoning` field of a request for `model` at `effort`, when it asks for reasoning.
pub(crate) fn reasoning_param(model: &str, effort: Option<Effort>) -> Option<Value> {
    let effort = clamp_effort(model, effort?)?;
    Some(json!({ "effort": effort.as_str(), "summary": "auto" }))
}

/// Builds the request body for `model`.
pub(crate) fn build_body(model: &str, request: &Request<'_>) -> Value {
    let mut body = json!({
        "model": model,
        "instructions": request.system,
        "input": input_json(request.messages),
        "tools": tools_json(request.tools),
        "tool_choice": "auto",
        "parallel_tool_calls": true,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
        "prompt_cache_key": identity::session_id(),
    });
    if let Some(reasoning) = reasoning_param(model, request.effort) {
        body["reasoning"] = reasoning;
    }
    body
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
}

impl Accumulator {
    /// Handles one event's JSON `data`, forwarding chunks to `sink`.
    ///
    /// Returns true once the response has ended.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] for an `error` or `response.failed` event.
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
    pub(crate) fn finish(self) -> Reply {
        Reply {
            text: self.text,
            thinking: self.thinking,
            tool_calls: self.tool_calls,
            tokens: self.tokens,
            opaque: self.opaque,
        }
    }
}

/// Sends the conversation to Codex using `model` and returns its reply,
/// streaming text and reasoning summary chunks to `sink`.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an
/// error event inside the stream.
pub async fn complete(
    credentials: &Credentials,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
) -> Result<Reply, ProviderError> {
    let body = build_body(model, &request);
    let response = authorized(reqwest::Client::new().post(URL), credentials, model)
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
    Ok(accumulator.finish())
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
        let body = build_body("gpt-5.5", &request);
        assert_eq!(body["instructions"], "be brief");
        assert_eq!(
            body["reasoning"],
            json!({ "effort": "xhigh", "summary": "auto" })
        );
        assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(body["store"], false);
        let quiet = build_body(
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
        let sent = |model, effort| reasoning_param(model, Some(effort)).unwrap()["effort"].clone();
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

    /// Feeds each of `events` into a fresh accumulator and returns the reply.
    fn run(events: &[Value], sink: &mut Recorder) -> Reply {
        let mut accumulator = Accumulator::default();
        for event in events {
            if accumulator.feed(&event.to_string(), sink).unwrap() {
                break;
            }
        }
        accumulator.finish()
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
}
