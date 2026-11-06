//! Z.ai's GLM Coding Plan, signed in with an API key from the environment.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Z.ai Coding Plan provider; `OMC_ZAI_CODING_URL` points it at another host.
pub(super) static DEF: Def = Def {
    id: "zai-coding",
    label: "Z.ai Coding Plan",
    wire: Wire::ChatCompletions,
    default_model: Some("glm-5.3"),
    auth: Auth::ApiKey {
        env: &["ZAI_API_KEY"],
    },
    base_url: "https://api.z.ai/api/coding/paas/v4",
    base_url_env: Some("OMC_ZAI_CODING_URL"),
    headers: &[],
    replay_reasoning: true,
};
