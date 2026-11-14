//! Google Gemini through its native `generateContent` API, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Gemini provider; `OMC_GEMINI_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "gemini",
    label: "Google Gemini",
    wire: Wire::Gemini,
    default_model: Some("gemini-3.8-flash"),
    auth: Auth::ApiKey {
        env: &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
    },
    base_url: "https://generativelanguage.googleapis.com/v1beta",
    base_url_env: Some("OMC_GEMINI_URL"),
    headers: &[],
    replay_reasoning: false,
};
