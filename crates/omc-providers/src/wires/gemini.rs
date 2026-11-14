//! Streaming chat against Google's Gemini API through `streamGenerateContent`.
//!
//! The conversation goes as `contents` of `user` and `model` turns made of parts:
//! text, inline images, function calls and function responses. Thinking models
//! stream thought summaries as parts marked `thought`, and sign their thinking
//! with opaque `thoughtSignature`s on the parts that follow it. Those signatures
//! are kept in a reply's `opaque` blocks and sent back on the same parts, which
//! Gemini requires around function calls for its reasoning to carry over.

use futures_util::{Stream, StreamExt};
use omc_auth::Credentials;
use serde_json::{Map, Value, json};

use crate::chat::Blocks;
use crate::{
    Effort, Image, Message, Part, Provider, ProviderError, Reply, Request, Sink, Stop, Tokens,
    ToolCall, ToolResult, ToolSpec, sse,
};

/// The `type` of the opaque blocks that keep a reply's thought signatures.
const SIGNATURE_BLOCK: &str = "gemini_thought_signature";

/// The signature Gemini takes on a function call it did not sign itself, such as one made in a conversation started with another provider, in place of the one it would otherwise demand.
const SKIP_SIGNATURE: &str = "skip_thought_signature_validator";

/// The prefix of the ids crab gives function calls Gemini sent without one; such ids are never sent back to Gemini.
const LOCAL_ID_PREFIX: &str = "gemini_call_";

/// How many pages of the model list are read at most.
const MAX_MODEL_PAGES: usize = 20;

/// Words in a model id that mark a model which cannot hold a conversation, such as one for embeddings, speech or images.
const NOT_FOR_CHAT: &[&str] = &[
    "embed",
    "imagen",
    "veo",
    "tts",
    "image",
    "aqa",
    "live",
    "native-audio",
];

/// The keywords of a Gemini `Schema` copied from a JSON Schema as they are.
const KEPT_KEYWORDS: &[&str] = &[
    "title",
    "description",
    "nullable",
    "minItems",
    "maxItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "minProperties",
    "maxProperties",
    "pattern",
];

/// The `format` values Gemini takes, by type; any other format is dropped.
const FORMATS: &[(&str, &[&str])] = &[
    ("string", &["enum", "date-time"]),
    ("number", &["float", "double"]),
    ("integer", &["int32", "int64"]),
];

/// How deep `$ref`s are followed before a schema is left open, so that a recursive schema ends.
const MAX_REF_DEPTH: usize = 8;

/// Where a request goes and who it is sent as.
#[derive(Clone, Copy)]
pub(crate) struct Target<'a> {
    /// The login's credentials, whose access token is the API key.
    pub credentials: &'a Credentials,
    /// The API address, such as `https://generativelanguage.googleapis.com/v1beta`, without a trailing slash.
    pub base_url: &'a str,
}

/// Returns the API address for `provider`: the one kept with the login when there is one, else the provider's own.
///
/// An address ending in `/openai`, as the OpenAI-compatible endpoint's does, loses that ending, so an override written for that endpoint keeps working.
pub(crate) fn base_url(provider: Provider, credentials: &Credentials) -> String {
    let base = credentials
        .base_url
        .clone()
        .unwrap_or_else(|| provider.def().base_url());
    let base = base.trim_end_matches('/');
    base.strip_suffix("/openai").unwrap_or(base).to_owned()
}

/// Adds the headers kept with the login and the API key to `request`.
fn authorized(request: reqwest::RequestBuilder, target: &Target<'_>) -> reqwest::RequestBuilder {
    let mut request = request;
    for (name, value) in &target.credentials.headers {
        request = request.header(name.as_str(), value.as_str());
    }
    if target.credentials.access.is_empty() {
        return request;
    }
    request.header("x-goog-api-key", &target.credentials.access)
}

/// The address `model` streams from under `base`.
pub(crate) fn stream_url(base: &str, model: &str) -> String {
    let model = model.strip_prefix("models/").unwrap_or(model);
    format!("{base}/models/{model}:streamGenerateContent?alt=sse")
}

/// Picks the chat model ids out of one page of a model list: those that take `generateContent`, without the `models/` prefix, and without the ones whose id names another kind of model.
pub(crate) fn model_ids(page: &Value) -> Vec<String> {
    page.get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry
                .get("supportedGenerationMethods")
                .and_then(Value::as_array)
                .is_none_or(|methods| {
                    methods
                        .iter()
                        .any(|method| method.as_str() == Some("generateContent"))
                })
        })
        .filter_map(|entry| entry.get("name").and_then(Value::as_str))
        .map(|name| name.strip_prefix("models/").unwrap_or(name))
        .filter(|id| {
            let lower = id.to_ascii_lowercase();
            !NOT_FOR_CHAT.iter().any(|word| lower.contains(word))
        })
        .map(str::to_owned)
        .collect()
}

/// `value` escaped for a query string: every byte but unreserved ones as `%XX`.
fn query_escape(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}

/// Lists the chat model ids the key can use, in the order the API gives them, following its pages.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(crate) async fn list_models(target: Target<'_>) -> Result<Vec<String>, ProviderError> {
    let client = reqwest::Client::new();
    let mut ids = Vec::new();
    let mut page_token: Option<String> = None;
    for _ in 0..MAX_MODEL_PAGES {
        let mut url = format!("{}/models?pageSize=1000", target.base_url);
        if let Some(token) = &page_token {
            url.push_str("&pageToken=");
            url.push_str(&query_escape(token));
        }
        let request = client.get(url);
        let response = authorized(request, &target).send().await?;
        if !response.status().is_success() {
            return Err(ProviderError::from_response(response).await);
        }
        let page: Value = response.json().await?;
        ids.extend(model_ids(&page));
        page_token = page
            .get("nextPageToken")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .map(str::to_owned);
        if page_token.is_none() {
            break;
        }
    }
    Ok(ids)
}

/// The generation of a Gemini model id, such as `3` for `gemini-3.8-flash` or `2.5` for `gemini-2.5-pro`, when the id names one.
pub(crate) fn generation(model: &str) -> Option<f64> {
    let model = model.strip_prefix("models/").unwrap_or(model);
    let rest = model.strip_prefix("gemini-")?;
    let version: String = rest
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    version.trim_end_matches('.').parse().ok()
}

/// Whether `model` thinks before it answers: every Gemini from 2.5 on does.
pub(crate) fn thinks(model: &str) -> bool {
    generation(model).is_some_and(|generation| generation >= 2.5)
}

/// Whether `model` sets its thinking by level, as Gemini 3 and later do, rather than by a token budget.
fn thinks_by_level(model: &str) -> bool {
    generation(model).is_some_and(|generation| generation >= 3.0)
}

/// Whether `model` takes images inside a function response, as Gemini 3 and later do; older models are shown them in a user turn after the responses.
fn images_in_responses(model: &str) -> bool {
    thinks_by_level(model)
}

/// The effort levels `model` accepts, lightest first; none when it does not think.
pub(crate) fn efforts(model: &str) -> Vec<Effort> {
    if thinks_by_level(model) {
        vec![Effort::Low, Effort::Medium, Effort::High]
    } else if thinks(model) {
        vec![Effort::Low, Effort::Medium, Effort::High, Effort::XHigh]
    } else {
        Vec::new()
    }
}

/// The `thinkingConfig` for `model` at `effort`: thought summaries asked for, with a level on Gemini 3 and later and a token budget on 2.5; nothing for a model that does not think.
pub(crate) fn thinking_config(model: &str, effort: Option<Effort>) -> Option<Value> {
    if !thinks(model) {
        return None;
    }
    let mut config = json!({ "includeThoughts": true });
    match effort {
        None => {}
        Some(effort) if thinks_by_level(model) => {
            let level = match effort {
                Effort::Low => "low",
                Effort::Medium => "medium",
                Effort::High | Effort::XHigh | Effort::Max => "high",
            };
            config["thinkingLevel"] = json!(level);
        }
        Some(effort) => {
            let budget = match effort {
                Effort::Low => 1024,
                Effort::Medium => 8192,
                Effort::High => 16384,
                Effort::XHigh | Effort::Max => 24576,
            };
            config["thinkingBudget"] = json!(budget);
        }
    }
    Some(config)
}

/// Converts a JSON Schema into the `Schema` Gemini takes for function parameters.
///
/// `$ref`s into `$defs` or `definitions` are inlined, `oneOf` becomes `anyOf`,
/// `allOf` is merged into one schema, a `null` type or branch becomes
/// `nullable`, a list of types becomes a single type or an `anyOf` of them,
/// `const` becomes a one-value `enum`, enums become lists of strings, and every
/// keyword Gemini does not know, `additionalProperties` and `$schema` among
/// them, is dropped.
pub(crate) fn convert_schema(schema: &Value) -> Value {
    convert_node(schema, schema, 0)
}

/// Converts the schema `node`, whose `$ref`s are looked up in `root` and which sits `depth` references deep.
fn convert_node(node: &Value, root: &Value, depth: usize) -> Value {
    let Some(object) = node.as_object() else {
        return json!({});
    };
    if let Some(target) = object.get("$ref").and_then(Value::as_str) {
        let resolved = resolve_ref(root, target).filter(|_| depth < MAX_REF_DEPTH);
        let Some(resolved) = resolved else {
            return json!({});
        };
        let mut merged = resolved.as_object().cloned().unwrap_or_default();
        for (key, value) in object {
            if key != "$ref" {
                merged.insert(key.clone(), value.clone());
            }
        }
        return convert_node(&Value::Object(merged), root, depth + 1);
    }
    if let Some(parts) = object.get("allOf").and_then(Value::as_array) {
        let mut merged = object.clone();
        merged.remove("allOf");
        for part in parts {
            merge_into(&mut merged, &inline_ref(part, root));
        }
        return convert_node(&Value::Object(merged), root, depth);
    }
    let mut out = Map::new();
    let mut nullable = object.get("nullable").and_then(Value::as_bool) == Some(true);
    let types = types_of(object.get("type"));
    if types.iter().any(|kind| kind == "null") {
        nullable = true;
    }
    let concrete: Vec<&String> = types.iter().filter(|kind| *kind != "null").collect();
    match concrete.as_slice() {
        [] if !types.is_empty() => {
            out.insert("type".to_owned(), json!("string"));
        }
        [] => {}
        [single] => {
            out.insert("type".to_owned(), json!(single));
        }
        several => {
            let branches: Vec<Value> = several
                .iter()
                .map(|kind| {
                    let mut branch = object.clone();
                    branch.insert("type".to_owned(), json!(kind));
                    convert_node(&Value::Object(branch), root, depth)
                })
                .collect();
            out.insert("anyOf".to_owned(), Value::Array(branches));
        }
    }
    for key in KEPT_KEYWORDS {
        if let Some(value) = object.get(*key) {
            out.insert((*key).to_owned(), value.clone());
        }
    }
    if let (Some(format), Some(kind)) = (
        object.get("format").and_then(Value::as_str),
        out.get("type").and_then(Value::as_str),
    ) && FORMATS
        .iter()
        .any(|(owner, formats)| *owner == kind && formats.contains(&format))
    {
        out.insert("format".to_owned(), json!(format));
    }
    let values = object
        .get("enum")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| object.get("const").map(|value| vec![value.clone()]));
    if let Some(values) = values {
        if values.iter().any(Value::is_null) {
            nullable = true;
        }
        let words: Vec<Value> = values
            .iter()
            .filter(|value| !value.is_null())
            .map(|value| match value {
                Value::String(text) => json!(text),
                other => json!(other.to_string()),
            })
            .collect();
        if !words.is_empty() {
            out.insert("type".to_owned(), json!("string"));
            out.insert("enum".to_owned(), Value::Array(words));
            out.remove("format");
        }
    }
    if let Some(properties) = object.get("properties").and_then(Value::as_object) {
        let converted: Map<String, Value> = properties
            .iter()
            .map(|(name, property)| (name.clone(), convert_node(property, root, depth)))
            .collect();
        if let Some(required) = object.get("required").and_then(Value::as_array) {
            let required: Vec<Value> = required
                .iter()
                .filter(|name| {
                    name.as_str()
                        .is_some_and(|name| converted.contains_key(name))
                })
                .cloned()
                .collect();
            if !required.is_empty() {
                out.insert("required".to_owned(), Value::Array(required));
            }
        }
        out.insert("properties".to_owned(), Value::Object(converted));
        out.entry("type").or_insert_with(|| json!("object"));
    }
    if let Some(items) = object.get("items") {
        let items = match items {
            Value::Array(tuple) => tuple.first().cloned().unwrap_or_else(|| json!({})),
            single => single.clone(),
        };
        out.insert("items".to_owned(), convert_node(&items, root, depth));
        out.entry("type").or_insert_with(|| json!("array"));
    }
    let branches = object
        .get("anyOf")
        .or_else(|| object.get("oneOf"))
        .and_then(Value::as_array);
    if let Some(branches) = branches {
        let mut kept = Vec::new();
        for branch in branches {
            let null_only = types_of(branch.get("type")) == ["null"];
            if null_only {
                nullable = true;
            } else {
                kept.push(convert_node(branch, root, depth));
            }
        }
        match kept.len() {
            0 => {}
            1 => {
                if let Some(single) = kept.pop().and_then(|value| value.as_object().cloned()) {
                    for (key, value) in single {
                        out.entry(key).or_insert(value);
                    }
                }
            }
            _ => {
                out.insert("anyOf".to_owned(), Value::Array(kept));
            }
        }
    }
    if out.get("type").and_then(Value::as_str) == Some("array") && !out.contains_key("items") {
        out.insert("items".to_owned(), json!({}));
    }
    if nullable {
        out.insert("nullable".to_owned(), json!(true));
    } else {
        out.remove("nullable");
    }
    Value::Object(out)
}

/// The types a schema's `type` names: none, one, or a list.
fn types_of(kind: Option<&Value>) -> Vec<String> {
    match kind {
        Some(Value::String(kind)) => vec![kind.clone()],
        Some(Value::Array(kinds)) => kinds
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// The schema a local `$ref` such as `#/$defs/Point` points at in `root`, when it points at one.
fn resolve_ref<'a>(root: &'a Value, target: &str) -> Option<&'a Value> {
    let pointer = target.strip_prefix('#')?;
    let pointer = if pointer.is_empty() { "" } else { pointer };
    root.pointer(pointer)
}

/// `part` with a `$ref` at its top replaced by what it points at in `root`.
fn inline_ref(part: &Value, root: &Value) -> Value {
    part.get("$ref")
        .and_then(Value::as_str)
        .and_then(|target| resolve_ref(root, target))
        .cloned()
        .unwrap_or_else(|| part.clone())
}

/// Merges the schema `part` of an `allOf` into `merged`: properties and required names are joined, other keywords are taken when `merged` has none.
fn merge_into(merged: &mut Map<String, Value>, part: &Value) {
    let Some(part) = part.as_object() else {
        return;
    };
    for (key, value) in part {
        match (key.as_str(), merged.get_mut(key)) {
            ("properties", Some(Value::Object(existing))) => {
                if let Some(added) = value.as_object() {
                    for (name, schema) in added {
                        existing.insert(name.clone(), schema.clone());
                    }
                }
            }
            ("required", Some(Value::Array(existing))) => {
                for name in value.as_array().into_iter().flatten() {
                    if !existing.contains(name) {
                        existing.push(name.clone());
                    }
                }
            }
            (_, Some(_)) => {}
            (_, None) => {
                merged.insert(key.clone(), value.clone());
            }
        }
    }
}

/// Converts the tool specs into the `tools` array: one entry holding every function declaration.
pub(crate) fn tools_json(tools: &[ToolSpec]) -> Vec<Value> {
    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": convert_schema(&tool.schema),
            })
        })
        .collect();
    vec![json!({ "functionDeclarations": declarations })]
}

/// An image as an inline data part.
fn image_part(image: &Image) -> Value {
    json!({ "inlineData": { "mimeType": image.media_type, "data": image.data } })
}

/// The signature kept in `reply` for the part `slot`, which is `thinking`, `text` or a tool call's id.
fn signature<'a>(reply: &'a Reply, slot: &str) -> Option<&'a str> {
    reply
        .opaque
        .iter()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some(SIGNATURE_BLOCK))
        .find(|block| block.get("part").and_then(Value::as_str) == Some(slot))
        .and_then(|block| block.get("signature"))
        .and_then(Value::as_str)
}

/// The opaque block that keeps `signature` for the part `slot` of a reply.
fn signature_block(slot: &str, signature: &str) -> Value {
    json!({ "type": SIGNATURE_BLOCK, "part": slot, "signature": signature })
}

/// The `model` turn's parts for `reply`: its thinking, its text and its function calls, each with the signature Gemini gave it.
fn reply_parts(reply: &Reply) -> Vec<Value> {
    let mut parts = Vec::new();
    let thinking_signature = signature(reply, "thinking");
    if !reply.thinking.is_empty() || thinking_signature.is_some() {
        let mut part = json!({ "thought": true, "text": reply.thinking });
        if let Some(signature) = thinking_signature {
            part["thoughtSignature"] = json!(signature);
        }
        parts.push(part);
    }
    if !reply.text.is_empty() {
        let mut part = json!({ "text": reply.text });
        if let Some(signature) = signature(reply, "text") {
            part["thoughtSignature"] = json!(signature);
        }
        parts.push(part);
    }
    for call in &reply.tool_calls {
        let args = match &call.arguments {
            Value::Object(_) => call.arguments.clone(),
            Value::String(raw) => json!({ "raw": raw }),
            other => json!({ "value": other }),
        };
        let mut function = json!({ "name": call.name, "args": args });
        if !call.id.starts_with(LOCAL_ID_PREFIX) {
            function["id"] = json!(call.id);
        }
        let signature = signature(reply, &call.id).unwrap_or(SKIP_SIGNATURE);
        parts.push(json!({ "functionCall": function, "thoughtSignature": signature }));
    }
    parts
}

/// The `user` turn's parts for `results`, and the parts of the turn after it that shows their images to a model that cannot take them in a function response.
fn result_parts(results: &[ToolResult], nested_images: bool) -> (Vec<Value>, Vec<Value>) {
    let mut responses = Vec::new();
    let mut shown = Vec::new();
    for result in results {
        let key = if result.is_error { "error" } else { "output" };
        let mut response = json!({
            "name": result.name,
            "response": { key: result.text },
        });
        if !result.call_id.starts_with(LOCAL_ID_PREFIX) {
            response["id"] = json!(result.call_id);
        }
        if !result.images.is_empty() {
            if nested_images {
                response["parts"] = result.images.iter().map(image_part).collect();
            } else {
                shown.push(json!({
                    "text": format!("Images returned by the {} call {}:", result.name, result.call_id),
                }));
                shown.extend(result.images.iter().map(image_part));
            }
        }
        responses.push(json!({ "functionResponse": response }));
    }
    (responses, shown)
}

/// Appends `parts` to `contents` as a turn of `role`, joining them to the last turn when it has the same role, since Gemini wants the roles to alternate.
fn push_turn(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = contents.last_mut()
        && last["role"] == role
        && let Some(existing) = last["parts"].as_array_mut()
    {
        existing.extend(parts);
        return;
    }
    contents.push(json!({ "role": role, "parts": parts }));
}

/// Converts the conversation into the `contents` array for `model`.
///
/// A compaction is left out: the agent gives the model its summary as a user message in its place. So is a pruning, which the agent applies to the tool results it gives.
pub(crate) fn contents_json(messages: &[Message], model: &str) -> Vec<Value> {
    let nested_images = images_in_responses(model);
    let mut contents = Vec::new();
    for message in messages {
        match message {
            Message::User(text) if !text.trim().is_empty() => {
                push_turn(&mut contents, "user", vec![json!({ "text": text })]);
            }
            Message::Images(images) => {
                push_turn(
                    &mut contents,
                    "user",
                    images.iter().map(image_part).collect(),
                );
            }
            Message::Assistant(reply) => push_turn(&mut contents, "model", reply_parts(reply)),
            Message::ToolResults(results) => {
                let (responses, shown) = result_parts(results, nested_images);
                push_turn(&mut contents, "user", responses);
                push_turn(&mut contents, "user", shown);
            }
            Message::User(_) | Message::Compaction(_) | Message::Pruning(_) => {}
        }
    }
    contents
}

/// Builds the request body for `model`.
pub(crate) fn build_body(model: &str, request: &Request<'_>) -> Value {
    let mut body = json!({
        "contents": contents_json(request.messages, model),
    });
    if !request.system.is_empty() {
        body["systemInstruction"] = json!({ "parts": [{ "text": request.system }] });
    }
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(tools_json(request.tools));
        body["toolConfig"] = json!({ "functionCallingConfig": { "mode": "AUTO" } });
    }
    if let Some(config) = thinking_config(model, request.effort) {
        body["generationConfig"] = json!({ "thinkingConfig": config });
    }
    body
}

/// Reads the token counts out of a `usageMetadata` object: cached input is part of the prompt count, and thinking is output.
pub(crate) fn tokens_from(usage: &Value) -> Option<Tokens> {
    let count = |key: &str| usage.get(key).and_then(Value::as_u64);
    let candidates = count("candidatesTokenCount").unwrap_or(0);
    let thoughts = count("thoughtsTokenCount").unwrap_or(0);
    let total = count("totalTokenCount");
    let prompt = count("promptTokenCount")
        .or_else(|| total.map(|total| total.saturating_sub(candidates + thoughts)))?;
    let cached = count("cachedContentTokenCount").unwrap_or(0).min(prompt);
    Some(Tokens {
        input: prompt - cached,
        output: candidates + thoughts,
        cache_read: cached,
        cache_write: 0,
    })
}

/// Collects the chunks of one streamed response.
#[derive(Default)]
pub(crate) struct Accumulator {
    /// Answer text so far.
    text: String,
    /// Thought summaries so far.
    thinking: String,
    /// The latest signature a thought part carried.
    thinking_signature: Option<String>,
    /// The latest signature a text part carried.
    text_signature: Option<String>,
    /// The kind of the last part with text, which a signature-only part signs.
    last: Option<Part>,
    /// The function calls so far, with the signature each carried.
    calls: Vec<(ToolCall, Option<String>)>,
    /// Usage, when the server reported it.
    tokens: Option<Tokens>,
    /// The block chunks go to now.
    blocks: Blocks,
    /// The position of the block chunks go to, moved on after each function call.
    block: usize,
    /// Why the server said the answer ended, when it said.
    finish_reason: Option<String>,
}

impl Accumulator {
    /// Handles one event's `data`, a `GenerateContentResponse`, forwarding chunks to `sink`.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] when the server reports an error in the stream or blocks the prompt.
    pub(crate) fn feed(&mut self, data: &str, sink: &mut dyn Sink) -> Result<(), ProviderError> {
        let Ok(chunk) = serde_json::from_str::<Value>(data) else {
            return Ok(());
        };
        if let Some(error) = chunk.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned);
            return Err(ProviderError::Stream(message));
        }
        let candidate = chunk.pointer("/candidates/0");
        if candidate.is_none()
            && let Some(reason) = chunk
                .pointer("/promptFeedback/blockReason")
                .and_then(Value::as_str)
        {
            return Err(ProviderError::Stream(format!(
                "Gemini blocked the prompt ({reason})"
            )));
        }
        if let Some(usage) = chunk.get("usageMetadata")
            && let Some(tokens) = tokens_from(usage)
        {
            self.tokens = Some(tokens);
        }
        let Some(candidate) = candidate else {
            return Ok(());
        };
        for part in candidate
            .pointer("/content/parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            self.part(part, sink);
        }
        if let Some(reason) = candidate.get("finishReason").and_then(Value::as_str) {
            self.finish_reason = Some(reason.to_owned());
        }
        Ok(())
    }

    /// Handles one part of a candidate's content.
    fn part(&mut self, part: &Value, sink: &mut dyn Sink) {
        let signature = part
            .get("thoughtSignature")
            .and_then(Value::as_str)
            .filter(|signature| !signature.is_empty())
            .map(str::to_owned);
        if let Some(call) = part.get("functionCall") {
            self.call(call, signature);
            return;
        }
        let Some(text) = part.get("text").and_then(Value::as_str) else {
            return;
        };
        let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
        let kind = if thought {
            Part::Thinking
        } else if text.is_empty() {
            self.last.unwrap_or(Part::Text)
        } else {
            Part::Text
        };
        if self.blocks.send(sink, kind, self.block, text) {
            self.last = Some(kind);
            match kind {
                Part::Thinking => self.thinking.push_str(text),
                Part::Text => self.text.push_str(text),
            }
        }
        if let Some(signature) = signature {
            match kind {
                Part::Thinking => self.thinking_signature = Some(signature),
                Part::Text => self.text_signature = Some(signature),
            }
        }
    }

    /// Records a function call, which Gemini sends whole in one part.
    fn call(&mut self, call: &Value, signature: Option<String>) {
        let name = call
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let given = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .filter(|id| self.calls.iter().all(|(known, _)| known.id != *id));
        let id = given.map_or_else(
            || format!("{LOCAL_ID_PREFIX}{}", self.calls.len()),
            str::to_owned,
        );
        let arguments = call
            .get("args")
            .cloned()
            .filter(|args| !args.is_null())
            .unwrap_or_else(|| json!({}));
        self.calls.push((
            ToolCall {
                id,
                name,
                arguments,
            },
            signature,
        ));
        self.block += 1;
    }

    /// Ends the stream and returns the reply it described.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Stream`] when Gemini stopped for a reason other
    /// than the end of the answer or the output limit, such as its safety
    /// filters or a malformed function call, and [`ProviderError::Unfinished`]
    /// when the stream ended without a reason.
    pub(crate) fn finish(self) -> Result<Reply, ProviderError> {
        let stop = match self.finish_reason.as_deref() {
            None => return Err(ProviderError::Unfinished),
            Some("STOP") => Stop::Finished,
            Some("MAX_TOKENS") => Stop::Length,
            Some(other) => {
                return Err(ProviderError::Stream(format!(
                    "Gemini stopped the answer: {other}"
                )));
            }
        };
        let mut opaque = Vec::new();
        if let Some(signature) = &self.thinking_signature {
            opaque.push(signature_block("thinking", signature));
        }
        if let Some(signature) = &self.text_signature {
            opaque.push(signature_block("text", signature));
        }
        let tool_calls = self
            .calls
            .into_iter()
            .filter(|(call, _)| !call.name.is_empty())
            .map(|(call, signature)| {
                if let Some(signature) = signature {
                    opaque.push(signature_block(&call.id, &signature));
                }
                call
            })
            .collect();
        Ok(Reply {
            text: self.text,
            thinking: self.thinking,
            tool_calls,
            tokens: self.tokens,
            opaque,
            stop,
        })
    }
}

/// Reads `events` to their end and returns the reply they describe, giving `sink` each chunk.
///
/// # Errors
///
/// Returns [`ProviderError`] when the stream breaks, reports an error, or ends without a reason.
pub(crate) async fn read<S>(mut events: S, sink: &mut dyn Sink) -> Result<Reply, ProviderError>
where
    S: Stream<Item = Result<eventsource_stream::Event, ProviderError>> + Unpin,
{
    let mut accumulator = Accumulator::default();
    while let Some(event) = events.next().await {
        accumulator.feed(&event?.data, sink)?;
    }
    accumulator.finish()
}

/// Sends the conversation to `target` using `model` and returns the reply, streaming text and thought chunks to `sink`.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status or an error inside the stream.
pub(crate) async fn complete(
    target: Target<'_>,
    model: &str,
    request: Request<'_>,
    sink: &mut dyn Sink,
) -> Result<Reply, ProviderError> {
    let body = build_body(model, &request);
    let response = authorized(
        reqwest::Client::new().post(stream_url(target.base_url, model)),
        &target,
    )
    .header("accept", "text/event-stream")
    .json(&body)
    .send()
    .await?;
    read(sse::events(response).await?, sink).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use eventsource_stream::Eventsource;

    /// A recorded stream from a thinking model that thinks, says a word and calls a tool.
    const TOOL_STREAM: &str = include_str!("testdata/gemini_tool_call.sse");

    /// A recorded stream of a plain answer cut off by the output limit.
    const LENGTH_STREAM: &str = include_str!("testdata/gemini_max_tokens.sse");

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
            self.text.push(chunk.to_owned());
        }

        /// Records a thinking chunk.
        fn thinking(&mut self, chunk: &str) {
            self.thinking.push(chunk.to_owned());
        }
    }

    /// Reads the server-sent events in `raw` as a server would send them, in small pieces.
    async fn replay(raw: &str, sink: &mut Recorder) -> Result<Reply, ProviderError> {
        let pieces: Vec<Result<Vec<u8>, std::io::Error>> = raw
            .as_bytes()
            .chunks(17)
            .map(|piece| Ok(piece.to_vec()))
            .collect();
        let events = futures_util::stream::iter(pieces)
            .eventsource()
            .map(|event| event.map_err(|error| ProviderError::Stream(error.to_string())));
        read(Box::pin(events), sink).await
    }

    /// The tool stream gives thinking, text and a signed call with its arguments, usage and signatures.
    #[tokio::test]
    async fn parses_a_streamed_tool_call() {
        let mut sink = Recorder::default();
        let reply = replay(TOOL_STREAM, &mut sink).await.unwrap();
        assert_eq!(reply.thinking, "Reading the file first.");
        assert_eq!(reply.text, "Let me look.");
        assert_eq!(sink.starts, [Part::Thinking, Part::Text]);
        assert_eq!(sink.thinking.concat(), "Reading the file first.");
        assert_eq!(reply.tool_calls.len(), 1);
        assert_eq!(reply.tool_calls[0].name, "read");
        assert_eq!(
            reply.tool_calls[0].arguments,
            json!({ "path": "src/main.rs" })
        );
        assert!(reply.tool_calls[0].id.starts_with(LOCAL_ID_PREFIX));
        assert_eq!(reply.stop, Stop::Finished);
        assert_eq!(
            reply.tokens,
            Some(Tokens {
                input: 80,
                output: 45,
                cache_read: 20,
                cache_write: 0,
            })
        );
        let id = reply.tool_calls[0].id.clone();
        assert_eq!(signature(&reply, &id), Some("Q0FMTFNJRw=="));
        assert_eq!(signature(&reply, "text"), None);
    }

    /// A stream that ends with `MAX_TOKENS` is a reply cut at the limit; a trailing empty part signs the text.
    #[tokio::test]
    async fn parses_a_reply_cut_at_the_limit() {
        let mut sink = Recorder::default();
        let reply = replay(LENGTH_STREAM, &mut sink).await.unwrap();
        assert_eq!(reply.text, "Hello there, this is");
        assert_eq!(reply.stop, Stop::Length);
        assert_eq!(signature(&reply, "text"), Some("VEVYVFNJRw=="));
        assert_eq!(sink.starts, [Part::Text]);
    }

    /// A stream without a finish reason is unfinished, and a safety stop or an error is a stream error.
    #[tokio::test]
    async fn rejects_broken_and_stopped_streams() {
        let mut sink = Recorder::default();
        let cut = "data: {\"candidates\":[{\"content\":{\"role\":\"model\",\"parts\":[{\"text\":\"Hi\"}]}}]}\n\n";
        assert!(matches!(
            replay(cut, &mut sink).await,
            Err(ProviderError::Unfinished)
        ));
        let unsafe_stop = "data: {\"candidates\":[{\"finishReason\":\"SAFETY\"}]}\n\n";
        assert!(matches!(
            replay(unsafe_stop, &mut sink).await,
            Err(ProviderError::Stream(message)) if message.contains("SAFETY")
        ));
        let error = "data: {\"error\":{\"code\":429,\"message\":\"quota\"}}\n\n";
        assert!(matches!(
            replay(error, &mut sink).await,
            Err(ProviderError::Stream(message)) if message == "quota"
        ));
        let blocked = "data: {\"promptFeedback\":{\"blockReason\":\"SAFETY\"}}\n\n";
        assert!(replay(blocked, &mut sink).await.is_err());
    }

    /// A reply read from the stream goes back as a model turn with every signature where Gemini put it.
    #[tokio::test]
    async fn sends_signatures_back() {
        let mut sink = Recorder::default();
        let reply = replay(TOOL_STREAM, &mut sink).await.unwrap();
        let id = reply.tool_calls[0].id.clone();
        let messages = [
            Message::User("read main".into()),
            Message::Assistant(reply),
            Message::ToolResults(vec![ToolResult {
                call_id: id,
                name: "read".into(),
                text: "fn main() {}".into(),
                is_error: false,
                images: Vec::new(),
            }]),
        ];
        let contents = contents_json(&messages, "gemini-3.8-flash");
        assert_eq!(contents.len(), 3);
        let model = &contents[1];
        assert_eq!(model["role"], "model");
        assert_eq!(model["parts"][0]["thought"], true);
        assert_eq!(model["parts"][1]["text"], "Let me look.");
        assert!(model["parts"][1].get("thoughtSignature").is_none());
        let call = &model["parts"][2];
        assert_eq!(call["thoughtSignature"], "Q0FMTFNJRw==");
        assert_eq!(call["functionCall"]["args"]["path"], "src/main.rs");
        assert!(call["functionCall"].get("id").is_none());
        let response = &contents[2]["parts"][0]["functionResponse"];
        assert_eq!(response["name"], "read");
        assert_eq!(response["response"]["output"], "fn main() {}");
        assert!(response.get("id").is_none());
    }

    /// The body carries the system instruction, user turns, images, declarations and the thinking config.
    #[test]
    fn builds_the_request_body() {
        let tools = [ToolSpec {
            name: "read".into(),
            description: "Read a file".into(),
            schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false,
            }),
        }];
        let image = Image {
            media_type: "image/png".into(),
            data: "AAAA".into(),
        };
        let messages = [
            Message::User("look".into()),
            Message::Images(vec![image]),
            Message::Assistant(Reply {
                text: "ok".into(),
                tool_calls: vec![ToolCall {
                    id: "server-1".into(),
                    name: "read".into(),
                    arguments: json!({ "path": "a" }),
                }],
                ..Reply::default()
            }),
        ];
        let request = Request {
            system: "be brief",
            messages: &messages,
            tools: &tools,
            effort: Some(Effort::High),
        };
        let body = build_body("gemini-3.8-flash", &request);
        assert_eq!(body["systemInstruction"]["parts"][0]["text"], "be brief");
        assert_eq!(body["contents"].as_array().unwrap().len(), 2);
        let user = &body["contents"][0];
        assert_eq!(user["parts"][0]["text"], "look");
        assert_eq!(user["parts"][1]["inlineData"]["mimeType"], "image/png");
        let call = &body["contents"][1]["parts"][1];
        assert_eq!(call["functionCall"]["id"], "server-1");
        assert_eq!(call["thoughtSignature"], SKIP_SIGNATURE);
        let declaration = &body["tools"][0]["functionDeclarations"][0];
        assert_eq!(declaration["name"], "read");
        assert!(
            declaration["parameters"]
                .get("additionalProperties")
                .is_none()
        );
        assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "AUTO");
        assert_eq!(
            body["generationConfig"]["thinkingConfig"],
            json!({ "includeThoughts": true, "thinkingLevel": "high" })
        );
        let older = build_body("gemini-2.5-pro", &request);
        assert_eq!(
            older["generationConfig"]["thinkingConfig"]["thinkingBudget"],
            16384
        );
        let plain = build_body("gemini-2.0-flash", &request);
        assert!(plain.get("generationConfig").is_none());
    }

    /// Images a tool returned go inside the function response on Gemini 3, and in a user turn after the responses before it.
    #[test]
    fn places_tool_result_images_by_generation() {
        let results = Message::ToolResults(vec![ToolResult {
            call_id: "c1".into(),
            name: "screenshot".into(),
            text: "[image]".into(),
            is_error: true,
            images: vec![Image {
                media_type: "image/jpeg".into(),
                data: "BBBB".into(),
            }],
        }]);
        let nested = contents_json(std::slice::from_ref(&results), "gemini-3.8-pro");
        let response = &nested[0]["parts"][0]["functionResponse"];
        assert_eq!(response["response"]["error"], "[image]");
        assert_eq!(response["id"], "c1");
        assert_eq!(response["parts"][0]["inlineData"]["data"], "BBBB");
        let separate = contents_json(&[results], "gemini-2.5-flash");
        let parts = separate[0]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert!(parts[0]["functionResponse"].get("parts").is_none());
        assert_eq!(parts[2]["inlineData"]["mimeType"], "image/jpeg");
    }

    /// References, unions, nulls, constants and unknown keywords become a schema Gemini takes.
    #[test]
    fn converts_json_schema() {
        let schema = json!({
            "$schema": "https://json-schema.org/draft/2020-12/schema",
            "type": "object",
            "additionalProperties": false,
            "$defs": { "Point": { "type": "object", "properties": { "x": { "type": "number" } }, "required": ["x", "gone"] } },
            "properties": {
                "point": { "$ref": "#/$defs/Point" },
                "label": { "type": ["string", "null"], "format": "uri", "maxLength": 9 },
                "mode": { "const": "fast" },
                "level": { "type": "integer", "enum": [1, 2] },
                "either": { "oneOf": [{ "type": "string" }, { "type": "integer" }] },
                "maybe": { "anyOf": [{ "type": "boolean" }, { "type": "null" }] },
                "many": { "type": "array", "items": { "type": "string", "format": "date-time" }, "uniqueItems": true },
                "mixed": { "type": ["string", "number"] },
                "joined": { "allOf": [
                    { "type": "object", "properties": { "a": { "type": "string" } }, "required": ["a"] },
                    { "properties": { "b": { "type": "string" } }, "required": ["b"] }
                ] },
                "list": { "type": "array" },
                "anything": true
            },
            "required": ["point", "missing"]
        });
        let converted = convert_schema(&schema);
        assert!(converted.get("$schema").is_none());
        assert!(converted.get("additionalProperties").is_none());
        assert!(converted.get("$defs").is_none());
        assert_eq!(converted["required"], json!(["point"]));
        let properties = &converted["properties"];
        assert_eq!(properties["point"]["type"], "object");
        assert_eq!(properties["point"]["properties"]["x"]["type"], "number");
        assert_eq!(properties["point"]["required"], json!(["x"]));
        assert_eq!(
            properties["label"],
            json!({ "type": "string", "nullable": true, "maxLength": 9 })
        );
        assert_eq!(
            properties["mode"],
            json!({ "type": "string", "enum": ["fast"] })
        );
        assert_eq!(
            properties["level"],
            json!({ "type": "string", "enum": ["1", "2"] })
        );
        assert_eq!(properties["either"]["anyOf"][1]["type"], "integer");
        assert_eq!(
            properties["maybe"],
            json!({ "type": "boolean", "nullable": true })
        );
        assert_eq!(
            properties["many"],
            json!({ "type": "array", "items": { "type": "string", "format": "date-time" } })
        );
        assert_eq!(properties["mixed"]["anyOf"][0]["type"], "string");
        assert_eq!(properties["mixed"]["anyOf"][1]["type"], "number");
        assert_eq!(properties["joined"]["required"], json!(["a", "b"]));
        assert_eq!(properties["joined"]["properties"]["b"]["type"], "string");
        assert_eq!(properties["list"]["items"], json!({}));
        assert_eq!(properties["anything"], json!({}));
    }

    /// A schema that refers to itself ends instead of recursing forever.
    #[test]
    fn ends_recursive_schemas() {
        let schema = json!({
            "type": "object",
            "properties": { "child": { "$ref": "#" } }
        });
        let converted = convert_schema(&schema);
        assert_eq!(converted["properties"]["child"]["type"], "object");
    }

    /// Usage counts cached input apart from the rest of the prompt and thinking as output.
    #[test]
    fn reads_usage() {
        let usage = json!({
            "promptTokenCount": 100,
            "cachedContentTokenCount": 30,
            "candidatesTokenCount": 7,
            "thoughtsTokenCount": 5,
            "totalTokenCount": 112
        });
        assert_eq!(
            tokens_from(&usage),
            Some(Tokens {
                input: 70,
                output: 12,
                cache_read: 30,
                cache_write: 0,
            })
        );
        assert_eq!(tokens_from(&json!({})), None);
    }

    /// The model list keeps chat models that generate content, without the prefix.
    #[test]
    fn picks_chat_models() {
        let page = json!({ "models": [
            { "name": "models/gemini-3.8-flash", "supportedGenerationMethods": ["generateContent", "countTokens"] },
            { "name": "models/gemini-embedding-2", "supportedGenerationMethods": ["embedContent"] },
            { "name": "models/imagen-5", "supportedGenerationMethods": ["predict"] },
            { "name": "models/gemini-3.8-flash-image", "supportedGenerationMethods": ["generateContent"] },
            { "name": "models/gemini-2.5-pro" }
        ] });
        assert_eq!(model_ids(&page), ["gemini-3.8-flash", "gemini-2.5-pro"]);
    }

    /// Generations parse from ids, and set how the model thinks and which efforts it offers.
    #[test]
    fn reads_generations() {
        assert_eq!(generation("gemini-3.8-flash"), Some(3.8));
        assert_eq!(generation("models/gemini-2.5-pro"), Some(2.5));
        assert_eq!(generation("gemini-3-pro-preview"), Some(3.0));
        assert_eq!(generation("gemma-3"), None);
        assert!(thinks("gemini-2.5-flash") && !thinks("gemini-2.0-flash"));
        assert_eq!(efforts("gemini-3.8-flash").len(), 3);
        assert_eq!(efforts("gemini-2.5-pro").len(), 4);
        assert!(efforts("gemini-1.5-pro").is_empty());
    }

    /// Addresses drop the OpenAI-compatible ending and the `models/` prefix.
    #[test]
    fn builds_addresses() {
        let mut credentials = Credentials::static_key("key".to_owned());
        credentials.base_url = Some("https://example.test/v1beta/openai/".into());
        assert_eq!(
            base_url(Provider::Gemini, &credentials),
            "https://example.test/v1beta"
        );
        assert_eq!(
            stream_url("https://h/v1beta", "models/gemini-3.8-flash"),
            "https://h/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse"
        );
    }
}
