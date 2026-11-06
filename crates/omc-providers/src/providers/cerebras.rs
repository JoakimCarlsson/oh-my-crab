//! Cerebras, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Cerebras provider; `OMC_CEREBRAS_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "cerebras",
    label: "Cerebras",
    wire: Wire::ChatCompletions,
    default_model: Some("gpt-oss-120b"),
    auth: Auth::ApiKey {
        env: &["CEREBRAS_API_KEY"],
    },
    base_url: "https://api.cerebras.ai/v1",
    base_url_env: Some("OMC_CEREBRAS_URL"),
    headers: &[],
    replay_reasoning: false,
};
