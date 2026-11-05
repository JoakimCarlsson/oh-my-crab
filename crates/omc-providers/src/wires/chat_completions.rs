//! Streaming chat against any server that speaks the Chat Completions API.

use futures_util::StreamExt;
use omc_auth::Credentials;
use serde_json::{Value, json};

use crate::chat::{Blocks, parse_arguments};
use crate::{
    Message, Part, Provider, ProviderError, Reply, Request, Sink, Tokens, ToolCall, ToolSpec, sse,
};

/// Where a request goes and who it is sent as.
#[derive(Clone, Copy)]
pub(crate) struct Target<'a> {
    /// The login's credentials.
    pub credentials: &'a Credentials,
    /// The API address, without a trailing slash.
    pub base_url: &'a str,
    /// Headers sent on every request.
    pub headers: &'a [(&'a str, &'a str)],
}

/// Returns the API address for `provider`: the one assigned to the account when there is one, else the provider's own.
pub(crate) fn base_url(provider: Provider, credentials: &Credentials) -> String {
    credentials
        .base_url
        .clone()
        .unwrap_or_else(|| provider.def().base_url())
}

/// Adds the provider's headers and the bearer token to `request`.
fn authorized(request: reqwest::RequestBuilder, target: &Target<'_>) -> reqwest::RequestBuilder {
    let mut request = request;
    for (name, value) in target.headers {
        request = request.header(*name, *value);
    }
    if target.credentials.access.is_empty() {
        return request;
    }
    request.bearer_auth(&target.credentials.access)
}

/// Picks the model ids out of a model list response, dropping entries the server marks as not for chat.
pub(crate) fn model_ids(list: &Value) -> Vec<String> {
    list.get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            let chat = entry
                .pointer("/capabilities/type")
                .and_then(Value::as_str)
                .is_none_or(|kind| kind == "chat");
            let picked = entry
                .get("model_picker_enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true);
            chat && picked
        })
        .filter_map(|entry| entry.get("id").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// Lists the model ids the server offers, in the order it gives them.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(crate) async fn list_models(target: Target<'_>) -> Result<Vec<String>, ProviderError> {
    let request = reqwest::Client::new().get(format!("{}/models", target.base_url));
    let response = authorized(request, &target).send().await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::Status {
            status: status.to_string(),
            body,
        });
    }
    Ok(model_ids(&response.json().await?))
}

/// Converts the conversation into the `messages` array, system prompt first.
pub(crate) fn messages_json(system: &str, messages: &[Message]) -> Vec<Value> {
    let mut out = vec![json!({"role": "system", "content": system})];
    for message in messages {
        match message {
            Message::User(text) => out.push(json!({"role": "user", "content": text})),
            Message::Assistant(reply) => {
                let calls: Vec<Value> = reply
                    .tool_calls
                    .iter()
                    .map(|call| {
                        let arguments = match &call.arguments {
                            Value::String(raw) => raw.clone(),
                            other => other.to_string(),
                        };
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": {"name": call.name, "arguments": arguments},
                        })
                    })
                    .collect();
                let content = if reply.text.is_empty() {
                    Value::Null
                } else {
                    Value::String(reply.text.clone())
                };
                let mut entry = json!({"role": "assistant", "content": content});
                if !calls.is_empty() {
                    entry["tool_calls"] = Value::Array(calls);
                }
                out.push(entry);
            }
            Message::ToolResults(results) => {
                for result in results {
                    out.push(json!({
                        "role": "tool",
                        "tool_call_id": result.call_id,
                        "content": result.text,
                    }));
                }
                out.extend(result_images(results));
            }
        }
    }
    out
}

/// A user message carrying the images `results` returned, when any did.
///
/// Tool messages take only text on this wire, so the images follow the tool
/// messages of the batch in one user message, each after a line naming the
/// call it came from.
fn result_images(results: &[crate::ToolResult]) -> Option<Value> {
    let parts: Vec<Value> = results
        .iter()
        .filter(|result| !result.images.is_empty())
        .flat_map(|result| {
            let label = json!({
                "type": "text",
                "text": format!("Images returned by the {} call {}:", result.name, result.call_id),
            });
            let images = result.images.iter().map(
                |image| json!({ "type": "image_url", "image_url": { "url": image.data_url() } }),
            );
            std::iter::once(label).chain(images)
        })
        .collect();
    (!parts.is_empty()).then(|| json!({ "role": "user", "content": parts }))
}

/// Converts the tool specs into the `tools` array.
pub(crate) fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
    tools
        .iter()
        .map(|tool| {
            json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.schema,
                },
            })
        })
        .collect()
}

/// Builds the request body for `model`.
pub(crate) fn build_body(model: &str, request: &Request<'_>) -> Value {
    let mut body = json!({
        "model": model,
        "stream": true,
        "stream_options": {"include_usage": true},
        "messages": messages_json(request.system, request.messages),
    });
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(tools_json(request.tools));
    }
    body
}

/// Says who started the request: the user, or the agent continuing after tool results.
pub(crate) fn initiator(messages: &[Message]) -> &'static str {
    match messages.last() {
        Some(Message::ToolResults(_)) => "agent",
        _ => "user",
    }
}

/// Reads the token counts out of a `usage` object.
pub(crate) fn tokens_from(usage: &Value) -> Option<Tokens> {
    let count = |pointer: &str| usage.pointer(pointer).and_then(Value::as_u64);
    let prompt = count("/prompt_tokens")?;
    let cached = count("/prompt_tokens_details/cached_tokens").unwrap_or(0);
    Some(Tokens {
        input: prompt.saturating_sub(cached),
        output: count("/completion_tokens").unwrap_or(0),
        cache_read: cached,
        cache_write: 0,
    })
}

/// A tool call being assembled from streamed fragments.
#[derive(Default)]
struct PartialCall {
    /// The call id, sent with the first fragment.
    id: String,
    /// The tool name, sent with the first fragment.
    name: String,
    /// The argument text so far.
    arguments: String,
}

/// Collects the chunks of one streamed response.
#[derive(Default)]
pub(crate) struct Accumulator {
    /// Answer text so far.
    text: String,
    /// Reasoning text so far.
    thinking: String,
    /// Tool calls by the index the server gave them.
    calls: Vec<PartialCall>,
    /// Usage, when the server reported it.
    tokens: Option<Tokens>,
    /// The block chunks go to now.
    blocks: Blocks,
    /// Why the server said the answer ended, when it said.
    finish_reason: Option<String>,
}

impl Accumulator {
    /// Handles one event's `data`, forwarding chunks to `sink`.
    ///
    /// Returns true once the response has ended.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] when the server reports an error in the stream.
    pub(crate) fn feed(&mut self, data: &str, sink: &mut dyn Sink) -> Result<bool, ProviderError> {
        if data.trim() == "[DONE]" {
            return Ok(true);
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Ok(false);
        };
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_string);
            return Err(ProviderError::Stream(message));
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| !usage.is_null()) {
            self.tokens = tokens_from(usage);
        }
        if let Some(reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = Some(reason.to_owned());
        }
        let Some(delta) = chunk.pointer("/choices/0/delta") else {
            return Ok(false);
        };
        let reasoning = ["reasoning_content", "reasoning"].iter().find_map(|key| {
            delta
                .get(key)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
        });
        if let Some(text) = reasoning
            && self.blocks.send(sink, Part::Thinking, 0, text)
        {
            self.thinking.push_str(text);
        }
        if let Some(text) = delta.get("content").and_then(Value::as_str)
            && self.blocks.send(sink, Part::Text, 0, text)
        {
            self.text.push_str(text);
        }
        for fragment in delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.call_fragment(fragment);
        }
        Ok(false)
    }

    /// Adds one streamed tool-call fragment to the call it belongs to.
    fn call_fragment(&mut self, fragment: &Value) {
        let index = fragment.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if self.calls.len() <= index {
            self.calls.resize_with(index + 1, PartialCall::default);
        }
        let call = &mut self.calls[index];
        if let Some(id) = fragment.get("id").and_then(Value::as_str) {
            call.id = id.to_string();
        }
        if let Some(name) = fragment.pointer("/function/name").and_then(Value::as_str) {
            call.name.push_str(name);
        }
        if let Some(arguments) = fragment
            .pointer("/function/arguments")
            .and_then(Value::as_str)
        {
            call.arguments.push_str(arguments);
        }
    }

    /// Ends the stream and returns the reply it described.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] when the server says its content
    /// filter cut the answer, which would otherwise pass for a short or empty
    /// answer.
    pub(crate) fn finish(self) -> Result<Reply, ProviderError> {
        if self.finish_reason.as_deref() == Some("content_filter") {
            return Err(ProviderError::Stream(
                "the provider's content filter stopped the answer".to_owned(),
            ));
        }
        Ok(self.reply())
    }

    /// The reply the chunks so far describe.
    fn reply(self) -> Reply {
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|call| !call.name.is_empty())
            .enumerate()
            .map(|(position, call)| ToolCall {
                id: if call.id.is_empty() {
                    format!("call_{position}")
                } else {
                    call.id
                },
                name: call.name,
                arguments: parse_arguments(&call.arguments),
            })
            .collect();
        Reply {
            text: self.text,
            thinking: self.thinking,
            tool_calls,
            tokens: self.tokens,
            opaque: Vec::new(),
        }
    }
}

/// Sends the conversation to `target` using `model` and returns the reply,
/// streaming text and reasoning chunks to `sink`.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an
/// error inside the stream.
pub(crate) async fn complete(
    target: Target<'_>,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
) -> Result<Reply, ProviderError> {
    let body = build_body(model, &request);
    let response = authorized(
        reqwest::Client::new().post(format!("{}/chat/completions", target.base_url)),
        &target,
    )
    .header("accept", "text/event-stream")
    .header("X-Initiator", initiator(request.messages))
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

    /// Tool calls and their results become assistant `tool_calls` and `tool` messages.
    #[test]
    fn translates_tool_turns() {
        let messages = [
            Message::User("go".into()),
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: json!({"path": "a"}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: "data".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        ];
        let out = messages_json("sys", &messages);
        assert_eq!(out[0]["role"], "system");
        assert_eq!(out[2]["content"], Value::Null);
        assert_eq!(
            out[2]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a\"}"
        );
        assert_eq!(out[3]["role"], "tool");
        assert_eq!(out[3]["tool_call_id"], "c1");
        assert_eq!(initiator(&messages), "agent");
        assert_eq!(initiator(&messages[..1]), "user");
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

    /// Images follow the tool messages in one user message, since tool messages take only text.
    #[test]
    fn sends_tool_result_images_in_a_user_message() {
        let out = messages_json("sys", &[Message::ToolResults(vec![pictured()])]);
        assert_eq!(out[1]["role"], "tool");
        assert_eq!(out[1]["content"], "[image]");
        assert_eq!(out[2]["role"], "user");
        assert_eq!(out[2]["content"][0]["type"], "text");
        assert_eq!(out[2]["content"][1]["type"], "image_url");
        assert_eq!(
            out[2]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AAAA"
        );
        let plain = messages_json("sys", &[Message::ToolResults(Vec::new())]);
        assert_eq!(plain.len(), 1);
    }

    /// An empty reasoning field gives way to a later one that holds the text, and empty content is not streamed.
    #[test]
    fn skips_empty_reasoning_and_content() {
        let mut accumulator = Accumulator::default();
        let mut sink = Recorder::default();
        let chunk = json!({"choices": [{"delta": {
            "content": "", "reasoning_content": "", "reasoning": "hm"
        }}]});
        accumulator.feed(&chunk.to_string(), &mut sink).unwrap();
        assert_eq!(sink.thinking, ["hm"]);
        assert!(sink.text.is_empty());
        assert_eq!(sink.starts, [Part::Thinking]);
    }

    /// Reasoning streamed beside empty content stays one thinking block, and the answer that follows starts a text block.
    #[test]
    fn reasoning_beside_empty_content_is_one_block() {
        let mut accumulator = Accumulator::default();
        let mut sink = Recorder::default();
        let chunks = [
            json!({"choices": [{"delta": {"content": "", "reasoning": "The"}}]}),
            json!({"choices": [{"delta": {"content": "", "reasoning": " user"}}]}),
            json!({"choices": [{"delta": {"content": "Hey!", "reasoning": ""}}]}),
        ];
        for chunk in &chunks {
            accumulator.feed(&chunk.to_string(), &mut sink).unwrap();
        }
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        assert_eq!(sink.thinking, ["The", " user"]);
        assert_eq!(sink.text, ["Hey!"]);
    }

    /// Streamed text, reasoning, split tool-call fragments and usage are assembled.
    #[test]
    fn assembles_a_stream() {
        let mut accumulator = Accumulator::default();
        let mut sink = Recorder::default();
        let chunks = [
            json!({"choices": [{"delta": {"reasoning": "hm"}}]}),
            json!({"choices": [{"delta": {"content": "hi"}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "id": "c1", "function": {"name": "read", "arguments": "{\"pa"}}
            ]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [
                {"index": 0, "function": {"arguments": "th\":\"a\"}"}}
            ]}}]}),
            json!({"choices": [], "usage": {
                "prompt_tokens": 10, "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 6}
            }}),
        ];
        for chunk in &chunks {
            assert!(!accumulator.feed(&chunk.to_string(), &mut sink).unwrap());
        }
        assert!(accumulator.feed("[DONE]", &mut sink).unwrap());
        let reply = accumulator.finish().unwrap();
        assert_eq!(reply.text, "hi");
        assert_eq!(reply.thinking, "hm");
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        assert_eq!(reply.tool_calls[0].arguments, json!({"path": "a"}));
        assert_eq!(
            reply.tokens,
            Some(Tokens {
                input: 4,
                output: 4,
                cache_read: 6,
                cache_write: 0
            })
        );
    }

    /// An answer the provider's content filter cut is an error, not an empty answer.
    #[test]
    fn content_filter_is_an_error() {
        let mut accumulator = Accumulator::default();
        let mut sink = Recorder::default();
        let chunk = json!({"choices": [{"delta": {}, "finish_reason": "content_filter"}]});
        assert!(!accumulator.feed(&chunk.to_string(), &mut sink).unwrap());
        let error = accumulator.finish().unwrap_err();
        assert!(error.to_string().contains("content filter"), "{error}");
        let mut plain = Accumulator::default();
        let chunk = json!({"choices": [{"delta": {"content": "ok"}, "finish_reason": "stop"}]});
        plain.feed(&chunk.to_string(), &mut sink).unwrap();
        assert_eq!(plain.finish().unwrap().text, "ok");
    }

    /// An error object inside the stream is reported.
    #[test]
    fn stream_errors_are_errors() {
        let mut accumulator = Accumulator::default();
        let mut sink = Recorder::default();
        let error = accumulator
            .feed(r#"{"error":{"message":"boom"}}"#, &mut sink)
            .unwrap_err();
        assert!(error.to_string().contains("boom"));
    }

    /// Serves `body` once on a local port and returns the address and the request text it received.
    fn serve_once(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
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
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (address, handle)
    }

    /// A request goes to `/chat/completions` with the bearer token and provider headers, and the streamed reply comes back.
    #[tokio::test]
    async fn completes_over_http() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"hel\"}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
            "data: [DONE]\n\n"
        );
        let (address, server) = serve_once(body);
        let credentials = Credentials::static_key("secret".to_string());
        let target = Target {
            credentials: &credentials,
            base_url: &address,
            headers: &[("Copilot-Integration-Id", "copilot-chat")],
        };
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "sys",
            messages: &messages,
            tools: &[],
            effort: None,
        };
        let mut sink = Recorder::default();
        let reply = complete(target, "m", request, &mut sink).await.unwrap();
        assert_eq!(reply.text, "hello");
        assert_eq!(sink.text, ["hel", "lo"]);
        assert_eq!(reply.tokens.map(|t| t.output), Some(2));
        let received = server.join().unwrap().to_ascii_lowercase();
        assert!(received.starts_with("post /chat/completions"));
        assert!(received.contains("authorization: bearer secret"));
        assert!(received.contains("copilot-integration-id: copilot-chat"));
        assert!(received.contains("x-initiator: user"));
    }

    /// Entries the server marks as not for chat or hidden from the picker are left out.
    #[test]
    fn model_list_keeps_chat_models() {
        let list = json!({"data": [
            {"id": "a", "capabilities": {"type": "chat"}},
            {"id": "embed", "capabilities": {"type": "embeddings"}},
            {"id": "hidden", "model_picker_enabled": false},
            {"id": "plain"},
        ]});
        assert_eq!(model_ids(&list), ["a", "plain"]);
    }
}
