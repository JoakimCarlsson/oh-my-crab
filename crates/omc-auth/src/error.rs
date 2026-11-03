//! Error type for login, storage and refresh.

use thiserror::Error;

/// Everything that can go wrong while logging in or loading credentials.
#[derive(Debug, Error)]
pub enum AuthError {
    /// The local callback server failed or the browser flow was aborted.
    #[error("login callback failed: {0}")]
    Callback(String),
    /// The token endpoint rejected a request or the response was unusable.
    #[error("token request failed: {0}")]
    Token(String),
    /// Reading or writing `auth.json` failed.
    #[error("credential storage failed: {0}")]
    Storage(String),
    /// The provider needs an API key and none of its environment variables is set.
    #[error("no API key found; set {0}")]
    MissingKey(String),
    /// No provider has been logged in yet.
    #[error("not logged in; use /login")]
    NotLoggedIn,
}
