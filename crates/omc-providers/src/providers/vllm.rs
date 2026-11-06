//! A local vLLM server, which needs no sign-in.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the vLLM provider; `OMC_VLLM_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "vllm",
    label: "vLLM",
    wire: Wire::ChatCompletions,
    default_model: None,
    auth: Auth::None,
    base_url: "http://localhost:8000/v1",
    base_url_env: Some("OMC_VLLM_URL"),
    headers: &[],
    replay_reasoning: false,
};
