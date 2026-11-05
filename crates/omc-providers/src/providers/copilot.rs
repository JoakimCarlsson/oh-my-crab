//! GitHub Copilot, signed in with GitHub's device-code flow.

use omc_auth::{Auth, Device};

use crate::provider::{Def, Wire};

/// The definition of the Copilot provider.
pub(super) static DEF: Def = Def {
    id: "copilot",
    label: "GitHub Copilot",
    wire: Wire::ChatCompletions,
    default_model: None,
    auth: Auth::DeviceCode(&DEVICE),
    base_url: "https://api.githubcopilot.com",
    base_url_env: None,
    headers: &[
        ("User-Agent", "copilot/1.0.82"),
        ("Editor-Version", "copilot/1.0.82"),
        ("Copilot-Integration-Id", "copilot-chat"),
        ("Openai-Intent", "conversation-agent"),
        ("X-GitHub-Api-Version", "2026-08-01"),
    ],
};

/// Device-code login for GitHub Copilot, using the minimal-grant OAuth app.
static DEVICE: Device = Device {
    client_id: "Ov23li8tweQw6odWQebz",
    scope: "read:user",
    code_url: "https://github.com/login/device/code",
    token_url: "https://github.com/login/oauth/access_token",
    headers: &[("User-Agent", "copilot-developer-action/0.0.1")],
    endpoint_url: Some("https://api.github.com/copilot_internal/user"),
};
