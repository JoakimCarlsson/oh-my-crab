//! OpenRouter, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the OpenRouter provider; `OMC_OPENROUTER_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "openrouter",
    label: "OpenRouter",
    wire: Wire::ChatCompletions,
    default_model: Some("openrouter/auto"),
    auth: Auth::ApiKey {
        env: &["OPENROUTER_API_KEY"],
    },
    base_url: "https://openrouter.ai/api/v1",
    base_url_env: Some("OMC_OPENROUTER_URL"),
    headers: &[],
    replay_reasoning: false,
};
