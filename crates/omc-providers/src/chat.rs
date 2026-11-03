//! Provider-neutral conversation types and the [`ChatModel`] trait.

use async_trait::async_trait;
use omc_auth::Credentials;
use serde_json::Value;

use crate::wires::{anthropic_messages, chat_completions, openai_responses};
use crate::{Provider, ProviderError, Wire};

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
    /// A model reply, possibly with tool calls.
    Assistant(Reply),
    /// The results for the tool calls of the previous assistant message.
    ToolResults(Vec<ToolResult>),
}

/// Where the chunks of a reply go as they stream in.
pub trait Sink: Send {
    /// Some more of the reply's text.
    fn text(&mut self, chunk: &str);

    /// Some more of what the model is thinking.
    fn thinking(&mut self, chunk: &str);
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
}

/// A hosted model reached with a login's credentials.
pub struct RemoteModel {
    /// The provider that serves the model.
    pub provider: Provider,
    /// The login's credentials.
    pub credentials: Credentials,
    /// The model id to request.
    pub model: String,
}

#[async_trait]
impl ChatModel for RemoteModel {
    /// Dispatches to the provider's streaming implementation.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        match self.provider.wire() {
            Wire::AnthropicMessages => {
                anthropic_messages::complete(&self.credentials, &self.model, request, sink).await
            }
            Wire::OpenAiResponses => {
                openai_responses::complete(&self.credentials, &self.model, request, sink).await
            }
            Wire::ChatCompletions => {
                let base_url = chat_completions::base_url(self.provider, &self.credentials);
                let target = chat_completions::Target {
                    credentials: &self.credentials,
                    base_url: &base_url,
                    headers: self.provider.def().headers,
                };
                chat_completions::complete(target, &self.model, request, sink).await
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
