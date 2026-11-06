//! Moonshot's Kimi platform API, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Moonshot provider; `OMC_MOONSHOT_URL` points it at another host, such as `https://api.moonshot.cn/v1` for keys from the China platform.
pub(super) static DEF: Def = Def {
    id: "moonshot",
    label: "Moonshot",
    wire: Wire::ChatCompletions,
    default_model: Some("kimi-k2.7-code"),
    auth: Auth::ApiKey {
        env: &["MOONSHOT_API_KEY"],
    },
    base_url: "https://api.moonshot.ai/v1",
    base_url_env: Some("OMC_MOONSHOT_URL"),
    headers: &[],
    replay_reasoning: true,
};
