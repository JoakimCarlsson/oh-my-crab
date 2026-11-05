//! What a provider is: a definition naming its wire and sign-in, and the calls that act on one.

use omc_auth::{Auth, AuthError, Credentials, LoginPrompt};

pub use crate::providers::Provider;

/// The wire protocol a provider speaks; one streaming implementation exists per protocol.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Wire {
    /// The Anthropic Messages API.
    AnthropicMessages,
    /// The OpenAI Responses API as served by the ChatGPT backend.
    OpenAiResponses,
    /// The Chat Completions API, which many hosts and local servers copy.
    ChatCompletions,
}

/// Everything that distinguishes one provider from another, kept in one place.
pub struct Def {
    /// Stable identifier used on the command line and in saved files; equals the variant name in lowercase.
    pub id: &'static str,
    /// The name a client shows for the provider, such as `GitHub Copilot`.
    pub label: &'static str,
    /// The wire protocol the provider speaks.
    pub wire: Wire,
    /// Model used when the user has not picked one, or `None` to ask the backend.
    pub default_model: Option<&'static str>,
    /// How the provider is signed in to.
    pub auth: Auth,
    /// Address of the provider's API, used by wires that serve many hosts.
    pub base_url: &'static str,
    /// Environment variable that overrides `base_url` when set.
    pub base_url_env: Option<&'static str>,
    /// Headers sent on every request to the provider.
    pub headers: &'static [(&'static str, &'static str)],
}

impl Def {
    /// Returns the API address to use: the override from the environment when set, else `base_url`, without a trailing slash.
    pub fn base_url(&self) -> String {
        self.base_url_env
            .and_then(|name| std::env::var(name).ok())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| self.base_url.to_string())
            .trim_end_matches('/')
            .to_string()
    }
}

impl Provider {
    /// Returns the provider whose identifier is `id`, when there is one.
    pub fn from_id(id: &str) -> Option<Provider> {
        Self::ALL.iter().copied().find(|p| p.def().id == id)
    }

    /// Returns the human-readable name shown in the UI.
    pub fn label(self) -> &'static str {
        self.def().label
    }

    /// Returns the wire protocol this provider speaks.
    pub fn wire(self) -> Wire {
        self.def().wire
    }
}

/// Signs in to `provider` and stores the result as active.
///
/// `notify` is told what the user has to do: open an address, or open a page and enter a code.
///
/// # Errors
///
/// Returns [`AuthError`] when the flow fails, a needed key is not set, or the
/// credentials cannot be saved.
pub async fn login(provider: Provider, notify: impl Fn(LoginPrompt)) -> Result<(), AuthError> {
    let def = provider.def();
    omc_auth::login(def.id, &def.auth, notify).await
}

/// Returns credentials for `provider` that are valid right now.
///
/// # Errors
///
/// Returns [`AuthError::NotLoggedIn`] when nothing is stored for `provider`, or
/// another [`AuthError`] when a key is missing or a refresh fails.
pub async fn credentials_for(provider: Provider) -> Result<Credentials, AuthError> {
    let def = provider.def();
    omc_auth::credentials_for(def.id, &def.auth).await
}

/// Returns the provider messages are currently sent to, if any.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when the stored logins cannot be read.
pub fn active_provider() -> Result<Option<Provider>, AuthError> {
    Ok(omc_auth::active_id()?.and_then(|id| Provider::from_id(&id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every provider's id is its saved form, is unique, and finds the provider again.
    #[test]
    fn ids_match_the_saved_form() {
        let mut seen = std::collections::HashSet::new();
        for provider in Provider::ALL {
            let saved = serde_json::to_string(provider).unwrap();
            assert_eq!(saved, format!("\"{}\"", provider.def().id));
            assert_eq!(Provider::from_id(provider.def().id), Some(*provider));
            assert!(seen.insert(provider.def().id));
        }
        assert_eq!(Provider::from_id("nope"), None);
    }
}
