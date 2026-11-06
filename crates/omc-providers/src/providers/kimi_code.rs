//! Kimi Code, the Kimi For Coding subscription from Moonshot, signed in with an API key from the environment.
//!
//! Kimi Code serves its subscription over an OpenAI-compatible endpoint and an
//! Anthropic-compatible one; crab uses the OpenAI-compatible one, since its
//! Anthropic wire is built for Anthropic's own API. Kimi asks every tool to keep its
//! real client identity, so requests carry crab's own `User-Agent`.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The `User-Agent` sent to Kimi Code, naming crab and its version.
const USER_AGENT: &str = concat!("oh-my-crab/", env!("CARGO_PKG_VERSION"));

/// The definition of the Kimi Code provider; `OMC_KIMI_CODE_URL` points it at another host,
/// such as `https://api.kimi.com/coding/v1` for subscriptions from the China site.
pub(super) static DEF: Def = Def {
    id: "kimi-code",
    label: "Kimi Code",
    wire: Wire::ChatCompletions,
    default_model: Some("kimi-for-coding"),
    auth: Auth::ApiKey {
        env: &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
    },
    base_url: "https://api.kimi.ai/coding/v1",
    base_url_env: Some("OMC_KIMI_CODE_URL"),
    headers: &[("User-Agent", USER_AGENT)],
    replay_reasoning: true,
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Kimi Code is reached over Chat Completions as crab itself, and its thinking goes back with tool calls.
    #[test]
    fn identifies_as_crab() {
        assert_eq!(DEF.wire, Wire::ChatCompletions);
        assert!(DEF.base_url.ends_with("/coding/v1"));
        assert!(
            DEF.headers
                .iter()
                .any(|(name, value)| *name == "User-Agent" && value.starts_with("oh-my-crab/"))
        );
        assert!(DEF.replay_reasoning);
    }
}
