//! Groq, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Groq provider; `OMC_GROQ_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "groq",
    label: "Groq",
    wire: Wire::ChatCompletions,
    default_model: Some("openai/gpt-oss-120b"),
    auth: Auth::ApiKey {
        env: &["GROQ_API_KEY"],
    },
    base_url: "https://api.groq.com/openai/v1",
    base_url_env: Some("OMC_GROQ_URL"),
    headers: &[],
    replay_reasoning: false,
};
