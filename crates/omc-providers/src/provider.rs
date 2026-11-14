//! What a provider is: a definition naming its wire and sign-in, and the calls that act on one.

use omc_auth::{Auth, AuthError, Credentials, LoginPrompt};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::custom::Custom;
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
    /// The Anthropic Messages API as Amazon Bedrock serves it, through `InvokeModelWithResponseStream`.
    Bedrock,
    /// The Anthropic Messages API as Google Vertex AI serves it, through `streamRawPredict`.
    Vertex,
    /// Google's Gemini API, through `streamGenerateContent`.
    Gemini,
}

impl Wire {
    /// Whether the wire carries the Anthropic Messages API, on Anthropic's own host or a cloud's.
    pub fn is_anthropic(self) -> bool {
        matches!(self, Wire::AnthropicMessages | Wire::Bedrock | Wire::Vertex)
    }
}

/// Everything that distinguishes one provider from another, kept in one place.
pub struct Def {
    /// Stable identifier used on the command line and in saved files, such as `claude` or `kimi-code`.
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
    /// Whether the model's reasoning goes back as `reasoning_content` on the assistant
    /// messages of later requests, which hosts of thinking models such as Kimi and
    /// DeepSeek require around tool calls; only the Chat Completions wire reads it.
    pub replay_reasoning: bool,
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
    /// Every provider a conversation can be opened with: the built-in ones of [`Provider::ALL`] in their order, then the ones the user defined in configuration in the order they were written.
    pub fn all() -> Vec<Provider> {
        Self::ALL
            .iter()
            .copied()
            .chain(Custom::all().map(Provider::Custom))
            .collect()
    }

    /// Returns the provider whose identifier is `id`, built in or defined by the user, when there is one.
    pub fn from_id(id: &str) -> Option<Provider> {
        Self::all().into_iter().find(|p| p.def().id == id)
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

impl Serialize for Provider {
    /// Writes the provider as its id.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.def().id)
    }
}

impl<'de> Deserialize<'de> for Provider {
    /// Reads a provider from its id.
    ///
    /// # Errors
    ///
    /// Fails when no provider, built in or defined by the user, has the id read.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Provider::from_id(&id)
            .ok_or_else(|| serde::de::Error::custom(format!("no provider is called {id}")))
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
        let read: Provider = serde_json::from_str("\"kimi-code\"").unwrap();
        assert_eq!(read, Provider::KimiCode);
        assert!(serde_json::from_str::<Provider>("\"nope\"").is_err());
    }

    /// The built-in providers come first in their own order, and the new hosts are among them with their keys and addresses.
    #[test]
    fn registers_the_built_in_providers() {
        let all = Provider::all();
        assert_eq!(&all[..Provider::ALL.len()], Provider::ALL);
        let ids: Vec<&str> = Provider::ALL.iter().map(|p| p.def().id).collect();
        for id in [
            "claude",
            "codex",
            "ollama",
            "openrouter",
            "copilot",
            "kimi-code",
            "moonshot",
            "zai-coding",
            "zai",
            "minimax",
            "deepseek",
            "xai",
            "groq",
            "cerebras",
            "mistral",
            "openai",
            "gemini",
            "together",
            "fireworks",
            "lmstudio",
            "llamacpp",
            "vllm",
            "anthropic",
            "bedrock",
            "vertex",
            "azure",
        ] {
            assert!(ids.contains(&id), "{id} is not registered");
        }
        let openai = Provider::from_id("openai").unwrap().def();
        assert_eq!(openai.wire, Wire::OpenAiResponses);
        let azure = Provider::from_id("azure").unwrap().def();
        assert_eq!(azure.wire, Wire::OpenAiResponses);
        assert!(matches!(azure.auth, Auth::ApiKey { env } if env == ["AZURE_OPENAI_API_KEY"]));
        let kimi = Provider::from_id("kimi-code").unwrap().def();
        assert_eq!(kimi.default_model, Some("kimi-for-coding"));
        assert!(
            matches!(kimi.auth, Auth::DeviceCodeOrKey { env, .. } if env.contains(&"KIMI_CODE_API_KEY"))
        );
        for provider in Provider::ALL {
            let def = provider.def();
            assert!(!def.label.is_empty());
            assert!(def.base_url.starts_with("http"), "{}", def.id);
            assert!(!def.base_url.ends_with('/'), "{}", def.id);
        }
    }
}
