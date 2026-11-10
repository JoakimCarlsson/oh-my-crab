//! Streaming messages against the Anthropic Messages API, on Anthropic's own host or as Amazon Bedrock and Google Vertex AI serve it.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::chat::{Blocks, parse_arguments};
use crate::cloud::eventstream::Decoder;
use crate::cloud::{aws, bedrock, google, sigv4, vertex};
use crate::identity;
use crate::models::{
    ClaudeModel, ThinkingMode, claude_efforts, claude_max_output, claude_supports_display,
    claude_thinking_mode, parse_claude,
};
use crate::{
    Effort, Message, Part, Provider, ProviderError, Reply, Request, Retention, Sink, Stop, Tokens,
    ToolCall, ToolSpec, Wire, sse,
};

/// The Messages API version Anthropic's own host is asked for.
const API_VERSION: &str = "2023-06-01";

/// Where a Messages request goes, which decides how it is addressed, authorized and framed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Host {
    /// Anthropic's own API.
    Anthropic {
        /// The API's address, such as `https://api.anthropic.com`.
        base_url: String,
        /// Whether requests present themselves as Claude Code, as subscription logins must.
        identity: bool,
    },
    /// Amazon Bedrock.
    Bedrock {
        /// The AWS region.
        region: String,
    },
    /// Google Vertex AI.
    Vertex {
        /// The Google Cloud region, or `global`.
        region: String,
    },
}

impl Host {
    /// The host requests of `provider` made with `credentials` go to.
    ///
    /// Only the `claude` provider with a subscription token presents the Claude Code identity; an API key never does.
    pub(crate) fn of(provider: Provider, credentials: &Credentials) -> Self {
        match provider.wire() {
            Wire::Bedrock => Host::Bedrock {
                region: aws::region(),
            },
            Wire::Vertex => Host::Vertex {
                region: google::region(),
            },
            _ => Host::Anthropic {
                base_url: provider.def().base_url(),
                identity: provider == Provider::Claude
                    && identity::is_oauth_token(&credentials.access),
            },
        }
    }

    /// Whether requests present themselves as Claude Code.
    fn identity(&self) -> bool {
        matches!(self, Host::Anthropic { identity: true, .. })
    }

    /// The lifetime cache entries are asked for when `retention` is wanted: as wanted on Anthropic's own host, and the default lifetime on the clouds, which crab does not ask for longer.
    fn retention(&self, retention: Retention) -> Retention {
        match self {
            Host::Anthropic { .. } => retention,
            Host::Bedrock { .. } | Host::Vertex { .. } => Retention::Short,
        }
    }
}

/// What the reader is told when the model declined to answer.
const REFUSAL: &str = "the model declined to answer this request";

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
    /// Why the model stopped, carried by the `message_delta` event.
    stop_reason: Option<String>,
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
    /// Why the model stopped, once the stream said.
    stop: Stop,
    /// Whether the stream reached its `message_stop` event.
    ended: bool,
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
            stop: Stop::Finished,
            ended: false,
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
    /// Returns [`ProviderError::Stream`] for an `error` event, and for a reply the model stopped because it declined to answer, which would otherwise pass for a short or empty answer.
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
                match payload.delta.and_then(|delta| delta.stop_reason).as_deref() {
                    Some("max_tokens" | "model_context_window_exceeded") => {
                        self.stop = Stop::Length;
                    }
                    Some("refusal") => {
                        return Err(ProviderError::Stream(REFUSAL.to_owned()));
                    }
                    _ => {}
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
            "message_stop" => {
                self.ended = true;
                return Ok(true);
            }
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
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Unfinished`] when the stream ended before its `message_stop` event, since the reply may then be cut short anywhere.
    pub(crate) fn finish(self) -> Result<Reply, ProviderError> {
        if !self.ended {
            return Err(ProviderError::Unfinished);
        }
        Ok(self.reply())
    }

    /// The reply the events so far describe.
    fn reply(self) -> Reply {
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
            stop: self.stop,
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
    let limit = claude_max_output(model);
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
/// `prefixed` is set. A compaction is left out: the agent gives the model its
/// summary as a user message in its place. So is a pruning, which the agent
/// applies to the tool results it gives.
pub(crate) fn messages_json(messages: &[Message], prefixed: bool, replay: bool) -> Vec<Value> {
    let wire_name = |name: &str| {
        if prefixed {
            identity::prefix_tool(name)
        } else {
            name.to_string()
        }
    };
    let mut out: Vec<Value> = Vec::new();
    for message in messages {
        if let Message::Images(images) = message {
            attach_images(&mut out, images);
            continue;
        }
        out.extend(match message {
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
                            "content": result_content(result),
                            "is_error": result.is_error,
                        })
                    })
                    .collect();
                Some(json!({ "role": "user", "content": blocks }))
            }
            Message::Compaction(_) | Message::Pruning(_) | Message::Images(_) => None,
        });
    }
    out
}

/// Adds `images`, which the user attached to a prompt, to the end of `messages` as `image` blocks: into the last message when it is the user's, so the prompt and its images are one message, and as a user message of their own otherwise.
fn attach_images(messages: &mut Vec<Value>, images: &[crate::Image]) {
    let blocks: Vec<Value> = images.iter().map(image_block).collect();
    if blocks.is_empty() {
        return;
    }
    match messages.last_mut() {
        Some(last) if last["role"] == "user" => {
            if let Some(text) = last["content"].as_str().map(str::to_string) {
                last["content"] = json!([{ "type": "text", "text": text }]);
            }
            if let Some(content) = last["content"].as_array_mut() {
                content.extend(blocks);
            }
        }
        _ => messages.push(json!({ "role": "user", "content": blocks })),
    }
}

/// The `image` block that carries `image`.
fn image_block(image: &crate::Image) -> Value {
    json!({
        "type": "image",
        "source": {
            "type": "base64",
            "media_type": image.media_type,
            "data": image.data,
        },
    })
}

/// The `content` of the `tool_result` block for `result`: its text alone, or a
/// text block followed by one `image` block per image when it carries images.
fn result_content(result: &crate::ToolResult) -> Value {
    if result.images.is_empty() {
        return Value::String(result.text.clone());
    }
    let text = (!result.text.is_empty()).then(|| json!({ "type": "text", "text": result.text }));
    let images = result.images.iter().map(image_block);
    Value::Array(text.into_iter().chain(images).collect())
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

/// How a request is sent: whether it writes to the prompt cache and for how long, and whether it only warms the cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Form {
    /// How long the cache entries the request writes live, or nothing to mark no cache breakpoint at all.
    pub cache: Option<Retention>,
    /// Whether the request asks for no output and is not streamed, which only reads or writes the cache.
    pub warm: bool,
}

impl Form {
    /// A streamed request whose cache entries live as `cache` says, or that writes none when it is nothing.
    pub(crate) const fn streamed(cache: Option<Retention>) -> Self {
        Self { cache, warm: false }
    }
}

/// The `cache_control` value of a breakpoint whose entry lives as `retention` says.
fn breakpoint(retention: Retention) -> Value {
    match retention {
        Retention::Short => json!({ "type": "ephemeral" }),
        Retention::Long => json!({ "type": "ephemeral", "ttl": "1h" }),
    }
}

/// Marks the last content block of the last message as the cache breakpoint `marker`.
fn mark_last_message(messages: &mut [Value], marker: &Value) {
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
        block["cache_control"] = marker.clone();
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

/// Builds the request body sent in `form` to `host`, with the billing placeholder still in place for the Claude Code identity.
///
/// Every breakpoint gets the same lifetime, since the API refuses a longer-lived entry after a shorter-lived one. Bedrock and Vertex get the body their `anthropic_version` names, Bedrock with its betas in it.
pub(crate) fn build_body(
    model: &str,
    request: &Request<'_>,
    credentials: &Credentials,
    params: &ThinkingParams,
    form: Form,
    host: &Host,
) -> Value {
    let oauth = host.identity();
    let parsed = parse_claude(model);
    let marker = form.cache.map(breakpoint);
    let mut messages = messages_json(request.messages, oauth, replays_thinking(&parsed, params));
    if let Some(marker) = &marker {
        mark_last_message(&mut messages, marker);
    }
    let mut system = Vec::new();
    if oauth {
        system.push(json!({
            "type": "text",
            "text": identity::billing_header(first_user_text(request.messages)),
        }));
        let mut block = json!({ "type": "text", "text": identity::IDENTITY });
        if let Some(marker) = &marker {
            block["cache_control"] = marker.clone();
        }
        system.push(block);
    }
    if !request.system.trim().is_empty() {
        let mut block = json!({ "type": "text", "text": request.system });
        if let Some(marker) = &marker {
            block["cache_control"] = marker.clone();
        }
        system.push(block);
    }
    let mut body = json!({
        "model": model,
        "max_tokens": if form.warm { 0 } else { params.max_tokens },
        "stream": !form.warm,
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
    match host {
        Host::Anthropic { .. } => {}
        Host::Bedrock { .. } => bedrock::adapt_body(&mut body, &betas(oauth, params)),
        Host::Vertex { .. } => vertex::adapt_body(&mut body),
    }
    body
}

/// The `anthropic-beta` values for a request with `params`, with the Claude Code identity's own when `identity` says.
///
/// Without the identity only the standard betas are sent: interleaved thinking for a thinking budget, and the effort beta for an effort.
fn betas(identity: bool, params: &ThinkingParams) -> Vec<&'static str> {
    if identity {
        identity::oauth_betas(params.thinking.is_some() || params.effort.is_some())
    } else {
        let budget = params
            .thinking
            .as_ref()
            .is_some_and(|thinking| thinking["type"] == "enabled");
        identity::api_key_betas(budget, params.effort.is_some())
    }
}

/// The headers of a request to Anthropic's own API with the API key `key`, as names and values: the key, the API version, `betas`, and crab's own name.
pub(crate) fn key_headers(key: &str, betas: &[&str], stream: bool) -> Vec<(String, String)> {
    let accept = if stream {
        "text/event-stream"
    } else {
        "application/json"
    };
    let mut headers = vec![
        ("accept".to_owned(), accept.to_owned()),
        ("content-type".to_owned(), "application/json".to_owned()),
        ("anthropic-version".to_owned(), API_VERSION.to_owned()),
        (
            "user-agent".to_owned(),
            concat!("oh-my-crab/", env!("CARGO_PKG_VERSION")).to_owned(),
        ),
        ("x-api-key".to_owned(), key.to_owned()),
    ];
    if !betas.is_empty() {
        headers.push(("anthropic-beta".to_owned(), betas.join(",")));
    }
    headers
}

/// Adds the headers of a request to Anthropic's own API to `request`: the Claude Code identity's when `identity` says, else those of an API key.
fn authorized(
    mut request: reqwest::RequestBuilder,
    credentials: &Credentials,
    identity: bool,
    betas: &[&str],
    stream: bool,
) -> reqwest::RequestBuilder {
    let headers = if identity {
        identity::headers(&credentials.access, betas)
    } else {
        key_headers(&credentials.access, betas, stream)
    };
    for (name, value) in headers {
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

/// Lists the model ids `provider` can use with `credentials`: those Anthropic's API lists, newest first, or the Claude models the catalog lists for Bedrock and Vertex.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub async fn list_models(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Vec<String>, ProviderError> {
    let (base_url, identity) = match Host::of(provider, credentials) {
        Host::Bedrock { region } => return Ok(bedrock::models(&region)),
        Host::Vertex { .. } => return Ok(vertex::models()),
        Host::Anthropic { base_url, identity } => (base_url, identity),
    };
    let betas = betas(
        identity,
        &ThinkingParams {
            thinking: None,
            effort: None,
            max_tokens: 0,
        },
    );
    let request = authorized(
        reqwest::Client::new().get(format!("{base_url}/v1/models")),
        credentials,
        identity,
        &betas,
        false,
    );
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::from_response(response).await);
    }
    let list: ModelList = response.json().await?;
    Ok(list.data.into_iter().map(|entry| entry.id).collect())
}

/// Serializes the request for `model` sent in `form` to `host` to the bytes to send, attested for the Claude Code identity.
fn encode(
    model: &str,
    request: &Request<'_>,
    credentials: &Credentials,
    params: &ThinkingParams,
    form: Form,
    host: &Host,
) -> Vec<u8> {
    let body = build_body(model, request, credentials, params, form, host);
    let mut bytes = serde_json::to_vec(&body).unwrap_or_default();
    if host.identity() {
        identity::attest(&mut bytes);
    }
    bytes
}

/// The request carrying `bytes` for `model` to `host`, addressed and authorized as the host wants, streamed unless `form` warms the cache.
///
/// # Errors
///
/// Returns [`ProviderError::Credentials`] when a cloud's credentials or project cannot be found or used.
async fn prepare(
    client: &reqwest::Client,
    host: &Host,
    credentials: &Credentials,
    model: &str,
    params: &ThinkingParams,
    form: Form,
    bytes: Vec<u8>,
) -> Result<reqwest::RequestBuilder, ProviderError> {
    let stream = !form.warm;
    let request = match host {
        Host::Anthropic { base_url, identity } => {
            let url = if *identity {
                format!("{base_url}/v1/messages?beta=true")
            } else {
                format!("{base_url}/v1/messages")
            };
            authorized(
                client.post(url),
                credentials,
                *identity,
                &betas(*identity, params),
                stream,
            )
        }
        Host::Bedrock { region } => {
            let authorization = bedrock::Authorization::find(&credentials.access)?;
            let url = bedrock::invoke_url(&bedrock::base_url(region), model, stream);
            let headers =
                bedrock::headers(&url, &bytes, stream, region, &authorization, &sigv4::now())?;
            headers
                .into_iter()
                .fold(client.post(url), |request, (name, value)| {
                    request.header(name, value)
                })
        }
        Host::Vertex { region } => {
            let project = google::project().map_err(ProviderError::Credentials)?;
            let token = google::access_token()
                .await
                .map_err(ProviderError::Credentials)?;
            let url =
                vertex::predict_url(&vertex::base_url(region), &project, region, model, stream);
            let mut request = client
                .post(url)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json");
            let betas = betas(false, params);
            if !betas.is_empty() {
                request = request.header("anthropic-beta", betas.join(","));
            }
            request
        }
    };
    Ok(request.body(bytes))
}

/// Sends the request for `model` in `form` to `host` and returns the successful response.
///
/// A rejection that says the claimed Claude Code version is too old is retried
/// once with the version it asks for.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status, or cloud credentials that cannot be found.
async fn send(
    host: &Host,
    credentials: &Credentials,
    model: &str,
    request: &Request<'_>,
    params: &ThinkingParams,
    form: Form,
) -> Result<reqwest::Response, ProviderError> {
    let oauth = host.identity();
    let client = reqwest::Client::new();
    let mut attempt = 0;
    loop {
        let bytes = encode(model, request, credentials, params, form, host);
        let response = prepare(&client, host, credentials, model, params, form, bytes)
            .await?
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(response);
        }
        let error = ProviderError::from_response(response).await;
        if let ProviderError::Status { body, .. } = &error
            && oauth
            && attempt == 0
            && identity::adopt_required_version(body)
        {
            attempt += 1;
            continue;
        }
        return Err(error);
    }
}

/// Sends the conversation to Claude on `provider` using `model` and returns its reply,
/// streaming text and thinking chunks to `sink`; the cache entries the request
/// writes live as `cache` says, and it writes none when that is nothing.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status, cloud
/// credentials that cannot be found, or an error event inside the stream.
pub async fn complete(
    provider: Provider,
    credentials: &Credentials,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
    cache: Option<Retention>,
) -> Result<Reply, ProviderError> {
    let host = Host::of(provider, credentials);
    let params = thinking_params(model, request.effort);
    let form = Form::streamed(cache.map(|retention| host.retention(retention)));
    let response = send(&host, credentials, model, &request, &params, form).await?;
    let mut accumulator = Accumulator::new(host.identity());
    if matches!(host, Host::Bedrock { .. }) {
        read_event_stream(response, &mut accumulator, sink).await?;
    } else {
        let mut events = sse::events(response).await?;
        while let Some(event) = events.next().await {
            if accumulator.feed(&event?.data, sink)? {
                break;
            }
        }
    }
    accumulator.finish()
}

/// Feeds the Messages events Bedrock's binary event stream in `response` carries to `accumulator`, until the message is complete or the stream ends.
///
/// # Errors
///
/// Returns [`ProviderError`] when the connection fails, the stream is damaged, or it carries an exception or an error event.
async fn read_event_stream(
    response: reqwest::Response,
    accumulator: &mut Accumulator,
    sink: &mut dyn Sink,
) -> Result<(), ProviderError> {
    let mut bytes = response.bytes_stream();
    let mut decoder = Decoder::default();
    while let Some(chunk) = bytes.next().await {
        decoder.push(&chunk?);
        while let Some(message) = decoder.next().map_err(ProviderError::Stream)? {
            if let Some(data) = bedrock::event_data(&message)?
                && accumulator.feed(&data, sink)?
            {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// The body of an answer that is not streamed, of which only the usage is read.
#[derive(Deserialize)]
struct Answered {
    /// What the request cost.
    usage: Option<Usage>,
}

/// Sends `request` to `model` again asking for no output and without streaming, which reads the cache entries it wrote and so keeps them alive for another lifetime of `retention`; returns what it cost.
///
/// Nothing is sent, and nothing returned, when the request thinks within a token budget: a request with no output cannot keep that budget, and the cache is keyed on it. Nor is anything sent to Bedrock or Vertex, which are not known to take a request with no output.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or a body that is not JSON.
pub async fn warm(
    provider: Provider,
    credentials: &Credentials,
    model: &str,
    request: Request<'_>,
    retention: Retention,
) -> Result<Option<Tokens>, ProviderError> {
    let host = Host::of(provider, credentials);
    if !matches!(host, Host::Anthropic { .. }) {
        return Ok(None);
    }
    let params = thinking_params(model, request.effort);
    if params
        .thinking
        .as_ref()
        .is_some_and(|thinking| thinking["type"] == "enabled")
    {
        return Ok(None);
    }
    let form = Form {
        cache: Some(retention),
        warm: true,
    };
    let response = send(&host, credentials, model, &request, &params, form).await?;
    let answered: Answered = response.json().await?;
    Ok(Some(tokens_of(answered.usage.unwrap_or_default())))
}

/// The token counts `usage` reports, with what it leaves out as zero.
fn tokens_of(usage: Usage) -> Tokens {
    Tokens {
        input: usage.input_tokens.unwrap_or(0),
        output: usage.output_tokens.unwrap_or(0),
        cache_read: usage.cache_read_input_tokens.unwrap_or(0),
        cache_write: usage.cache_creation_input_tokens.unwrap_or(0),
    }
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

    /// Anthropic's own host with the Claude Code identity, as a subscription login reaches it.
    fn subscription() -> Host {
        Host::Anthropic {
            base_url: "https://api.anthropic.com".into(),
            identity: true,
        }
    }

    /// Anthropic's own host without the identity, as an API key reaches it.
    fn api_key() -> Host {
        Host::Anthropic {
            base_url: "https://api.anthropic.com".into(),
            identity: false,
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
                images: Vec::new(),
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

    /// Images attached to a prompt join the prompt's user message, after its text, so the model sees them with it.
    #[test]
    fn attached_images_join_their_prompt() {
        let out = messages_json(
            &[
                Message::User("look".into()),
                Message::Images(pictured().images),
            ],
            false,
            true,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["role"], "user");
        assert_eq!(
            out[0]["content"][0],
            json!({ "type": "text", "text": "look" })
        );
        assert_eq!(out[0]["content"][1]["type"], "image");
        assert_eq!(out[0]["content"][1]["source"]["data"], "AAAA");
    }

    /// A result with images becomes a text block followed by base64 image blocks.
    #[test]
    fn translates_tool_result_images() {
        let out = messages_json(&[Message::ToolResults(vec![pictured()])], false, true);
        let content = &out[0]["content"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "[image]");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert_eq!(content[1]["source"]["data"], "AAAA");
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
        let older = thinking_params("claude-sonnet-4-6", Some(Effort::XHigh));
        assert_eq!(older.thinking, Some(json!({ "type": "adaptive" })));
        assert_eq!(older.effort, Some(Effort::High));
        let capped = thinking_params("claude-opus-4-5-20251101", Some(Effort::Max));
        assert_eq!(capped.effort, Some(Effort::High));
    }

    /// The output limit comes from the catalog where its generation would suggest less, and Bedrock and Vertex forms of an id read as the id.
    #[test]
    fn the_output_limit_comes_from_the_catalog() {
        assert_eq!(
            thinking_params("claude-sonnet-4-6", None).max_tokens,
            128_000
        );
        assert_eq!(
            thinking_params("us.anthropic.claude-sonnet-4-5-20250929-v1:0", None).max_tokens,
            64_000
        );
        assert_eq!(
            thinking_params("claude-opus-4-5@20251101", Some(Effort::Low)).effort,
            Some(Effort::Low)
        );
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
        let short = Form::streamed(Some(Retention::Short));
        let body = build_body(
            "claude-opus-5-5",
            &request,
            &creds,
            &params,
            short,
            &subscription(),
        );
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
        let bytes = encode(
            "claude-opus-5-5",
            &request,
            &creds,
            &params,
            short,
            &subscription(),
        );
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("cch=00000"));
        assert!(text.contains("cch="));
        let used = betas(true, &params);
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
        let body = build_body(
            "claude-haiku-5-5",
            &request,
            &creds,
            &params,
            Form::streamed(Some(Retention::Short)),
            &api_key(),
        );
        assert_eq!(body["system"].as_array().unwrap().len(), 1);
        assert!(body.get("tools").is_none());
        assert!(body.get("metadata").is_none());
        assert!(body.get("thinking").is_none());
    }

    /// Only the `claude` provider with a subscription token presents the identity; the Anthropic API provider never does, even with such a token, and goes to its own address.
    #[test]
    fn hosts_follow_the_provider() {
        let oauth = credentials("sk-ant-oat01-x");
        assert!(Host::of(Provider::Claude, &oauth).identity());
        assert!(!Host::of(Provider::Claude, &credentials("sk-ant-api03-x")).identity());
        let anthropic = Host::of(Provider::Anthropic, &oauth);
        assert!(!anthropic.identity());
        if std::env::var_os("OMC_ANTHROPIC_URL").is_none() {
            assert_eq!(anthropic, api_key());
        }
        assert!(matches!(
            Host::of(Provider::Bedrock, &oauth),
            Host::Bedrock { .. }
        ));
        assert!(matches!(
            Host::of(Provider::Vertex, &oauth),
            Host::Vertex { .. }
        ));
        assert_eq!(api_key().retention(Retention::Long), Retention::Long);
        let vertex = Host::Vertex {
            region: "global".into(),
        };
        assert_eq!(vertex.retention(Retention::Long), Retention::Short);
    }

    /// An API key goes in `x-api-key` with crab's own name and the standard betas, without any Claude Code header.
    #[test]
    fn api_key_headers_carry_no_identity() {
        let params = thinking_params("claude-sonnet-4-5-20250929", Some(Effort::Medium));
        let used = betas(false, &params);
        assert_eq!(used, ["interleaved-thinking-2025-05-14"]);
        let headers = key_headers("sk-ant-api03-abc", &used, true);
        let header = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(name))
                .map(|(_, value)| value.as_str())
        };
        assert_eq!(header("x-api-key"), Some("sk-ant-api03-abc"));
        assert_eq!(header("anthropic-version"), Some("2023-06-01"));
        assert_eq!(header("accept"), Some("text/event-stream"));
        assert_eq!(
            header("anthropic-beta"),
            Some("interleaved-thinking-2025-05-14")
        );
        assert!(header("user-agent").unwrap().starts_with("oh-my-crab/"));
        for absent in [
            "authorization",
            "x-app",
            "anthropic-dangerous-direct-browser-access",
            "x-claude-code-session-id",
            "x-stainless-lang",
        ] {
            assert_eq!(header(absent), None, "{absent}");
        }
        assert!(
            !used
                .iter()
                .any(|beta| beta.starts_with("claude-code") || beta.starts_with("oauth"))
        );
    }

    /// Bedrock and Vertex bodies carry the plain Messages body with their API version, cache breakpoints and thinking intact, and no identity.
    #[test]
    fn cloud_bodies_keep_caching_and_thinking() {
        let messages = [Message::User("hi".into())];
        let tools = [ToolSpec {
            name: "bash".into(),
            description: "d".into(),
            schema: json!({ "type": "object" }),
        }];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &tools,
            effort: Some(Effort::Medium),
        };
        let creds = credentials("");
        let model = "us.anthropic.claude-sonnet-4-5-20250929-v1:0";
        let params = thinking_params(model, request.effort);
        let short = Form::streamed(Some(Retention::Short));
        let bedrock = build_body(
            model,
            &request,
            &creds,
            &params,
            short,
            &Host::Bedrock {
                region: "us-east-1".into(),
            },
        );
        assert!(bedrock.get("model").is_none() && bedrock.get("stream").is_none());
        assert_eq!(bedrock["anthropic_version"], "bedrock-2023-05-31");
        assert_eq!(
            bedrock["anthropic_beta"],
            json!(["interleaved-thinking-2025-05-14"])
        );
        assert_eq!(bedrock["thinking"]["type"], "enabled");
        assert_eq!(bedrock["tools"][0]["name"], "bash");
        assert_eq!(breakpoints(&bedrock).len(), 2);
        assert!(bedrock.get("metadata").is_none());
        let vertex_params = thinking_params("claude-opus-5-5", Some(Effort::High));
        let vertex = build_body(
            "claude-opus-5-5",
            &request,
            &creds,
            &vertex_params,
            short,
            &Host::Vertex {
                region: "global".into(),
            },
        );
        assert!(vertex.get("model").is_none());
        assert_eq!(vertex["stream"], true);
        assert_eq!(vertex["anthropic_version"], "vertex-2023-10-16");
        assert_eq!(vertex["thinking"]["type"], "adaptive");
        assert_eq!(vertex["output_config"]["effort"], "high");
        assert!(vertex.get("anthropic_beta").is_none());
        assert_eq!(vertex["system"].as_array().unwrap().len(), 1);
    }

    /// The `cache_control` values of every block of `body` that carries one, system blocks first.
    fn breakpoints(body: &Value) -> Vec<Value> {
        let system = body["system"].as_array().into_iter().flatten();
        let messages = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|message| message["content"].as_array())
            .flatten();
        system
            .chain(messages)
            .filter_map(|block| block.get("cache_control").cloned())
            .collect()
    }

    /// The hour-long lifetime marks every breakpoint with it, an uncached request marks none, and a warm request asks for no output without streaming.
    #[test]
    fn cache_forms_shape_the_body() {
        let messages = [
            Message::User("a".into()),
            Message::Assistant(Reply {
                text: "b".into(),
                ..Reply::default()
            }),
            Message::User("c".into()),
        ];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &[],
            effort: Some(Effort::High),
        };
        let creds = credentials("sk-ant-oat01-x");
        let params = thinking_params("claude-opus-5-5", request.effort);
        let body = |form| {
            build_body(
                "claude-opus-5-5",
                &request,
                &creds,
                &params,
                form,
                &subscription(),
            )
        };
        let short = body(Form::streamed(Some(Retention::Short)));
        assert_eq!(breakpoints(&short), vec![json!({ "type": "ephemeral" }); 3]);
        let long = body(Form::streamed(Some(Retention::Long)));
        assert_eq!(
            breakpoints(&long),
            vec![json!({ "type": "ephemeral", "ttl": "1h" }); 3]
        );
        let uncached = body(Form::streamed(None));
        assert!(breakpoints(&uncached).is_empty());
        assert_eq!(uncached["messages"][2]["content"], "c");
        let warm = body(Form {
            cache: Some(Retention::Short),
            warm: true,
        });
        assert_eq!(
            (warm["max_tokens"].clone(), warm["stream"].clone()),
            (json!(0), json!(false))
        );
        let mut same = warm.clone();
        same["max_tokens"] = short["max_tokens"].clone();
        same["stream"] = json!(true);
        assert_eq!(same, short);
    }

    /// Usage the API leaves out counts as zero.
    #[test]
    fn warm_usage_fills_gaps() {
        let usage: Usage =
            serde_json::from_value(json!({ "cache_read_input_tokens": 900, "input_tokens": 3 }))
                .unwrap();
        assert_eq!(
            tokens_of(usage),
            Tokens {
                input: 3,
                output: 0,
                cache_read: 900,
                cache_write: 0,
            }
        );
    }

    /// Feeds each of `events` into a fresh accumulator and returns how the stream ended.
    fn ended(
        events: &[Value],
        prefixed: bool,
        sink: &mut Recorder,
    ) -> Result<Reply, ProviderError> {
        let mut accumulator = Accumulator::new(prefixed);
        for event in events {
            if accumulator.feed(&event.to_string(), sink).unwrap() {
                break;
            }
        }
        accumulator.finish()
    }

    /// Feeds each of `events` into a fresh accumulator and returns the reply.
    fn run(events: &[Value], prefixed: bool, sink: &mut Recorder) -> Reply {
        ended(events, prefixed, sink).unwrap()
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

    /// A reply the output limit cut off says so, and one that ends normally does not.
    #[test]
    fn the_output_limit_is_reported() {
        let cut = [
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "half a" } }),
            json!({ "type": "message_delta", "delta": { "stop_reason": "max_tokens" } }),
            json!({ "type": "message_stop" }),
        ];
        assert_eq!(
            run(&cut, false, &mut Recorder::default()).stop,
            Stop::Length
        );
        let whole = [
            json!({ "type": "message_delta", "delta": { "stop_reason": "end_turn" } }),
            json!({ "type": "message_stop" }),
        ];
        assert_eq!(
            run(&whole, false, &mut Recorder::default()).stop,
            Stop::Finished
        );
    }

    /// A reply the context window cut off counts as cut by the output limit.
    #[test]
    fn a_full_context_window_is_a_cut_reply() {
        let cut = [
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "half a" } }),
            json!({ "type": "message_delta",
                "delta": { "stop_reason": "model_context_window_exceeded" } }),
            json!({ "type": "message_stop" }),
        ];
        assert_eq!(
            run(&cut, false, &mut Recorder::default()).stop,
            Stop::Length
        );
    }

    /// A reply the model declined to give is an error that is not sent again, not an empty answer.
    #[test]
    fn a_refusal_is_an_error() {
        let mut accumulator = Accumulator::new(false);
        let event = json!({ "type": "message_delta", "delta": { "stop_reason": "refusal" } });
        let error = accumulator
            .feed(&event.to_string(), &mut Recorder::default())
            .unwrap_err();
        assert!(error.to_string().contains("declined"), "{error}");
        assert!(!error.is_transient());
        assert!(!error.is_context_overflow());
    }

    /// A stream that ends before `message_stop` is unfinished rather than a short reply.
    #[test]
    fn a_stream_without_its_stop_is_unfinished() {
        let events = [
            json!({ "type": "content_block_start", "index": 0,
                "content_block": { "type": "text", "text": "" } }),
            json!({ "type": "content_block_delta", "index": 0,
                "delta": { "type": "text_delta", "text": "half" } }),
        ];
        let error = ended(&events, false, &mut Recorder::default()).unwrap_err();
        assert!(matches!(error, ProviderError::Unfinished), "{error}");
    }
}
