//! A tool server that is an HTTP address: every message a POST, or the older transport of one long event stream.

use std::collections::HashMap;
use std::sync::Arc;

use futures_util::StreamExt;
use omc_wire::Reach;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

use crate::Failure;
use crate::oauth::{challenge, refresh, resource_of};
use crate::store::{Grant, Tokens};

/// The header that carries the session the server opened.
const SESSION: &str = "mcp-session-id";

/// The token a server's requests carry, kept and refreshed in the store.
pub(crate) struct Bearer {
    /// Where the token is kept.
    tokens: Tokens,
    /// The server's address, which the token is kept under.
    key: String,
    /// The grant as last read or refreshed.
    grant: Mutex<Option<Grant>>,
}

impl Bearer {
    /// The token of the server at `url`, kept in `tokens`.
    pub(crate) fn new(tokens: Tokens, url: &str) -> Self {
        let key = resource_of(url);
        let grant = tokens.get(&key);
        Self {
            tokens,
            key,
            grant: Mutex::new(grant),
        }
    }

    /// The access token to send, refreshed first when it has expired and can be; nothing when there is none.
    async fn token(&self, client: &reqwest::Client) -> Option<String> {
        let mut grant = self.grant.lock().await;
        if grant.is_none() {
            *grant = self.tokens.get(&self.key);
        }
        if grant.as_ref().is_some_and(Grant::expired)
            && let Some(held) = grant.as_ref()
            && held.refresh.is_some()
            && let Ok(fresh) = refresh(client, held).await
        {
            let _ = self.tokens.set(&self.key, fresh.clone());
            *grant = Some(fresh);
        }
        grant.as_ref().map(|grant| grant.access.clone())
    }

    /// Refreshes the token after the server turned `rejected` away; whether a new one was got. A token that cannot be refreshed is forgotten, so the next sign-in starts afresh.
    async fn renew(&self, client: &reqwest::Client, rejected: &str) -> bool {
        let mut grant = self.grant.lock().await;
        let Some(held) = grant.clone() else {
            return false;
        };
        if held.access != rejected {
            return true;
        }
        match refresh(client, &held).await {
            Ok(fresh) => {
                let _ = self.tokens.set(&self.key, fresh.clone());
                *grant = Some(fresh);
                true
            }
            Err(_) => {
                let _ = self.tokens.remove(&self.key);
                *grant = None;
                false
            }
        }
    }
}

/// What every request to an address carries, and how it is signed in.
struct Common {
    /// The client requests are made with.
    client: reqwest::Client,
    /// The headers the setup gives.
    headers: HeaderMap,
    /// The token requests carry, unless the headers carry their own authorization.
    bearer: Option<Arc<Bearer>>,
}

impl Common {
    /// What `reach` says every request carries, signed in with `bearer` unless its headers authorize themselves.
    ///
    /// # Errors
    ///
    /// Fails with the reason when `reach` is not an address or a header is not valid.
    fn new(reach: &Reach, bearer: Option<Arc<Bearer>>) -> Result<(Self, String), String> {
        let Reach::Http { url, headers } = reach else {
            return Err("not an address".to_owned());
        };
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?,
                HeaderValue::from_str(value).map_err(|error| error.to_string())?,
            );
        }
        let bearer = bearer.filter(|_| !map.contains_key(AUTHORIZATION));
        Ok((
            Self {
                client: reqwest::Client::new(),
                headers: map,
                bearer,
            },
            url.clone(),
        ))
    }

    /// Sends what `build` makes, with the token when there is one; after a `401` the token is refreshed and the request sent once more.
    ///
    /// # Errors
    ///
    /// Fails with [`Failure::Unauthorized`] when the server still wants a token, or with the reason when it cannot be reached.
    async fn send(
        &self,
        build: impl Fn() -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, Failure> {
        let mut retried = false;
        loop {
            let token = match &self.bearer {
                Some(bearer) => bearer.token(&self.client).await,
                None => None,
            };
            let mut request = build().headers(self.headers.clone());
            if let Some(token) = &token {
                request = request.bearer_auth(token);
            }
            let response = request
                .send()
                .await
                .map_err(|error| Failure::Other(error.to_string()))?;
            if response.status() != StatusCode::UNAUTHORIZED {
                return Ok(response);
            }
            if let (Some(bearer), Some(token), false) = (&self.bearer, &token, retried)
                && bearer.renew(&self.client, token).await
            {
                retried = true;
                continue;
            }
            let asked = response
                .headers()
                .get(reqwest::header::WWW_AUTHENTICATE)
                .and_then(|value| value.to_str().ok())
                .map(challenge)
                .unwrap_or_default();
            return Err(match &self.bearer {
                Some(_) => Failure::Unauthorized(asked),
                None => Failure::Other(format!("the tool server answered {}", response.status())),
            });
        }
    }
}

/// A connection to a server that answers POST requests.
pub(crate) struct Http {
    /// What every request carries.
    common: Common,
    /// Where requests go.
    url: String,
    /// The session the server opened, once it has.
    session: Mutex<Option<String>>,
}

impl Http {
    /// A connection to the address `reach` describes, signed in with `bearer` when the server wants it.
    ///
    /// # Errors
    ///
    /// Fails with the reason when `reach` is not an address or a header is not valid.
    pub(crate) fn new(reach: &Reach, bearer: Option<Arc<Bearer>>) -> Result<Self, String> {
        let (common, url) = Common::new(reach, bearer)?;
        Ok(Self {
            common,
            url,
            session: Mutex::new(None),
        })
    }

    /// Posts `message` and returns what the server answered, as text.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, wants a token, or answers with a failure status.
    async fn post(&self, message: &Value) -> Result<String, Failure> {
        let session = self.session.lock().await.clone();
        let response = self
            .common
            .send(|| {
                let mut request = self
                    .common
                    .client
                    .post(&self.url)
                    .header(ACCEPT, "application/json, text/event-stream")
                    .header(CONTENT_TYPE, "application/json")
                    .header("MCP-Protocol-Version", crate::client::VERSION)
                    .json(message);
                if let Some(session) = session.as_deref() {
                    request = request.header(SESSION, session);
                }
                request
            })
            .await?;
        if !response.status().is_success() {
            return Err(Failure::Other(format!(
                "the tool server answered {}",
                response.status()
            )));
        }
        if let Some(session) = response
            .headers()
            .get(SESSION)
            .and_then(|value| value.to_str().ok())
        {
            *self.session.lock().await = Some(session.to_owned());
        }
        response
            .text()
            .await
            .map_err(|error| Failure::Other(error.to_string()))
    }

    /// Sends the notification `message`, which has no answer.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached.
    pub(crate) async fn notify(&self, message: &Value) -> Result<(), Failure> {
        self.post(message).await.map(|_| ())
    }

    /// Sends the request `method` under `id` and returns its result.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, wants a token, or answers with an error.
    pub(crate) async fn request(
        &self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, Failure> {
        let body = self
            .post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let message = messages(&body)
            .into_iter()
            .find(|message| message["id"] == json!(id))
            .ok_or_else(|| Failure::Other("the tool server did not answer".to_owned()))?;
        crate::answer(&message).map_err(Failure::Other)
    }
}

/// The answers waited for on an event stream, by request id.
type Waiters = Arc<std::sync::Mutex<Option<HashMap<u64, oneshot::Sender<Value>>>>>;

/// A connection to a server of the older transport: answers come on one long event stream, which first names where messages are posted.
pub(crate) struct Sse {
    /// What every request carries.
    common: Common,
    /// Where messages are posted.
    endpoint: String,
    /// The requests waiting for their answers; nothing once the stream ended.
    waiting: Waiters,
    /// The task reading the stream, stopped when this is dropped.
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Sse {
    /// Stops reading the stream.
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Sse {
    /// Opens the event stream of the address `reach` describes, signed in with `bearer` when the server wants it, and waits for it to name where messages go.
    ///
    /// # Errors
    ///
    /// Fails when the stream cannot be opened, the server wants a token, or the stream ends before naming the endpoint.
    pub(crate) async fn connect(
        reach: &Reach,
        bearer: Option<Arc<Bearer>>,
    ) -> Result<Self, Failure> {
        let (common, url) = Common::new(reach, bearer).map_err(Failure::Other)?;
        let response = common
            .send(|| common.client.get(&url).header(ACCEPT, "text/event-stream"))
            .await?;
        if !response.status().is_success() {
            return Err(Failure::Other(format!(
                "the tool server answered {}",
                response.status()
            )));
        }
        let waiting: Waiters = Arc::new(std::sync::Mutex::new(Some(HashMap::new())));
        let (named, endpoint) = oneshot::channel();
        let reader = tokio::spawn(read_stream(response, waiting.clone(), named));
        let endpoint = endpoint.await.map_err(|_| {
            Failure::Other("the event stream ended before naming an endpoint".to_owned())
        })?;
        let endpoint = url::Url::parse(&url)
            .and_then(|base| base.join(&endpoint))
            .map_err(|error| Failure::Other(format!("bad endpoint: {error}")))?
            .to_string();
        Ok(Self {
            common,
            endpoint,
            waiting,
            reader,
        })
    }

    /// Posts `message` to the endpoint.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, wants a token, or refuses the message.
    async fn post(&self, message: &Value) -> Result<(), Failure> {
        let response = self
            .common
            .send(|| {
                self.common
                    .client
                    .post(&self.endpoint)
                    .header(CONTENT_TYPE, "application/json")
                    .json(message)
            })
            .await?;
        if response.status().is_success() {
            Ok(())
        } else {
            Err(Failure::Other(format!(
                "the tool server answered {}",
                response.status()
            )))
        }
    }

    /// Sends the notification `message`, which has no answer.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached.
    pub(crate) async fn notify(&self, message: &Value) -> Result<(), Failure> {
        self.post(message).await
    }

    /// Sends the request `method` under `id` and waits for its answer on the stream.
    ///
    /// # Errors
    ///
    /// Fails when the server cannot be reached, the stream ends first, or the answer is an error.
    pub(crate) async fn request(
        &self,
        id: u64,
        method: &str,
        params: Value,
    ) -> Result<Value, Failure> {
        let (answer, answered) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock().unwrap_or_else(|held| held.into_inner());
            let Some(waiting) = waiting.as_mut() else {
                return Err(Failure::Other("the tool server stopped".to_owned()));
            };
            waiting.insert(id, answer);
        }
        let sent = self
            .post(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        if let Err(failure) = sent {
            if let Some(waiting) = self
                .waiting
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .as_mut()
            {
                waiting.remove(&id);
            }
            return Err(failure);
        }
        let message = answered
            .await
            .map_err(|_| Failure::Other("the tool server stopped".to_owned()))?;
        crate::answer(&message).map_err(Failure::Other)
    }
}

/// Reads the event stream `response`: sends the first `endpoint` event's data to `named`, and each answer to the request in `waiting` it belongs to; when the stream ends, the requests still waiting fail.
async fn read_stream(
    response: reqwest::Response,
    waiting: Waiters,
    named: oneshot::Sender<String>,
) {
    let mut named = Some(named);
    let mut stream = response.bytes_stream();
    let mut pending = String::new();
    let mut event = String::new();
    let mut data = String::new();
    while let Some(Ok(chunk)) = stream.next().await {
        pending.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(end) = pending.find('\n') {
            let line = pending[..end].trim_end_matches('\r').to_owned();
            pending.drain(..=end);
            if let Some(name) = line.strip_prefix("event:") {
                event = name.trim().to_owned();
            } else if let Some(more) = line.strip_prefix("data:") {
                if !data.is_empty() {
                    data.push('\n');
                }
                data.push_str(more.strip_prefix(' ').unwrap_or(more));
            } else if line.is_empty() && !data.is_empty() {
                let text = std::mem::take(&mut data);
                if event == "endpoint" {
                    if let Some(named) = named.take() {
                        let _ = named.send(text.trim().to_owned());
                    }
                } else if let Ok(message) = serde_json::from_str::<Value>(&text)
                    && let Some(id) = message["id"].as_u64()
                    && message.get("method").is_none()
                {
                    let sender = waiting
                        .lock()
                        .unwrap_or_else(|held| held.into_inner())
                        .as_mut()
                        .and_then(|waiting| waiting.remove(&id));
                    if let Some(sender) = sender {
                        let _ = sender.send(message);
                    }
                }
                event.clear();
            }
        }
    }
    waiting
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .take();
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
