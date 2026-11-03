//! Streaming chat requests and model discovery for the logged-in provider.

mod chat;
mod error;
mod identity;
mod models;
mod prompt;
mod provider;
mod providers;
mod sse;
mod wires;
mod xxh64;

pub use chat::{
    ChatModel, DeltaSink, Effort, Message, RemoteModel, Reply, Request, Sink, Tokens, ToolCall,
    ToolResult, ToolSpec,
};
pub use error::ProviderError;
pub use models::{context_window, efforts};
pub use prompt::PROMPT;
pub use provider::{Def, Provider, Wire, active_provider, credentials_for, login};

use omc_auth::Credentials;
use wires::{anthropic_messages, chat_completions, openai_responses};

/// Lists the model ids the account can use with `provider`, best first.
///
/// # Errors
///
/// Returns [`ProviderError`] when the request fails or the provider answers with
/// a non-success status.
pub async fn list_models(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Vec<String>, ProviderError> {
    match provider.wire() {
        Wire::AnthropicMessages => anthropic_messages::list_models(credentials).await,
        Wire::OpenAiResponses => openai_responses::list_models(credentials).await,
        Wire::ChatCompletions => {
            let base_url = chat_completions::base_url(provider, credentials);
            chat_completions::list_models(chat_completions::Target {
                credentials,
                base_url: &base_url,
                headers: provider.def().headers,
            })
            .await
        }
    }
}

/// Returns the model to use when the user has not picked one.
///
/// A provider with a fixed default uses it; otherwise the backend is asked which
/// models the account may use and the first is taken.
///
/// # Errors
///
/// Returns [`ProviderError::NoModels`] when the backend reports no usable model, or
/// another [`ProviderError`] when the lookup fails.
pub async fn default_model(
    provider: Provider,
    credentials: &Credentials,
) -> Result<String, ProviderError> {
    if let Some(model) = provider.def().default_model {
        return Ok(model.to_string());
    }
    list_models(provider, credentials)
        .await?
        .into_iter()
        .next()
        .ok_or(ProviderError::NoModels)
}
