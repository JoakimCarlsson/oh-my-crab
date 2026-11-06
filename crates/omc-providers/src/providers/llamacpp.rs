//! A local llama.cpp `llama-server`, which needs no sign-in.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the llama.cpp provider; `OMC_LLAMACPP_URL` points it at another host, such as `http://localhost:8080/v1` for builds that listened there.
pub(super) static DEF: Def = Def {
    id: "llamacpp",
    label: "llama.cpp",
    wire: Wire::ChatCompletions,
    default_model: None,
    auth: Auth::None,
    base_url: "http://localhost:9931/v1",
    base_url_env: Some("OMC_LLAMACPP_URL"),
    headers: &[],
    replay_reasoning: false,
};
