//! The OpenAI API over the Responses API, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the OpenAI provider; `OMC_OPENAI_URL` points it at another host.
///
/// It speaks the Responses API, which takes tools and reasoning together on
/// every current model and streams reasoning summaries.
pub(super) static DEF: Def = Def {
    id: "openai",
    label: "OpenAI",
    wire: Wire::OpenAiResponses,
    default_model: Some("gpt-6.1-terra"),
    auth: Auth::ApiKey {
        env: &["OPENAI_API_KEY"],
    },
    base_url: "https://api.openai.com/v1",
    base_url_env: Some("OMC_OPENAI_URL"),
    headers: &[],
    replay_reasoning: false,
};
