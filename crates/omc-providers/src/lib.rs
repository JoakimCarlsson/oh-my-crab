//! Streaming chat requests and model discovery for the logged-in provider.

mod chat;
mod claude;
mod codex;
mod error;
mod identity;
mod models;
mod prompt;
mod sse;
mod xxh64;

pub use chat::{
    ChatModel, DeltaSink, Effort, Message, RemoteModel, Reply, Request, Sink, Tokens, ToolCall,
    ToolResult, ToolSpec,
};
pub use error::ProviderError;
pub use models::{context_window, efforts};
pub use prompt::PROMPT;

use omc_auth::{Credentials, Provider};

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
    match provider {
        Provider::Claude => claude::list_models(credentials).await,
        Provider::Codex => codex::list_models(credentials).await,
    }
}

/// Returns the model to use when the user has not picked one.
///
/// Claude has a fixed default; Codex asks the backend which models the account
/// may use and takes the first.
///
/// # Errors
///
/// Returns [`ProviderError::NoModels`] when Codex reports no usable model, or
/// another [`ProviderError`] when the lookup fails.
pub async fn default_model(
    provider: Provider,
    credentials: &Credentials,
) -> Result<String, ProviderError> {
    match provider {
        Provider::Claude => Ok(claude::DEFAULT_MODEL.to_string()),
        Provider::Codex => codex::list_models(credentials)
            .await?
            .into_iter()
            .next()
            .ok_or(ProviderError::NoModels),
    }
}
