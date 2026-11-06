//! Mistral's La Plateforme, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Mistral provider; `OMC_MISTRAL_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "mistral",
    label: "Mistral",
    wire: Wire::ChatCompletions,
    default_model: Some("mistral-large-latest"),
    auth: Auth::ApiKey {
        env: &["MISTRAL_API_KEY"],
    },
    base_url: "https://api.mistral.ai/v1",
    base_url_env: Some("OMC_MISTRAL_URL"),
    headers: &[],
    replay_reasoning: false,
};
