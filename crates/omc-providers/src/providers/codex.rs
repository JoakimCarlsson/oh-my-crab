//! OpenAI Codex, signed in with ChatGPT OAuth.

use omc_auth::{Auth, Spec};

use crate::provider::{Def, Wire};

/// The definition of the Codex provider.
pub(super) static DEF: Def = Def {
    id: "codex",
    label: "Codex",
    wire: Wire::OpenAiResponses,
    default_model: None,
    auth: Auth::OAuth(&SPEC),
    base_url: "https://chatgpt.com/backend-api/codex",
    base_url_env: None,
    headers: &[],
};

/// OAuth endpoints and parameters for Codex.
static SPEC: Spec = Spec {
    client_id: "app_EMoamEEZ73f0CkXaXp7hrann",
    authorize_url: "https://auth.openai.com/oauth/authorize",
    token_url: "https://auth.openai.com/oauth/token",
    scopes: &[
        "openid",
        "profile",
        "email",
        "offline_access",
        "api.connectors.read",
        "api.connectors.invoke",
    ],
    authorize_params: &[
        ("id_token_add_organizations", "true"),
        ("codex_cli_simplified_flow", "true"),
        ("originator", "omc"),
    ],
    port: 1455,
    path: "/auth/callback",
    redirect_uri: "http://localhost:1455/auth/callback",
    json_body: false,
    state_in_exchange: false,
    refresh_headers: &[],
    account_from_jwt: true,
};
