//! Error type for chat requests.

use std::time::Duration;

use reqwest::header::HeaderMap;
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
        /// How long the provider asked to be left alone before the request is sent again, when it said.
        retry_after: Option<Duration>,
    },
    /// The event stream was malformed or the provider reported an error in it.
    #[error("stream error: {0}")]
    Stream(String),
    /// The stream stopped before the provider said the reply was complete, as when the connection drops.
    #[error("the connection closed before the reply was complete")]
    Unfinished,
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
            ProviderError::Http(_) | ProviderError::Unfinished | ProviderError::NoModels => {
                return false;
            }
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

    /// Whether the failure is likely to pass, so the same request is worth sending again after a wait: the provider is overloaded, rate limited or failing inside, or the connection dropped or the stream ended early.
    ///
    /// Refusals that waiting does not cure are not: a sign-in problem, a spent quota, billing or credits, a request the provider will not take, and a context overflow, which has its own cure.
    pub fn is_transient(&self) -> bool {
        if self.is_context_overflow() {
            return false;
        }
        match self {
            ProviderError::Status { status, body, .. } => {
                let text = body.to_lowercase();
                let code: u16 = status
                    .split_whitespace()
                    .next()
                    .and_then(|code| code.parse().ok())
                    .unwrap_or(0);
                let busy = matches!(code, 408 | 425 | 429) || (500..600).contains(&code);
                busy && !mentions(&text, &PERMANENT_PHRASES)
            }
            ProviderError::Stream(message) => {
                let text = message.to_lowercase();
                mentions(&text, &TRANSIENT_PHRASES) && !mentions(&text, &PERMANENT_PHRASES)
            }
            ProviderError::Http(error) => {
                error.is_timeout()
                    || error.is_connect()
                    || error.is_request()
                    || error.is_body()
                    || error.is_decode()
            }
            ProviderError::Unfinished => true,
            ProviderError::NoModels => false,
        }
    }

    /// How long the provider asked to wait before the request is sent again, when it said.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            ProviderError::Status { retry_after, .. } => *retry_after,
            _ => None,
        }
    }

    /// The error a response with a non-success status stands for: its status line, its body and the wait its headers ask for.
    pub(crate) async fn from_response(response: reqwest::Response) -> Self {
        let status = response.status().to_string();
        let retry_after = retry_after(response.headers());
        let body = response.text().await.unwrap_or_default();
        ProviderError::Status {
            status,
            body,
            retry_after,
        }
    }
}

/// Phrases, in lower case, of failures that go away on their own: overload, rate limits, server faults and dropped connections.
const TRANSIENT_PHRASES: [&str; 26] = [
    "overloaded",
    "rate limit",
    "rate_limit",
    "too many requests",
    "server error",
    "server_error",
    "internal error",
    "internal_error",
    "service unavailable",
    "temporarily unavailable",
    "server_busy",
    "at capacity",
    "high demand",
    "try again",
    "retry your request",
    "timed out",
    "timeout",
    "connection reset",
    "connection closed",
    "connection refused",
    "connection error",
    "broken pipe",
    "unexpected eof",
    "stream ended",
    "stream disconnected",
    "stream was interrupted",
];

/// Phrases, in lower case, of refusals that waiting does not cure, such as a spent quota or missing credits, even when they come with a status that usually means a short wait.
const PERMANENT_PHRASES: [&str; 11] = [
    "insufficient_quota",
    "insufficient quota",
    "exceeded your current quota",
    "quota exceeded",
    "billing",
    "insufficient credits",
    "insufficient_credits",
    "credit balance",
    "out of budget",
    "usage_limit_reached",
    "usage limit",
];

/// Whether `text` holds any of `phrases`.
fn mentions(text: &str, phrases: &[&str]) -> bool {
    phrases.iter().any(|phrase| text.contains(phrase))
}

/// The wait `headers` ask for before a request is sent again: the longer of `retry-after-ms`, in milliseconds, and `retry-after`, in seconds. A `retry-after` given as a date is not read, and neither is a value that is not a positive number.
fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let number = |name: &str| {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| *value > 0.0)
    };
    let millis = number("retry-after-ms").map(|ms| seconds_to_wait(ms / 1000.0));
    let seconds = number("retry-after").map(seconds_to_wait);
    millis.max(seconds)
}

/// The wait of `seconds`, a positive number, held at the longest [`Duration`] when it is too long, or infinite, to be one.
fn seconds_to_wait(seconds: f64) -> Duration {
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
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
            retry_after: None,
        }
    }

    /// Overload, rate limits, server faults and dropped streams are worth sending again.
    #[test]
    fn transient_failures_are_retried() {
        assert!(status("429 Too Many Requests", "slow down").is_transient());
        assert!(status("500 Internal Server Error", "").is_transient());
        assert!(status("503 Service Unavailable", "").is_transient());
        assert!(status("520 <unknown status code>", "").is_transient());
        assert!(status("524 <unknown status code>", "").is_transient());
        assert!(
            status(
                "529 <unknown status code>",
                r#"{"error":{"type":"overloaded_error"}}"#
            )
            .is_transient()
        );
        assert!(ProviderError::Stream("Overloaded".to_owned()).is_transient());
        assert!(
            ProviderError::Stream("upstream connection reset by peer".to_owned()).is_transient()
        );
        assert!(ProviderError::Unfinished.is_transient());
    }

    /// Sign-in problems, spent quotas, bad requests and overflows are not worth sending again.
    #[test]
    fn permanent_failures_are_not_retried() {
        assert!(!status("401 Unauthorized", "invalid key").is_transient());
        assert!(!status("403 Forbidden", "").is_transient());
        assert!(!status("400 Bad Request", "invalid tool schema").is_transient());
        assert!(
            !status(
                "429 Too Many Requests",
                r#"{"error":{"code":"insufficient_quota"}}"#
            )
            .is_transient()
        );
        assert!(!status("402 Payment Required", "insufficient credits").is_transient());
        assert!(!status("503 Service Unavailable", "billing hard limit reached").is_transient());
        assert!(!status("413 Payload Too Large", "").is_transient());
        assert!(!ProviderError::Stream("prompt is too long".to_owned()).is_transient());
        assert!(!ProviderError::Stream("invalid_request_error".to_owned()).is_transient());
        assert!(!ProviderError::NoModels.is_transient());
    }

    /// The wait asked for is read from either header, the longer one winning, and nonsense is ignored.
    #[test]
    fn reads_the_wait_asked_for() {
        let headers = |pairs: &[(&'static str, &'static str)]| {
            let mut map = HeaderMap::new();
            for (name, value) in pairs {
                map.insert(*name, value.parse().unwrap());
            }
            map
        };
        assert_eq!(
            retry_after(&headers(&[("retry-after", "7")])),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after(&headers(&[("retry-after-ms", "1500")])),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            retry_after(&headers(&[
                ("retry-after", "2"),
                ("retry-after-ms", "2500")
            ])),
            Some(Duration::from_millis(2500))
        );
        assert_eq!(
            retry_after(&headers(&[(
                "retry-after",
                "Wed, 21 Oct 2015 07:28:00 GMT"
            )])),
            None
        );
        assert_eq!(retry_after(&headers(&[("retry-after", "-3")])), None);
        assert_eq!(retry_after(&headers(&[])), None);
    }

    /// A wait too long to hold, infinite or not a number is read without panicking: the first two as the longest wait, the last not at all.
    #[test]
    fn reads_absurd_waits_without_panicking() {
        let headers = |name: &'static str, value: &'static str| {
            let mut map = HeaderMap::new();
            map.insert(name, value.parse().unwrap());
            map
        };
        assert_eq!(
            retry_after(&headers("retry-after", "1e20")),
            Some(Duration::MAX)
        );
        assert_eq!(
            retry_after(&headers("retry-after-ms", "1e300")),
            Some(Duration::MAX)
        );
        assert_eq!(
            retry_after(&headers("retry-after", "inf")),
            Some(Duration::MAX)
        );
        assert_eq!(retry_after(&headers("retry-after", "-inf")), None);
        assert_eq!(retry_after(&headers("retry-after", "NaN")), None);
        assert_eq!(retry_after(&headers("retry-after", "-1e20")), None);
        assert_eq!(
            retry_after(&headers("retry-after", "Fri, 31 Dec 9999 23:59:59 GMT")),
            None
        );
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
