//! Fireworks AI, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Fireworks AI provider; `OMC_FIREWORKS_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "fireworks",
    label: "Fireworks AI",
    wire: Wire::ChatCompletions,
    default_model: Some("accounts/fireworks/models/kimi-k2-thinking"),
    auth: Auth::ApiKey {
        env: &["FIREWORKS_API_KEY"],
    },
    base_url: "https://api.fireworks.ai/inference/v1",
    base_url_env: Some("OMC_FIREWORKS_URL"),
    headers: &[],
    replay_reasoning: true,
};
