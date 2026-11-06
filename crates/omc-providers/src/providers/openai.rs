//! The OpenAI API over Chat Completions, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the OpenAI provider; `OMC_OPENAI_URL` points it at another host.
///
/// The default model is one that takes tools over Chat Completions, which OpenAI's
/// largest models do not.
pub(super) static DEF: Def = Def {
    id: "openai",
    label: "OpenAI",
    wire: Wire::ChatCompletions,
    default_model: Some("gpt-6.1-terra"),
    auth: Auth::ApiKey {
        env: &["OPENAI_API_KEY"],
    },
    base_url: "https://api.openai.com/v1",
    base_url_env: Some("OMC_OPENAI_URL"),
    headers: &[],
    replay_reasoning: false,
};
