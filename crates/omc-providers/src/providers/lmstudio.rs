//! A local LM Studio server, which needs no sign-in.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the LM Studio provider; `OMC_LMSTUDIO_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "lmstudio",
    label: "LM Studio",
    wire: Wire::ChatCompletions,
    default_model: None,
    auth: Auth::None,
    base_url: "http://localhost:1234/v1",
    base_url_env: Some("OMC_LMSTUDIO_URL"),
    headers: &[],
    replay_reasoning: false,
};
