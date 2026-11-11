//! Provider-neutral conversation types and the [`ChatModel`] trait.

use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use omc_auth::Credentials;
use serde_json::Value;

use crate::wires::{anthropic_messages, chat_completions, openai_responses};
use crate::{Provider, ProviderError, Retention, Wire};

/// A tool the model may call, in provider-neutral form.
#[derive(Debug, Clone)]
pub struct ToolSpec {
    /// Name the model calls the tool by.
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// JSON schema of the arguments object.
    pub schema: Value,
}

/// A tool invocation requested by the model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    /// Provider-assigned id that the result must echo.
    pub id: String,
    /// Name of the tool to run.
    pub name: String,
    /// Arguments; a string when the model produced malformed JSON.
    pub arguments: Value,
}

/// The outcome of one tool call, sent back to the model.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolResult {
    /// Id of the call this answers.
    pub call_id: String,
    /// Name of the tool that ran.
    pub name: String,
    /// Result text or error description.
    pub text: String,
    /// Whether the call failed.
    #[serde(default)]
    pub is_error: bool,
    /// Images the tool returned for the model to look at, after the text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<Image>,
}

/// An image sent to the model, such as a picture a tool read.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Image {
    /// The MIME type, such as `image/png`.
    pub media_type: String,
    /// The image's bytes, base64-encoded.
    pub data: String,
}

impl Image {
    /// The image as a `data:` URL, the form the OpenAI wires take images in.
    pub fn data_url(&self) -> String {
        format!("data:{};base64,{}", self.media_type, self.data)
    }
}

/// How hard the model is asked to think.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    /// Little or no reasoning.
    Low,
    /// A balance of speed and depth.
    Medium,
    /// Deep reasoning.
    High,
    /// Deeper still.
    XHigh,
    /// The most the model will do.
    Max,
}

impl Effort {
    /// Every level, lightest first.
    pub const ALL: [Effort; 5] = [
        Effort::Low,
        Effort::Medium,
        Effort::High,
        Effort::XHigh,
        Effort::Max,
    ];

    /// The word the level is known by.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
            Effort::XHigh => "xhigh",
            Effort::Max => "max",
        }
    }

    /// The level `word` names, when it names one.
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == word)
    }
}

/// What a request cost in tokens, as the provider counted it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Tokens {
    /// Input that was not read from a cache.
    pub input: u64,
    /// Output, reasoning included.
    pub output: u64,
    /// Input read from a cache.
    pub cache_read: u64,
    /// Input written to a cache.
    pub cache_write: u64,
}

impl Tokens {
    /// How many tokens the conversation held when the request was made: everything that was sent.
    pub fn context(&self) -> u64 {
        self.input + self.cache_read + self.cache_write
    }
}

/// What the model produced in one request.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Reply {
    /// The text part of the answer.
    pub text: String,
    /// What the model thought, as text to show.
    pub thinking: String,
    /// Tool calls the model wants run before it continues.
    pub tool_calls: Vec<ToolCall>,
    /// What the request cost, when the provider said.
    pub tokens: Option<Tokens>,
    /// Blocks the provider wants sent back with the next request, such as signed thinking; kept as the provider gave them.
    pub opaque: Vec<Value>,
    /// Why the model stopped.
    #[serde(skip_serializing_if = "Stop::is_finished")]
    pub stop: Stop,
}

/// Why the model stopped writing a reply.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Stop {
    /// It came to the end of what it meant to say, or of the tool calls it meant to make.
    #[default]
    Finished,
    /// The output limit cut it off, so its last block, a tool call's arguments included, may be incomplete.
    Length,
}

impl Stop {
    /// Whether the model came to the end of what it meant to say.
    pub fn is_finished(&self) -> bool {
        *self == Stop::Finished
    }
}

/// Everything one request to a model carries.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// The system prompt.
    pub system: &'a str,
    /// The conversation so far.
    pub messages: &'a [Message],
    /// The tools the model may call.
    pub tools: &'a [ToolSpec],
    /// How hard the model is asked to think, or the provider's default when there is none.
    pub effort: Option<Effort>,
}

/// One entry of the conversation history.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Message {
    /// Text typed by the user.
    User(String),
    /// Images the user attached to the prompt before it, which the model is shown with that prompt.
    Images(Vec<Image>),
    /// A model reply, possibly with tool calls.
    Assistant(Reply),
    /// The results for the tool calls of the previous assistant message.
    ToolResults(Vec<ToolResult>),
    /// The point where the older part of the history was summarized; it is never sent as it is, but stands for what the model is given from then on.
    Compaction(Compaction),
    /// The point where tool outputs earlier in the history were elided to save room; it is never sent, but from then on those outputs are given to the model as short notes.
    Pruning(Pruning),
}

/// Tool outputs left out of what the model is given from the point this is recorded on, each replaced with a short note saying why and how to read it again.
///
/// The outputs themselves stay in the history, so the conversation can still be shown, and the outputs read again, whole.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Pruning {
    /// The outputs left out.
    pub elided: Vec<Elided>,
    /// The large fenced blocks cut out of earlier prompts and replies, each replaced with a short note.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub excised: Vec<Excised>,
    /// About how many tokens the context held right after the outputs were left out, system prompt included.
    pub tokens_after: u64,
}

/// One fenced block cut out of the text of a prompt or a reply in what the model is given.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Excised {
    /// The position in the history of the prompt or reply the block is in.
    pub at: usize,
    /// The byte offset in its text where the block starts, at the start of its opening fence line.
    pub start: usize,
    /// The byte offset in its text where the block ends, at the end of its closing fence line.
    pub end: usize,
}

/// One tool output left out of what the model is given.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Elided {
    /// The position in the history of the tool results the output is one of.
    pub at: usize,
    /// The id of the call the output answers.
    pub call_id: String,
    /// Why it was left out.
    pub reason: Elision,
}

/// Why a tool output was left out of what the model is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Elision {
    /// It was old and large, and the context was nearly full.
    Old,
    /// The same read was made again later, so this copy is out of date.
    Superseded,
    /// It was heavy and older than the recent messages, and the context was shaken to make room without a summary.
    Shaken,
}

/// A summary that stands in for the older part of a history, recorded at the point it was made.
///
/// The messages before it stay in the history, so the conversation can still be shown and read whole; what the model is given from this point on is the summary, then the messages from `first_kept` up to this point, then the messages after it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Compaction {
    /// The summary of the messages it replaces.
    pub summary: String,
    /// The position in the history of the first message kept as it is after the summary.
    pub first_kept: usize,
    /// About how many tokens the context held before it was compacted.
    pub tokens_before: u64,
    /// About how many tokens the context held right after it was compacted, system prompt included.
    pub tokens_after: u64,
    /// The files read in the summarized messages and in those summarized before them, without the ones also changed.
    pub read_files: Vec<String>,
    /// The files changed in the summarized messages and in those summarized before them.
    pub modified_files: Vec<String>,
}

/// The kind of a block of a streamed reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    /// What the model is thinking.
    Thinking,
    /// The reply's text.
    Text,
}

/// Where the chunks of a reply go as they stream in.
///
/// Each block of the reply is announced with [`Sink::start`] before its first chunk, so the chunks that follow belong to that block until the next one starts.
pub trait Sink: Send {
    /// A new block of the reply, of the kind `part`, starts.
    fn start(&mut self, part: Part);

    /// Some more of the reply's text.
    fn text(&mut self, chunk: &str);

    /// Some more of what the model is thinking.
    fn thinking(&mut self, chunk: &str);
}

/// Tracks the block a stream is in and announces each new one to a [`Sink`] before its first chunk.
#[derive(Debug, Default)]
pub(crate) struct Blocks {
    /// The kind and the server's position of the block chunks go to now.
    open: Option<(Part, usize)>,
}

impl Blocks {
    /// Whether the block open now is the one at the server's position `block`, of the kind `part`.
    pub(crate) fn is_open(&self, part: Part, block: usize) -> bool {
        self.open == Some((part, block))
    }

    /// Sends `chunk` of the block at the server's position `block`, of the kind `part`, to `sink`, starting the block first when it is not the one open.
    ///
    /// An empty chunk is dropped and does not start a block. Returns whether the chunk was sent.
    pub(crate) fn send(
        &mut self,
        sink: &mut dyn Sink,
        part: Part,
        block: usize,
        chunk: &str,
    ) -> bool {
        if chunk.is_empty() {
            return false;
        }
        if !self.is_open(part, block) {
            self.open = Some((part, block));
            sink.start(part);
        }
        match part {
            Part::Thinking => sink.thinking(chunk),
            Part::Text => sink.text(chunk),
        }
        true
    }
}

/// A model that can continue a conversation.
#[async_trait]
pub trait ChatModel: Send + Sync {
    /// Sends `request` and returns the model's reply, giving `sink` each chunk as it streams in.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when the request fails, the provider answers
    /// with a non-success status, or the stream breaks.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError>;

    /// Sends `request` like [`ChatModel::complete`], asking the provider to write nothing to its prompt cache, for a request whose prefix is never sent again, such as a one-off summary; a model that cannot be asked so sends it like [`ChatModel::complete`].
    ///
    /// # Errors
    ///
    /// Same as [`ChatModel::complete`].
    async fn complete_uncached(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.complete(request, sink).await
    }

    /// Sends `request` again asking for no output, so that the provider keeps the prompt cache entries it reads alive, and returns what that cost; nothing, and nothing sent, when the model cannot be warmed so.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when the request fails or the provider answers with a non-success status.
    async fn warm(&self, request: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
        let _ = request;
        Ok(None)
    }
}

/// A hosted model reached with a login's credentials.
///
/// The credentials are looked at again for every request, so a login whose token runs out, or is renewed by another process, in the middle of long work is renewed rather than ending the work.
pub struct RemoteModel {
    /// The provider that serves the model.
    provider: Provider,
    /// The credentials the last request was sent with, or is to be.
    credentials: Mutex<Credentials>,
    /// The model id to request.
    model: String,
    /// How long the provider is asked to keep the prompt cache entries requests write.
    retention: Retention,
    /// The folder the logins are kept in, or nothing for the default one.
    home: Option<PathBuf>,
}

#[async_trait]
impl ChatModel for RemoteModel {
    /// Dispatches to the provider's streaming implementation.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.send(request, sink, Some(self.retention)).await
    }

    /// Sends the request without cache breakpoints when the provider takes them, and like any other otherwise.
    async fn complete_uncached(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.send(request, sink, None).await
    }

    /// Replays the request with no output on Anthropic's own host, when its thinking settings allow it; the other wires and hosts are not warmed.
    async fn warm(&self, request: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
        let credentials = self.current().await;
        match self.warm_with(&credentials, request).await {
            Err(error) if error.is_unauthorized() => match self.replacement(&credentials).await {
                Some(renewed) => self.warm_with(&renewed, request).await,
                None => Err(error),
            },
            answered => answered,
        }
    }
}

impl RemoteModel {
    /// The model `model` of `provider`, reached with `credentials`, which are kept in `home`, or in the default folder when there is none, and renewed there when they run out; `retention` is how long the provider is asked to keep the prompt cache entries requests write.
    pub fn new(
        provider: Provider,
        credentials: Credentials,
        model: String,
        retention: Retention,
        home: Option<PathBuf>,
    ) -> Self {
        Self {
            provider,
            credentials: Mutex::new(credentials),
            model,
            retention,
            home,
        }
    }

    /// The credentials held now.
    fn held(&self) -> Credentials {
        self.credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Holds `credentials` for the requests to come.
    fn hold(&self, credentials: &Credentials) {
        *self
            .credentials
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = credentials.clone();
    }

    /// The credentials to send the next request with: those held, or, when their token has run out, the login as it is stored now, renewed when need be. When that fails the held ones are sent, and the provider's refusal says what is wrong.
    async fn current(&self) -> Credentials {
        let held = self.held();
        if !held.is_expired() {
            return held;
        }
        let stored = omc_auth::with_home(self.home.clone(), crate::credentials_for(self.provider));
        match stored.await {
            Ok(fresh) => {
                self.hold(&fresh);
                fresh
            }
            Err(_) => held,
        }
    }

    /// The credentials to send a request again with after the provider refused the token of `refused`: those another request of this model got meanwhile, or the login renewed; nothing when there is no login to renew or the provider will not renew it.
    async fn replacement(&self, refused: &Credentials) -> Option<Credentials> {
        let held = self.held();
        if held.access != refused.access {
            return Some(held);
        }
        let def = self.provider.def();
        let renewal = omc_auth::credentials_after_refusal(def.id, &def.auth, refused);
        let renewed = omc_auth::with_home(self.home.clone(), renewal)
            .await
            .ok()
            .flatten()?;
        self.hold(&renewed);
        Some(renewed)
    }

    /// Sends `request` with the credentials of now, and once more with a renewed login when the provider refuses the token, giving `sink` each chunk; on the Anthropic wire `cache` says how long the cache entries it writes live, and nothing asks it to write none.
    ///
    /// # Errors
    ///
    /// Same as [`ChatModel::complete`].
    async fn send(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
        cache: Option<Retention>,
    ) -> Result<Reply, ProviderError> {
        let credentials = self.current().await;
        match self.send_with(&credentials, request, sink, cache).await {
            Err(error) if error.is_unauthorized() => match self.replacement(&credentials).await {
                Some(renewed) => self.send_with(&renewed, request, sink, cache).await,
                None => Err(error),
            },
            answered => answered,
        }
    }

    /// Dispatches `request`, authorized with `credentials`, to the provider's streaming implementation, giving `sink` each chunk; `cache` is as for [`RemoteModel::send`].
    ///
    /// # Errors
    ///
    /// Same as [`ChatModel::complete`].
    async fn send_with(
        &self,
        credentials: &Credentials,
        request: Request<'_>,
        sink: &mut dyn Sink,
        cache: Option<Retention>,
    ) -> Result<Reply, ProviderError> {
        match self.provider.wire() {
            Wire::AnthropicMessages | Wire::Bedrock | Wire::Vertex => {
                anthropic_messages::complete(
                    self.provider,
                    credentials,
                    &self.model,
                    request,
                    sink,
                    cache,
                )
                .await
            }
            Wire::OpenAiResponses => {
                openai_responses::complete(credentials, &self.model, request, sink).await
            }
            Wire::ChatCompletions => {
                let base_url = chat_completions::base_url(self.provider, credentials);
                let target = chat_completions::Target {
                    credentials,
                    base_url: &base_url,
                    headers: self.provider.def().headers,
                    replay_reasoning: self.provider.def().replay_reasoning,
                };
                chat_completions::complete(target, &self.model, request, sink).await
            }
        }
    }

    /// Replays `request`, authorized with `credentials`, asking for no output, where the wire and host can be warmed so.
    ///
    /// # Errors
    ///
    /// Same as [`ChatModel::warm`].
    async fn warm_with(
        &self,
        credentials: &Credentials,
        request: Request<'_>,
    ) -> Result<Option<Tokens>, ProviderError> {
        match self.provider.wire() {
            Wire::AnthropicMessages => {
                anthropic_messages::warm(
                    self.provider,
                    credentials,
                    &self.model,
                    request,
                    self.retention,
                )
                .await
            }
            Wire::OpenAiResponses | Wire::ChatCompletions | Wire::Bedrock | Wire::Vertex => {
                Ok(None)
            }
        }
    }
}

/// Parses the accumulated argument text of a streamed tool call.
///
/// Empty text means no arguments; malformed text is kept as a JSON string so
/// the tool can report it to the model.
pub fn parse_arguments(raw: &str) -> Value {
    if raw.trim().is_empty() {
        return Value::Object(Default::default());
    }
    serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// A callback that receives each streamed chunk of reply text.
///
/// A named alias because `async_trait` mishandles the elided lifetime in an
/// inline `dyn FnMut(&str)`.
pub type DeltaSink<'a> = &'a mut (dyn FnMut(&str) + Send + 'a);
