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

/// Phrases providers use, in lower case, when a request holds more than the model's context window takes.
const OVERFLOW_PHRASES: [&str; 21] = [
    "exceed context limit",
    "prompt is too long",
    "prompt too long",
    "prompt exceeds max length",
    "request_too_large",
    "input is too long",
    "exceeds the context window",
    "maximum context length",
    "context window exceeded",
    "context window exceeds limit",
    "context_length_exceeded",
    "context length exceeded",
    "model_context_window_exceeded",
    "exceeds the available context size",
    "greater than the context length",
    "longer than the model's context length",
    "maximum prompt length",
    "maximum allowed input length",
    "reduce the length of the messages",
    "exceeded model token limit",
    "token limit exceeded",
];

/// Phrases, in lower case, that mean an overflow only in an error that also speaks of tokens, since they are used for other limits too.
const TOKEN_OVERFLOW_PHRASES: [&str; 2] = ["too many tokens", "exceeds the limit of"];

/// Phrases, in lower case, of errors that only look like an overflow, such as rate limits that count tokens.
const NOT_OVERFLOW_PHRASES: [&str; 4] = [
    "rate limit",
    "too many requests",
    "rate_limit",
    "per minute",
];

impl ProviderError {
    /// Whether the provider refused the request because it holds more than the model's context window takes, which compacting the history can cure.
    ///
    /// A `413` status always counts; otherwise the error's text is matched against the phrases providers use for it, leaving out rate limits and limits on the reply's length.
    pub fn is_context_overflow(&self) -> bool {
        let text = match self {
            ProviderError::Status { status, .. } if status.starts_with("413") => return true,
            ProviderError::Status { status, .. } if status.starts_with("429") => return false,
            ProviderError::Status { body, .. } => body.to_lowercase(),
            ProviderError::Stream(message) => message.to_lowercase(),
            ProviderError::Http(_) | ProviderError::NoModels => return false,
        };
        !NOT_OVERFLOW_PHRASES
            .iter()
            .any(|phrase| text.contains(phrase))
            && (OVERFLOW_PHRASES.iter().any(|phrase| text.contains(phrase))
                || text.contains("token")
                    && TOKEN_OVERFLOW_PHRASES
                        .iter()
                        .any(|phrase| text.contains(phrase)))
    }
}

#[cfg(test)]
/// Tests for recognising a context overflow.
mod tests {
    use super::*;

    /// Returns a status error with `status` and `body`.
    fn status(status: &str, body: &str) -> ProviderError {
        ProviderError::Status {
            status: status.to_owned(),
            body: body.to_owned(),
        }
    }

    /// The overflow messages of the main providers count, and so does any 413.
    #[test]
    fn recognises_overflows() {
        assert!(
            status(
                "400 Bad Request",
                r#"{"error":{"message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#
            )
            .is_context_overflow()
        );
        assert!(
            status(
                "400 Bad Request",
                r#"{"error":{"code":"context_length_exceeded"}}"#
            )
            .is_context_overflow()
        );
        assert!(
            status(
                "400 Bad Request",
                "Your input exceeds the context window of this model."
            )
            .is_context_overflow()
        );
        assert!(
            status(
                "400 Bad Request",
                "input length and `max_tokens` exceed context limit: 190000 + 32000 > 200000"
            )
            .is_context_overflow()
        );
        assert!(
            status(
                "400 Bad Request",
                "prompt has 140000 tokens, which exceeds the limit of 128000"
            )
            .is_context_overflow()
        );
        assert!(status("413 Payload Too Large", "").is_context_overflow());
        assert!(ProviderError::Stream("prompt is too long".to_owned()).is_context_overflow());
    }

    /// Rate limits and other refusals are not overflows.
    #[test]
    fn ignores_other_errors() {
        assert!(
            !status("429 Too Many Requests", "too many tokens per minute").is_context_overflow()
        );
        assert!(
            !status("400 Bad Request", "rate limit: too many tokens per minute")
                .is_context_overflow()
        );
        assert!(!status("400 Bad Request", "image input is not supported").is_context_overflow());
        assert!(
            !status("400 Bad Request", "file size exceeds the limit of 5 MB").is_context_overflow()
        );
        assert!(
            !status("400 Bad Request", "too many tokens per minute, slow down")
                .is_context_overflow()
        );
        assert!(!status("401 Unauthorized", "invalid key").is_context_overflow());
        assert!(!ProviderError::NoModels.is_context_overflow());
    }
}
