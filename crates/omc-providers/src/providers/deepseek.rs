//! DeepSeek, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the DeepSeek provider; `OMC_DEEPSEEK_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "deepseek",
    label: "DeepSeek",
    wire: Wire::ChatCompletions,
    default_model: Some("deepseek-v4-pro"),
    auth: Auth::ApiKey {
        env: &["DEEPSEEK_API_KEY"],
    },
    base_url: "https://api.deepseek.com",
    base_url_env: Some("OMC_DEEPSEEK_URL"),
    headers: &[],
    replay_reasoning: true,
};
