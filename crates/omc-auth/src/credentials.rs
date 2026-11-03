//! The OAuth credentials stored for one provider.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Stored OAuth credentials for one provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    /// Bearer access token.
    pub access: String,
    /// Refresh token.
    pub refresh: String,
    /// Unix time in milliseconds after which the access token needs refreshing.
    pub expires_at_ms: u64,
    /// Provider account identifier, when the provider uses one.
    pub account_id: Option<String>,
    /// Account email address, when the provider reported one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Organization the token is scoped to, fixed at login.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_id: Option<String>,
    /// Display name of that organization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub org_name: Option<String>,
    /// API address the account is served from, when the provider assigns one per account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

impl Credentials {
    /// Credentials that never expire and carry `key` as the bearer token; empty for a server that needs none.
    pub fn static_key(key: String) -> Self {
        Self {
            access: key,
            refresh: String::new(),
            expires_at_ms: u64::MAX,
            account_id: None,
            email: None,
            org_id: None,
            org_name: None,
            base_url: None,
        }
    }

    /// Returns whether the access token has expired or is about to.
    pub fn is_expired(&self) -> bool {
        self.expires_at_ms <= now_ms()
    }
}

/// Returns the current Unix time in milliseconds.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}
