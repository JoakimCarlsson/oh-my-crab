//! Streaming messages against the Anthropic Messages API.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::chat::{Blocks, parse_arguments};
use crate::identity;
use crate::models::{
    ClaudeModel, ThinkingMode, claude_efforts, claude_output_limit, claude_supports_display,
    claude_thinking_mode, parse_claude,
};
use crate::{
    Effort, Message, Part, ProviderError, Reply, Request, Sink, Tokens, ToolCall, ToolSpec, sse,
};

/// Messages endpoint.
const URL: &str = "https://api.anthropic.com/v1/messages";

/// Model list endpoint.
const MODELS_URL: &str = "https://api.anthropic.com/v1/models";

/// Output tokens kept free for the visible answer when a thinking budget is set.
const ANSWER_RESERVE: u64 = 4_000;

/// One server-sent event payload from the Messages stream.
#[derive(Deserialize)]
struct Payload {
    /// Event kind, such as `content_block_delta` or `error`.
    #[serde(rename = "type")]
    kind: String,
    /// Incremental content for delta events; for `message_delta` it carries the stop reason.
    delta: Option<Delta>,
    /// Error details for error events.
    error: Option<ErrorBody>,
    /// Position of the content block the event belongs to.
    index: Option<usize>,
    /// The block opened by a `content_block_start` event.
    content_block: Option<Block>,
    /// The message announced by a `message_start` event.
    message: Option<StartedMessage>,
    /// Running usage totals carried by `message_delta` events.
    usage: Option<Usage>,
}

/// The message object of a `message_start` event.
#[derive(Deserialize)]
struct StartedMessage {
    /// Usage known when the message started.
    usage: Option<Usage>,
}

/// Token usage as the API reports it.
#[derive(Deserialize, Default)]
struct Usage {
    /// Input tokens that were neither read from nor written to the cache.
    input_tokens: Option<u64>,
    /// Output tokens so far, thinking included.
    output_tokens: Option<u64>,
    /// Input tokens written to the cache.
    cache_creation_input_tokens: Option<u64>,
    /// Input tokens read from the cache.
    cache_read_input_tokens: Option<u64>,
}

/// A content block announced by a `content_block_start` event.
#[derive(Deserialize)]
struct Block {
    /// Block kind, such as `text`, `thinking`, `redacted_thinking` or `tool_use`.
    #[serde(rename = "type")]
    kind: String,
    /// Tool call id for `tool_use` blocks.
    id: Option<String>,
    /// Tool name for `tool_use` blocks.
    name: Option<String>,
    /// Opaque payload of `redacted_thinking` blocks.
    data: Option<String>,
    /// Thinking text the block opens with.
    thinking: Option<String>,
    /// Signature the block opens with.
    signature: Option<String>,
}

/// The delta carried by a `content_block_delta` event.
#[derive(Deserialize)]
struct Delta {
    /// Text appended to the reply, absent for non-text deltas.
    text: Option<String>,
    /// Thinking text appended to a thinking block.
    thinking: Option<String>,
    /// Signature of the thinking block, which arrives just before it closes.
    signature: Option<String>,
    /// Fragment of a tool call's argument JSON, absent for other deltas.
    partial_json: Option<String>,
}

/// The error carried by an `error` event.
#[derive(Deserialize)]
struct ErrorBody {
    /// Human-readable error message.
    message: String,
}

/// A content block still being received.
enum Open {
    /// A block of reply text, which streams straight to the sink.
    Text,
    /// A block of thinking with the signature that vouches for it.
    Thinking {
        /// Thinking text received so far.
        text: String,
        /// Signature received so far.
        signature: String,
    },
    /// A thinking block the provider withheld, kept to be sent back.
    Redacted {
        /// The opaque payload.
        data: String,
    },
    /// A tool call whose arguments are still streaming in.
    Call {
        /// Tool call id.
        id: String,
        /// Tool name as sent on the wire.
        name: String,
        /// Argument JSON received so far.
        json: String,
    },
}

/// Builds a [`Reply`] from the events of one Messages stream.
pub(crate) struct Accumulator {
    /// Whether tool names carry the wire prefix.
    prefixed: bool,
    /// Reply text so far.
    text: String,
    /// Thinking text so far.
    thinking: String,
    /// Open blocks by position.
    blocks: BTreeMap<usize, Open>,
    /// The block chunks go to now.
    streaming: Blocks,
    /// Usage reported so far.
    tokens: Tokens,
    /// Whether any usage was reported.
    counted: bool,
}

impl Accumulator {
    /// Starts an empty accumulator; `prefixed` says tool names carry the wire prefix.
    pub(crate) fn new(prefixed: bool) -> Self {
        Self {
            prefixed,
            text: String::new(),
            thinking: String::new(),
            blocks: BTreeMap::new(),
            streaming: Blocks::default(),
            tokens: Tokens::default(),
            counted: false,
        }
    }

    /// Folds `usage` into the totals, keeping what it does not mention.
    fn count(&mut self, usage: Usage) {
        self.counted = true;
        if let Some(input) = usage.input_tokens {
            self.tokens.input = input;
        }
        if let Some(output) = usage.output_tokens {
            self.tokens.output = output;
        }
        if let Some(read) = usage.cache_read_input_tokens {
            self.tokens.cache_read = read;
        }
        if let Some(write) = usage.cache_creation_input_tokens {
            self.tokens.cache_write = write;
        }
    }

    /// Handles one event's JSON `data`, forwarding chunks to `sink`.
    ///
    /// Returns true once the message is complete.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] for an `error` event.
    pub(crate) fn feed(&mut self, data: &str, sink: &mut dyn Sink) -> Result<bool, ProviderError> {
        let Ok(payload) = serde_json::from_str::<Payload>(data) else {
            return Ok(false);
        };
        match payload.kind.as_str() {
            "message_start" => {
                if let Some(usage) = payload.message.and_then(|message| message.usage) {
                    self.count(usage);
                }
            }
            "message_delta" => {
                if let Some(usage) = payload.usage {
                    self.count(usage);
                }
            }
            "content_block_start" => {
                if let (Some(index), Some(block)) = (payload.index, payload.content_block) {
                    self.open(index, block);
                }
            }
            "content_block_delta" => {
                if let Some(delta) = payload.delta {
                    self.delta(payload.index, delta, sink);
                }
            }
            "error" => {
                let message = payload
                    .error
                    .map_or_else(|| data.to_string(), |e| e.message);
                return Err(ProviderError::Stream(message));
            }
            "message_stop" => return Ok(true),
            _ => {}
        }
        Ok(false)
    }

    /// Records the block that `content_block_start` opened at `index`.
    fn open(&mut self, index: usize, block: Block) {
        let open = match block.kind.as_str() {
            "text" => Open::Text,
            "thinking" => Open::Thinking {
                text: block.thinking.unwrap_or_default(),
                signature: block.signature.unwrap_or_default(),
            },
            "redacted_thinking" => Open::Redacted {
                data: block.data.unwrap_or_default(),
            },
            "tool_use" => Open::Call {
                id: block.id.unwrap_or_default(),
                name: block.name.unwrap_or_default(),
                json: String::new(),
            },
            _ => return,
        };
        self.blocks.insert(index, open);
    }

    /// Applies a `content_block_delta` to the block at `index`.
    fn delta(&mut self, index: Option<usize>, delta: Delta, sink: &mut dyn Sink) {
        let position = index.unwrap_or(0);
        if let Some(chunk) = delta.text
            && self.streaming.send(sink, Part::Text, position, &chunk)
        {
            self.text.push_str(&chunk);
        }
        let open = index.and_then(|i| self.blocks.get_mut(&i));
        match open {
            Some(Open::Thinking { text, signature }) => {
                if let Some(chunk) = delta.thinking
                    && self.streaming.send(sink, Part::Thinking, position, &chunk)
                {
                    self.thinking.push_str(&chunk);
                    text.push_str(&chunk);
                }
                if let Some(part) = delta.signature {
                    signature.push_str(&part);
                }
            }
            Some(Open::Call { json, .. }) => {
                if let Some(fragment) = delta.partial_json {
                    json.push_str(&fragment);
                }
            }
            _ => {}
        }
    }

    /// Ends the stream and returns the reply it described.
    pub(crate) fn finish(self) -> Reply {
        let mut opaque = Vec::new();
        let mut tool_calls = Vec::new();
        for block in self.blocks.into_values() {
            match block {
                Open::Thinking { text, signature } if !signature.is_empty() => {
                    opaque.push(json!({
                        "type": "thinking",
                        "thinking": text,
                        "signature": signature,
                    }));
                }
                Open::Redacted { data } if !data.is_empty() => {
                    opaque.push(json!({ "type": "redacted_thinking", "data": data }));
                }
                Open::Call { id, name, json } => {
                    let name = if self.prefixed {
                        identity::strip_tool_prefix(&name).to_string()
                    } else {
                        name
                    };
                    tool_calls.push(ToolCall {
                        id,
                        name,
                        arguments: parse_arguments(&json),
                    });
                }
                _ => {}
            }
        }
        Reply {
            text: self.text,
            thinking: self.thinking,
            tool_calls,
            tokens: self.counted.then_some(self.tokens),
            opaque,
        }
    }
}

/// The thinking-related fields of a request.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ThinkingParams {
    /// The `thinking` field, when thinking is asked for.
    pub thinking: Option<Value>,
    /// The `output_config.effort` value, when one is sent.
    pub effort: Option<Effort>,
    /// The `max_tokens` to request.
    pub max_tokens: u64,
}

/// Picks the effort `model` serves that is closest to `wanted` without going over.
fn clamp_effort(model: &str, wanted: Effort) -> Option<Effort> {
    let levels = claude_efforts(model);
    if levels.contains(&wanted) {
        return Some(wanted);
    }
    let rank = Effort::ALL.iter().position(|e| *e == wanted)?;
    levels
        .iter()
        .rev()
        .find(|level| Effort::ALL.iter().position(|e| e == *level) <= Some(rank))
        .or(levels.first())
        .copied()
}

/// Token budget the budget-thinking models get for `effort`.
fn budget_for(effort: Effort) -> u64 {
    match effort {
        Effort::Low => 4_096,
        Effort::Medium => 8_192,
        Effort::High => 16_384,
        Effort::XHigh | Effort::Max => 32_768,
    }
}

/// Maps `effort` onto the thinking fields `model` takes.
///
/// Adaptive models get `thinking: adaptive` and `output_config.effort`; the
/// others get an enabled budget, with `max_tokens` kept above it.
pub(crate) fn thinking_params(model: &str, effort: Option<Effort>) -> ThinkingParams {
    let parsed = parse_claude(model);
    let limit = claude_output_limit(&parsed);
    let none = ThinkingParams {
        thinking: None,
        effort: None,
        max_tokens: limit,
    };
    let Some(effort) = effort.and_then(|wanted| clamp_effort(model, wanted)) else {
        return none;
    };
    match claude_thinking_mode(&parsed) {
        ThinkingMode::None => none,
        ThinkingMode::Adaptive => {
            let mut thinking = json!({ "type": "adaptive" });
            if claude_supports_display(&parsed) {
                thinking["display"] = json!("summarized");
            }
            ThinkingParams {
                thinking: Some(thinking),
                effort: Some(effort),
                max_tokens: limit,
            }
        }
        mode @ (ThinkingMode::Budget | ThinkingMode::BudgetEffort) => {
            let budget = budget_for(effort).min(limit.saturating_sub(ANSWER_RESERVE));
            ThinkingParams {
                thinking: Some(json!({ "type": "enabled", "budget_tokens": budget })),
                effort: (mode == ThinkingMode::BudgetEffort).then_some(effort),
                max_tokens: limit,
            }
        }
    }
}

/// Whether replaying signed thinking blocks is valid for a request with `params`.
fn replays_thinking(parsed: &ClaudeModel, params: &ThinkingParams) -> bool {
    match claude_thinking_mode(parsed) {
        ThinkingMode::None => false,
        ThinkingMode::Adaptive => true,
        ThinkingMode::Budget | ThinkingMode::BudgetEffort => params.thinking.is_some(),
    }
}

/// Translates `messages` into the Messages API `messages` array.
///
/// Signed thinking blocks kept in a reply's `opaque` go back first in the
/// assistant message that carries them when `replay` is set; blocks that are
/// not Claude thinking are dropped. Tool names get the wire prefix when
/// `prefixed` is set.
pub(crate) fn messages_json(messages: &[Message], prefixed: bool, replay: bool) -> Vec<Value> {
    let wire_name = |name: &str| {
        if prefixed {
            identity::prefix_tool(name)
        } else {
            name.to_string()
        }
    };
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(text) => Some(json!({ "role": "user", "content": text })),
            Message::Assistant(reply) => {
                let mut blocks = Vec::new();
                if replay {
                    blocks.extend(
                        reply
                            .opaque
                            .iter()
                            .filter(|block| is_thinking(block))
                            .cloned(),
                    );
                }
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
                        "name": wire_name(&call.name),
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

/// Whether `block` is a Claude thinking or redacted thinking block.
fn is_thinking(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(Value::as_str),
        Some("thinking" | "redacted_thinking")
    )
}

/// Translates `tools` into the Messages API `tools` array.
pub(crate) fn tools_json(tools: &[ToolSpec], prefixed: bool) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            let name = if prefixed {
                identity::prefix_tool(&tool.name)
            } else {
                tool.name.clone()
            };
            json!({
                "name": name,
                "description": tool.description,
                "input_schema": tool.schema,
            })
        })
        .collect()
}

/// Marks the last content block of the last message as a cache breakpoint.
fn mark_last_message(messages: &mut [Value]) {
    let Some(last) = messages.last_mut() else {
        return;
    };
    if let Some(text) = last["content"].as_str().map(str::to_string) {
        last["content"] = json!([{ "type": "text", "text": text }]);
    }
    if let Some(block) = last["content"]
        .as_array_mut()
        .and_then(|blocks| blocks.last_mut())
    {
        block["cache_control"] = json!({ "type": "ephemeral" });
    }
}

/// The text of the first user message, which seeds the billing fingerprint.
fn first_user_text(messages: &[Message]) -> &str {
    messages
        .iter()
        .find_map(|message| match message {
            Message::User(text) if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .unwrap_or("")
}

/// Builds the request body, with the billing placeholder still in place for OAuth.
pub(crate) fn build_body(
    model: &str,
    request: &Request<'_>,
    credentials: &Credentials,
    params: &ThinkingParams,
) -> Value {
    let oauth = identity::is_oauth_token(&credentials.access);
    let parsed = parse_claude(model);
    let mut messages = messages_json(request.messages, oauth, replays_thinking(&parsed, params));
    mark_last_message(&mut messages);
    let mut system = Vec::new();
    if oauth {
        system.push(json!({
            "type": "text",
            "text": identity::billing_header(first_user_text(request.messages)),
        }));
        system.push(json!({
            "type": "text",
            "text": identity::IDENTITY,
            "cache_control": { "type": "ephemeral" },
        }));
    }
    if !request.system.trim().is_empty() {
        let mut block = json!({ "type": "text", "text": request.system });
        block["cache_control"] = json!({ "type": "ephemeral" });
        system.push(block);
    }
    let mut body = json!({
        "model": model,
        "max_tokens": params.max_tokens,
        "stream": true,
        "messages": messages,
    });
    if !system.is_empty() {
        body["system"] = Value::Array(system);
    }
    if oauth || !request.tools.is_empty() {
        body["tools"] = Value::Array(tools_json(request.tools, oauth));
    }
    if oauth {
        let user_id =
            identity::metadata_user_id(credentials.account_id.as_deref(), identity::session_id());
        body["metadata"] = json!({ "user_id": user_id });
    }
    if let Some(thinking) = &params.thinking {
        body["thinking"] = thinking.clone();
        if oauth {
            body["context_management"] =
                json!({ "edits": [{ "type": "clear_thinking_20251015", "keep": "all" }] });
        }
    }
    if let Some(effort) = params.effort {
        body["output_config"] = json!({ "effort": effort.as_str() });
    }
    body
}

/// The `anthropic-beta` values for a request to `model` sent with `credentials`.
fn betas(credentials: &Credentials, params: &ThinkingParams) -> Vec<&'static str> {
    if identity::is_oauth_token(&credentials.access) {
        identity::oauth_betas(params.thinking.is_some() || params.effort.is_some())
    } else {
        let budget = params
            .thinking
            .as_ref()
            .is_some_and(|thinking| thinking["type"] == "enabled");
        identity::api_key_betas(budget, params.effort.is_some())
    }
}

/// Adds the identity headers for `credentials` to `request`.
fn authorized(
    mut request: reqwest::RequestBuilder,
    credentials: &Credentials,
    betas: &[&str],
    stream: bool,
) -> reqwest::RequestBuilder {
    for (name, value) in identity::headers(&credentials.access, betas, stream) {
        request = request.header(name, value);
    }
    request
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

/// Lists the model ids the account can use, newest first.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub async fn list_models(credentials: &Credentials) -> Result<Vec<String>, ProviderError> {
    let betas = betas(
        credentials,
        &ThinkingParams {
            thinking: None,
            effort: None,
            max_tokens: 0,
        },
    );
    let request = authorized(
        reqwest::Client::new().get(MODELS_URL),
        credentials,
        &betas,
        false,
    );
    let response = request.send().await?;
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

/// Serializes the request for `model` to the bytes to send, attested for OAuth.
fn encode(
    model: &str,
    request: &Request<'_>,
    credentials: &Credentials,
    params: &ThinkingParams,
) -> Vec<u8> {
    let body = build_body(model, request, credentials, params);
    let mut bytes = serde_json::to_vec(&body).unwrap_or_default();
    if identity::is_oauth_token(&credentials.access) {
        identity::attest(&mut bytes);
    }
    bytes
}

/// Sends the conversation to Claude using `model` and returns its reply,
/// streaming text and thinking chunks to `sink`.
///
/// A rejection that says the claimed Claude Code version is too old is retried
/// once with the version it asks for.
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
    let oauth = identity::is_oauth_token(&credentials.access);
    let params = thinking_params(model, request.effort);
    let betas = betas(credentials, &params);
    let url = if oauth {
        format!("{URL}?beta=true")
    } else {
        URL.to_string()
    };
    let client = reqwest::Client::new();
    let mut attempt = 0;
    let response = loop {
        let bytes = encode(model, &request, credentials, &params);
        let response = authorized(client.post(&url), credentials, &betas, true)
            .body(bytes)
            .send()
            .await?;
        let status = response.status();
        if status.is_success() {
            break response;
        }
        let body = response.text().await.unwrap_or_default();
        if oauth && attempt == 0 && identity::adopt_required_version(&body) {
            attempt += 1;
            continue;
        }
        return Err(ProviderError::Status {
            status: status.to_string(),
            body,
        });
    };
    let mut events = sse::events(response).await?;
    let mut accumulator = Accumulator::new(oauth);
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

    /// Credentials carrying `access`.
    fn credentials(access: &str) -> Credentials {
        serde_json::from_value(json!({
            "access": access,
            "refresh": "r",
            "expires_at_ms": 0,
            "account_id": "acc-1",
        }))
        .unwrap()
    }

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
                tool_calls: vec![call],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "t1".into(),
                name: "read".into(),
                text: "ok".into(),
                is_error: false,
            }]),
        ];
        let out = messages_json(&messages, false, true);
        assert_eq!(out[1]["content"][0]["type"], "tool_use");
        assert_eq!(out[1]["content"][0]["name"], "read");
        assert_eq!(out[1]["content"][0]["input"]["path"], "a");
        assert_eq!(out[2]["content"][0]["tool_use_id"], "t1");
        let prefixed = messages_json(&messages, true, true);
        assert_eq!(prefixed[1]["content"][0]["name"], "_read");
    }

    /// Tool specs use the `input_schema` field and the prefix when asked.
    #[test]
    fn translates_tool_specs() {
        let spec = ToolSpec {
            name: "read".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        };
        assert_eq!(
            tools_json(std::slice::from_ref(&spec), false)[0]["input_schema"]["type"],
            "object"
        );
        assert_eq!(tools_json(&[spec], true)[0]["name"], "_read");
    }

    /// Adaptive models get adaptive thinking and an output effort.
    #[test]
    fn adaptive_thinking_params() {
        let params = thinking_params("claude-opus-5-5", Some(Effort::High));
        assert_eq!(
            params.thinking,
            Some(json!({ "type": "adaptive", "display": "summarized" }))
        );
        assert_eq!(params.effort, Some(Effort::High));
        let older = thinking_params("claude-sonnet-4-6", Some(Effort::Max));
        assert_eq!(older.thinking, Some(json!({ "type": "adaptive" })));
        assert_eq!(older.effort, Some(Effort::High));
    }

    /// Budget models get a budget below `max_tokens`, and no effort except on Opus 4.5.
    #[test]
    fn budget_thinking_params() {
        let params = thinking_params("claude-sonnet-4-5-20250929", Some(Effort::Medium));
        assert_eq!(
            params.thinking,
            Some(json!({ "type": "enabled", "budget_tokens": 8192 }))
        );
        assert_eq!(params.effort, None);
        assert!(params.max_tokens > 8192);
        let opus = thinking_params("claude-opus-4-5-20251101", Some(Effort::Low));
        assert_eq!(opus.effort, Some(Effort::Low));
        let capped = thinking_params("claude-opus-4-1-20250805", Some(Effort::XHigh));
        let budget = capped.thinking.unwrap()["budget_tokens"].as_u64().unwrap();
        assert!(budget < capped.max_tokens);
    }

    /// No effort or a model that cannot think sends no thinking fields.
    #[test]
    fn no_thinking_when_not_asked_or_unsupported() {
        assert_eq!(thinking_params("claude-opus-5-5", None).thinking, None);
        let old = thinking_params("claude-3-5-haiku-20241022", Some(Effort::High));
        assert_eq!((old.thinking, old.effort), (None, None));
    }

    /// Signed thinking replays first in the assistant message, and only Claude blocks do.
    #[test]
    fn replays_signed_thinking() {
        let reply = Reply {
            text: "ok".into(),
            tool_calls: vec![ToolCall {
                id: "t".into(),
                name: "read".into(),
                arguments: json!({}),
            }],
            opaque: vec![
                json!({ "type": "thinking", "thinking": "hm", "signature": "sig" }),
                json!({ "type": "redacted_thinking", "data": "xyz" }),
                json!({ "type": "reasoning", "encrypted_content": "codex" }),
            ],
            ..Reply::default()
        };
        let out = messages_json(&[Message::Assistant(reply.clone())], false, true);
        let content = out[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 4);
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["type"], "redacted_thinking");
        assert_eq!(content[2]["type"], "text");
        assert_eq!(content[3]["type"], "tool_use");
        let dropped = messages_json(&[Message::Assistant(reply)], false, false);
        assert_eq!(dropped[0]["content"].as_array().unwrap().len(), 2);
    }

    /// An OAuth body has billing and identity blocks, prefixed tools, metadata and no 1M beta.
    #[test]
    fn oauth_body_shape() {
        let messages = [Message::User("fix the bug in main please".into())];
        let tools = [ToolSpec {
            name: "bash".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        }];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &tools,
            effort: Some(Effort::High),
        };
        let creds = credentials("sk-ant-oat01-x");
        let params = thinking_params("claude-opus-5-5", request.effort);
        let body = build_body("claude-opus-5-5", &request, &creds, &params);
        let system = body["system"].as_array().unwrap();
        assert!(
            system[0]["text"]
                .as_str()
                .unwrap()
                .starts_with("x-anthropic-billing-header:")
        );
        assert_eq!(system[1]["text"], identity::IDENTITY);
        assert_eq!(system[2]["text"], "be brief");
        assert_eq!(body["tools"][0]["name"], "_bash");
        let user_id: Value =
            serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
        assert_eq!(user_id["account_uuid"], "acc-1");
        assert_eq!(body["output_config"]["effort"], "high");
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert!(body["context_management"].is_object());
        let bytes = encode("claude-opus-5-5", &request, &creds, &params);
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("cch=00000"));
        assert!(text.contains("cch="));
        let used = betas(&creds, &params);
        assert!(used.contains(&"effort-2025-11-24"));
        assert!(!used.iter().any(|beta| beta.starts_with("context-1m")));
    }

    /// An API-key body has neither the identity blocks, the prefix nor metadata.
    #[test]
    fn api_key_body_is_plain() {
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &[],
            effort: None,
        };
        let creds = credentials("sk-ant-api03-x");
        let params = thinking_params("claude-haiku-5-5", None);
        let body = build_body("claude-haiku-5-5", &request, &creds, &params);
        assert_eq!(body["system"].as_array().unwrap().len(), 1);
        assert!(body.get("tools").is_none());
        assert!(body.get("metadata").is_none());
        assert!(body.get("thinking").is_none());
    }

    /// Feeds each of `events` into a fresh accumulator and returns the reply.
    fn run(events: &[Value], prefixed: bool, sink: &mut Recorder) -> Reply {
        let mut accumulator = Accumulator::new(prefixed);
        for event in events {
            if accumulator.feed(&event.to_string(), sink).unwrap() {
                break;
            }
        }
        accumulator.finish()
    }

    /// Thinking, signature, text and tool deltas become sink chunks and reply fields.
    #[test]
    fn parses_thinking_text_and_tool_stream() {
        let events = [
            json!({ "type": "message_start", "message": { "usage": {
                "input_tokens": 12, "output_tokens": 1,
                "cache_creation_input_tokens": 5, "cache_read_input_tokens": 7 } } }),
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "thinking", "thinking": "", "signature": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "let me " } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "think" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "signature_delta", "signature": "SIG" } }),
            json!({ "type": "content_block_stop", "index": 0 }),
            json!({ "type": "content_block_start", "index": 1,
                "content_block": { "type": "redacted_thinking", "data": "RED" } }),
            json!({ "type": "content_block_start", "index": 2,
                "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 2,
                "delta": { "type": "text_delta", "text": "hello" } }),
            json!({ "type": "content_block_start", "index": 3,
                "content_block": { "type": "tool_use", "id": "tu1", "name": "_read" } }),
            json!({ "type": "content_block_delta", "index": 3,
                "delta": { "type": "input_json_delta", "partial_json": "{\"path\":" } }),
            json!({ "type": "content_block_delta", "index": 3,
                "delta": { "type": "input_json_delta", "partial_json": "\"a\"}" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "tool_use" },
                "usage": { "output_tokens": 40 } }),
            json!({ "type": "message_stop" }),
        ];
        let mut sink = Recorder::default();
        let reply = run(&events, true, &mut sink);
        assert_eq!(sink.thinking, ["let me ", "think"]);
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        assert_eq!(sink.text, ["hello"]);
        assert_eq!(reply.thinking, "let me think");
        assert_eq!(reply.text, "hello");
        assert_eq!(reply.opaque.len(), 2);
        assert_eq!(reply.opaque[0]["signature"], "SIG");
        assert_eq!(reply.opaque[0]["thinking"], "let me think");
        assert_eq!(reply.opaque[1]["data"], "RED");
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(reply.tool_calls[0].arguments["path"], "a");
        assert_eq!(
            reply.tokens,
            Some(Tokens {
                input: 12,
                output: 40,
                cache_read: 7,
                cache_write: 5,
            })
        );
    }

    /// An unsigned thinking block is not kept for replay, and usage is optional.
    #[test]
    fn unsigned_thinking_is_not_replayed() {
        let events = [
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "thinking" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "thinking_delta", "thinking": "x" } }),
            json!({ "type": "message_stop" }),
        ];
        let reply = run(&events, false, &mut Recorder::default());
        assert!(reply.opaque.is_empty());
        assert_eq!(reply.thinking, "x");
        assert_eq!(reply.tokens, None);
    }

    /// An error event surfaces as a stream error.
    #[test]
    fn error_events_fail_the_stream() {
        let mut accumulator = Accumulator::new(false);
        let event = json!({ "type": "error", "error": { "message": "overloaded" } });
        let error = accumulator
            .feed(&event.to_string(), &mut Recorder::default())
            .unwrap_err();
        assert!(error.to_string().contains("overloaded"));
    }
}
