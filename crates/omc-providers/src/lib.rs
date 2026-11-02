//! Streaming chat requests to the logged-in provider.

mod claude;
mod codex;
mod error;
mod sse;

pub use error::ProviderError;

use omc_auth::{Credentials, Provider};

/// Sends `prompt` to `provider` and calls `on_delta` with each streamed text chunk.
///
/// # Errors
///
/// Returns [`ProviderError`] when the request fails, the provider answers with a
/// non-success status, or the stream breaks.
pub async fn send(
    provider: Provider,
    credentials: &Credentials,
    prompt: &str,
    on_delta: impl FnMut(&str),
) -> Result<(), ProviderError> {
    match provider {
        Provider::Claude => claude::send(credentials, prompt, on_delta).await,
        Provider::Codex => codex::send(credentials, prompt, on_delta).await,
    }
}
