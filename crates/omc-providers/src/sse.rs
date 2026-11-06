//! Shared helpers for reading server-sent event responses.

use std::pin::Pin;

use eventsource_stream::{Event, EventStreamError, Eventsource};
use futures_util::{Stream, StreamExt};

use crate::ProviderError;

/// A boxed stream of server-sent events.
pub type EventStream = Pin<Box<dyn Stream<Item = Result<Event, ProviderError>> + Send>>;

/// Turns a successful `response` into a stream of events.
///
/// A connection that fails while the events stream in yields [`ProviderError::Http`], so it can be told apart from an error the provider reports.
///
/// # Errors
///
/// Returns [`ProviderError::Status`] when the response is not a success.
pub async fn events(response: reqwest::Response) -> Result<EventStream, ProviderError> {
    if !response.status().is_success() {
        return Err(ProviderError::from_response(response).await);
    }
    let stream = response
        .bytes_stream()
        .eventsource()
        .map(|event| event.map_err(stream_error));
    Ok(Box::pin(stream))
}

/// The error a broken event stream stands for: the connection's own error when it failed, and a stream error when what came was not an event stream.
fn stream_error(error: EventStreamError<reqwest::Error>) -> ProviderError {
    match error {
        EventStreamError::Transport(error) => ProviderError::Http(error),
        other => ProviderError::Stream(other.to_string()),
    }
}
