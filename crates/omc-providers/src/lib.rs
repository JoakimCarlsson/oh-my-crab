//! Streaming chat requests and model discovery for the logged-in provider.

mod chat;
mod custom;
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
    ChatModel, Compaction, DeltaSink, Effort, Elided, Elision, Excised, Image, Message, Part,
    Pruning, RemoteModel, Reply, Request, Sink, Stop, Tokens, ToolCall, ToolResult, ToolSpec,
};
pub use custom::{Custom, custom_providers_path};
pub use error::ProviderError;
pub use models::{Pricing, Retention, cache_ttl, context_window, efforts, pricing, warmable};
pub use prompt::PROMPT;
pub use provider::{Def, Provider, Wire, active_provider, credentials_for, login};

use omc_auth::Credentials;
use wires::{anthropic_messages, chat_completions, openai_responses};

/// Lists the model ids the account can use with `provider`, best first.
///
/// On the Chat Completions wire the provider's default model leads the list, listed
/// by the server or not, so that a conversation opened without a model picks it.
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
            let listed = chat_completions::list_models(chat_completions::Target {
                credentials,
                base_url: &base_url,
                headers: provider.def().headers,
                replay_reasoning: provider.def().replay_reasoning,
            })
            .await?;
            Ok(default_first(listed, provider.def().default_model))
        }
    }
}

/// Puts `default` at the front of `models`, moving it there when listed and adding it when not.
fn default_first(mut models: Vec<String>, default: Option<&str>) -> Vec<String> {
    let Some(default) = default else {
        return models;
    };
    models.retain(|model| model != default);
    models.insert(0, default.to_owned());
    models
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A provider's default model leads the listed models whether the server listed it or not, and a provider without one keeps the server's order.
    #[test]
    fn the_default_model_leads_the_list() {
        let listed = vec!["a".to_owned(), "kimi-for-coding".to_owned(), "b".to_owned()];
        assert_eq!(
            default_first(listed.clone(), Some("kimi-for-coding")),
            ["kimi-for-coding", "a", "b"]
        );
        assert_eq!(
            default_first(listed.clone(), Some("c")),
            ["c", "a", "kimi-for-coding", "b"]
        );
        assert_eq!(default_first(listed.clone(), None), listed);
    }
}
