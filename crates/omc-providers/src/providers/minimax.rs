//! MiniMax, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the MiniMax provider; `OMC_MINIMAX_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "minimax",
    label: "MiniMax",
    wire: Wire::ChatCompletions,
    default_model: Some("MiniMax-M3"),
    auth: Auth::ApiKey {
        env: &["MINIMAX_API_KEY"],
    },
    base_url: "https://api.minimax.io/v1",
    base_url_env: Some("OMC_MINIMAX_URL"),
    headers: &[],
    replay_reasoning: true,
};
