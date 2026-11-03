//! A tool server that is an HTTP address.

use omc_wire::Reach;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::Mutex;

/// The header that carries the session the server opened.
const SESSION: &str = "mcp-session-id";

/// A connection to a server that answers POST requests.
pub(crate) struct Http {
    /// The client requests are made with.
    client: reqwest::Client,
    /// Where requests go.
    url: String,
    /// What every request carries.
    headers: HeaderMap,
    /// The session the server opened, once it has.
    session: Mutex<Option<String>>,
}

impl Http {
    /// A connection to the address `reach` describes.
    ///
    /// # Errors
    ///
    /// Fails with the reason when `reach` is not an address or a header is not valid.
    pub(crate) fn new(reach: &Reach) -> Result<Self, String> {
        let Reach::Http { url, headers } = reach else {
            return Err("not an address".to_owned());
        };
        let mut map = HeaderMap::new();
        map.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/event-stream"),
        );
        map.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?,
                HeaderValue::from_str(value).map_err(|error| error.to_string())?,
            );
        }
        Ok(Self {
            client: reqwest::Client::new(),
            url: url.clone(),
            headers: map,
            session: Mutex::new(None),
        })
    }

    /// Posts `message` and returns what the server answered, as text.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot be reached or answers with a failure status.
    async fn post(&self, message: &Value) -> Result<String, String> {
        let mut request = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .json(message);
        if let Some(session) = self.session.lock().await.as_deref() {
            request = request.header(SESSION, session);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        if !response.status().is_success() {
            return Err(format!("the tool server answered {}", response.status()));
        }
        if let Some(session) = response
            .headers()
            .get(SESSION)
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock().await = Some(session.to_owned());
        }
        response.text().await.map_err(|error| error.to_string())
    }

    /// Sends the notification `method`, which has no answer.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot be reached.
    pub(crate) async fn notify(&self, message: &Value) -> Result<(), String> {
        self.post(message).await.map(|_| ())
    }

    /// Sends the request `method` under `id` and returns its result.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the server cannot be reached or answers with an error.
    pub(crate) async fn request(
        &self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, String> {
        let body = self
            .post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let message = messages(&body)
            .into_iter()
            .find(|message| message["id"] == json!(id))
            .ok_or("the tool server did not answer")?;
        crate::answer(&message)
    }
}

/// The messages a response body holds: one JSON document, or the `data:` lines of an event stream.
fn messages(body: &str) -> Vec<Value> {
    if let Ok(message) = serde_json::from_str::<Value>(body) {
        return vec![message];
    }
    body.lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .filter_map(|data| serde_json::from_str(data.trim()).ok())
        .collect()
}

#[cfg(test)]
/// Tests for reading what a server answers.
mod tests {
    use super::messages;

    /// A plain document and an event stream both give their messages.
    #[test]
    fn both_shapes() {
        assert_eq!(messages(r#"{"id":1}"#).len(), 1);
        let stream = "event: message\ndata: {\"id\":2}\n\ndata: {\"id\":3}\n";
        assert_eq!(messages(stream).len(), 2);
    }
}
