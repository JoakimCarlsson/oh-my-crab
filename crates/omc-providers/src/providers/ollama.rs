//! A local Ollama server, which needs no sign-in.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Ollama provider; `OMC_OLLAMA_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "ollama",
    label: "Ollama",
    wire: Wire::ChatCompletions,
    default_model: None,
    auth: Auth::None,
    base_url: "http://localhost:11434/v1",
    base_url_env: Some("OMC_OLLAMA_URL"),
    headers: &[],
};
