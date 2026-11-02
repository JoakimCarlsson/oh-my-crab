//! Shared helpers for reading server-sent event responses.

use std::pin::Pin;

use eventsource_stream::{Event, Eventsource};
use futures_util::{Stream, StreamExt};

use crate::ProviderError;

/// A boxed stream of server-sent events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, ProviderError>> + Send>>;

/// Turns a successful `response` into a stream of events.
///
/// # Errors
///
/// Returns [`ProviderError::Status`] when the response is not a success.
pub async fn events(response: reqwest::Response) -> Result<EventStream, ProviderError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(ProviderError::Status {
            status: status.to_string(),
            body,
        });
    }
    let stream = response
        .bytes_stream()
        .eventsource()
        .map(|event| event.map_err(|e| ProviderError::Stream(e.to_string())));
    Ok(Box::pin(stream))
}
