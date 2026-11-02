//! Provider-neutral conversation types and the [`ChatModel`] trait.

use async_trait::async_trait;
use omc_auth::{Credentials, Provider};
use serde_json::Value;

use crate::{ProviderError, claude, codex};

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
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    /// Provider-assigned id that the result must echo.
    pub id: String,
    /// Name of the tool to run.
    pub name: String,
    /// Arguments; a string when the model produced malformed JSON.
    pub arguments: Value,
}

/// The outcome of one tool call, sent back to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolResult {
    /// Id of the call this answers.
    pub call_id: String,
    /// Name of the tool that ran.
    pub name: String,
    /// Result text or error description.
    pub text: String,
    /// Whether the call failed.
    pub is_error: bool,
}

/// What the model produced in one request.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reply {
    /// The text part of the answer.
    pub text: String,
    /// Tool calls the model wants run before it continues.
    pub tool_calls: Vec<ToolCall>,
}

/// One entry of the conversation history.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// Text typed by the user.
    User(String),
    /// A model reply, possibly with tool calls.
    Assistant(Reply),
    /// The results for the tool calls of the previous assistant message.
    ToolResults(Vec<ToolResult>),
}

/// A callback that receives each streamed chunk of reply text.
///
/// A named alias because `async_trait` mishandles the elided lifetime in an
/// inline `dyn FnMut(&str)`.
pub type DeltaSink<'a> = &'a mut (dyn FnMut(&str) + Send + 'a);

/// A model that can continue a conversation.
#[async_trait]
pub trait ChatModel: Send + Sync {
    /// Sends `messages` with `tools` available and returns the model's reply.
    ///
    /// `on_delta` receives each streamed text chunk as it arrives.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError`] when the request fails, the provider answers
    /// with a non-success status, or the stream breaks.
    async fn complete(
        &self,
        messages: &[Message],
        tools: &[ToolSpec],
        on_delta: DeltaSink<'_>,
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
        messages: &[Message],
        tools: &[ToolSpec],
        on_delta: DeltaSink<'_>,
    ) -> Result<Reply, ProviderError> {
        match self.provider {
            Provider::Claude => {
                claude::complete(&self.credentials, &self.model, messages, tools, on_delta).await
            }
            Provider::Codex => {
                codex::complete(&self.credentials, &self.model, messages, tools, on_delta).await
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
