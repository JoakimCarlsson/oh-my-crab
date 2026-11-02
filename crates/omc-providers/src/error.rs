//! Error type for chat requests.

use thiserror::Error;

/// Everything that can go wrong while sending a message.
#[derive(Debug, Error)]
pub enum ProviderError {
    /// The request could not be sent or the connection failed.
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// The provider answered with a non-success status.
    #[error("provider returned {status}: {body}")]
    Status {
        /// HTTP status line.
        status: String,
        /// Response body.
        body: String,
    },
    /// The event stream was malformed or the provider reported an error in it.
    #[error("stream error: {0}")]
    Stream(String),
    /// The provider reported no models for this account.
    #[error("no models available for this account; pick one with /model <id>")]
    NoModels,
}
