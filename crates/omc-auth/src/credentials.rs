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
}

impl Credentials {
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
