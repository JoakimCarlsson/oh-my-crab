//! The Anthropic API, signed in with an API key: Claude without the Claude Code identity a subscription login presents.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Anthropic API provider; `OMC_ANTHROPIC_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "anthropic",
    label: "Anthropic API",
    wire: Wire::AnthropicMessages,
    default_model: Some("claude-sonnet-5-5"),
    auth: Auth::ApiKey {
        env: &["ANTHROPIC_API_KEY"],
    },
    base_url: "https://api.anthropic.com",
    base_url_env: Some("OMC_ANTHROPIC_URL"),
    headers: &[],
    replay_reasoning: false,
};
