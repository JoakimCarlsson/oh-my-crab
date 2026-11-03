//! The supported login providers and their OAuth endpoints.

use serde::{Deserialize, Serialize};

/// Static description of a provider's OAuth endpoints and parameters.
pub struct Spec {
    /// Public OAuth client id.
    pub client_id: &'static str,
    /// Browser authorization endpoint.
    pub authorize_url: &'static str,
    /// Token endpoint used for exchange and refresh.
    pub token_url: &'static str,
    /// Requested scopes.
    pub scopes: &'static [&'static str],
    /// Extra query parameters on the authorization URL.
    pub authorize_params: &'static [(&'static str, &'static str)],
    /// Local callback port.
    pub port: u16,
    /// Local callback path.
    pub path: &'static str,
    /// Redirect URI registered with the provider.
    pub redirect_uri: &'static str,
    /// Whether the token endpoint expects a JSON body instead of a form.
    pub json_body: bool,
    /// Whether the authorization `state` is echoed in the code exchange.
    pub state_in_exchange: bool,
    /// Extra headers sent on refresh requests.
    pub refresh_headers: &'static [(&'static str, &'static str)],
    /// Whether the account id is read from the access token's JWT claims.
    pub account_from_jwt: bool,
}

/// A supported login provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    /// Anthropic Claude via OAuth.
    Claude,
    /// OpenAI Codex via ChatGPT OAuth.
    Codex,
}

impl Provider {
    /// Every provider shown in the login picker, in display order.
    pub const ALL: &'static [Provider] = &[Provider::Claude, Provider::Codex];

    /// Returns the human-readable name shown in the UI.
    pub fn label(self) -> &'static str {
        match self {
            Provider::Claude => "Claude (Anthropic OAuth)",
            Provider::Codex => "Codex (OpenAI/ChatGPT OAuth)",
        }
    }

    /// Returns the OAuth description for this provider.
    pub(crate) fn spec(self) -> &'static Spec {
        match self {
            Provider::Claude => &CLAUDE,
            Provider::Codex => &CODEX,
        }
    }
}

/// OAuth endpoints and parameters for Claude.
static CLAUDE: Spec = Spec {
    client_id: "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
    authorize_url: "https://claude.ai/oauth/authorize",
    token_url: "https://api.anthropic.com/v1/oauth/token",
    scopes: &[
        "org:create_api_key",
        "user:profile",
        "user:inference",
        "user:sessions:claude_code",
        "user:mcp_servers",
        "user:file_upload",
    ],
    authorize_params: &[("code", "true")],
    port: 54545,
    path: "/callback",
    redirect_uri: "http://localhost:54545/callback",
    json_body: true,
    state_in_exchange: true,
    refresh_headers: &[
        ("anthropic-beta", "oauth-2025-04-20"),
        (
            "user-agent",
            "anthropic-sdk-typescript/0.112.1 userOAuthProvider",
        ),
    ],
    account_from_jwt: false,
};

/// OAuth endpoints and parameters for Codex.
static CODEX: Spec = Spec {
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
