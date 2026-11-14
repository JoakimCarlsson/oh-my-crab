//! Azure OpenAI over the Responses API, signed in with an API key from the environment.
//!
//! The resource is named by `AZURE_OPENAI_ENDPOINT`, `AZURE_OPENAI_RESOURCE_NAME`
//! or `AZURE_OPENAI_BASE_URL`, the API version by `AZURE_OPENAI_API_VERSION`
//! and the deployments offered as models by `AZURE_OPENAI_DEPLOYMENT`; the
//! Responses wire reads them on every request.

use omc_auth::Auth;

use crate::provider::{Def, Wire};

/// The definition of the Azure OpenAI provider; its address is read from the environment when a request is sent, and `base_url` is only a placeholder.
pub(super) static DEF: Def = Def {
    id: "azure",
    label: "Azure OpenAI",
    wire: Wire::OpenAiResponses,
    default_model: None,
    auth: Auth::ApiKey {
        env: &["AZURE_OPENAI_API_KEY"],
    },
    base_url: "https://resource.openai.azure.com/openai/v1",
    base_url_env: Some("AZURE_OPENAI_BASE_URL"),
    headers: &[],
    replay_reasoning: false,
};
