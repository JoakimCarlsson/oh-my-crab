//! xAI's Grok models, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the xAI provider; `OMC_XAI_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "xai",
    label: "xAI",
    wire: Wire::ChatCompletions,
    default_model: Some("grok-4.7"),
    auth: Auth::ApiKey {
        env: &["XAI_API_KEY"],
    },
    base_url: "https://api.x.ai/v1",
    base_url_env: Some("OMC_XAI_URL"),
    headers: &[],
    replay_reasoning: false,
};
