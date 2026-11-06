//! Together AI, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Together AI provider; `OMC_TOGETHER_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "together",
    label: "Together AI",
    wire: Wire::ChatCompletions,
    default_model: Some("MiniMaxAI/MiniMax-M3"),
    auth: Auth::ApiKey {
        env: &["TOGETHER_API_KEY"],
    },
    base_url: "https://api.together.ai/v1",
    base_url_env: Some("OMC_TOGETHER_URL"),
    headers: &[],
    replay_reasoning: false,
};
