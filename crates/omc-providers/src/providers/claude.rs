//! Anthropic Claude, signed in with OAuth.

use omc_auth::{Auth, Spec};

use crate::provider::{Def, Wire};

/// The definition of the Claude provider.
pub(super) static DEF: Def = Def {
    id: "claude",
    label: "Claude (Anthropic OAuth)",
    wire: Wire::AnthropicMessages,
    default_model: Some("claude-haiku-5-5"),
    auth: Auth::OAuth(&SPEC),
    base_url: "https://api.anthropic.com",
    base_url_env: None,
    headers: &[],
};

/// OAuth endpoints and parameters for Claude.
static SPEC: Spec = Spec {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The Claude login asks for the scopes, callback and state oh-my-pi uses.
    #[test]
    fn spec_matches_the_reference() {
        assert!(SPEC.scopes.contains(&"user:sessions:claude_code"));
        assert_eq!(SPEC.port, 54545);
        assert_eq!(SPEC.path, "/callback");
        assert!(SPEC.state_in_exchange && SPEC.json_body);
        assert!(SPEC.refresh_headers.iter().any(|(k, _)| *k == "user-agent"));
    }
}
