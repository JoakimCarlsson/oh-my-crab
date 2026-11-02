//! One-shot local HTTP server that receives the OAuth redirect.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use tiny_http::{Header, Response, Server};

use crate::AuthError;

/// How long to wait for the browser to hit the callback.
const WAIT: Duration = Duration::from_secs(300);

/// Page shown in the browser once the redirect has been received.
const DONE_PAGE: &str =
    "<html><body><h3>Login complete.</h3>You can close this tab and return to crab.</body></html>";

/// Query parameters delivered to the callback.
pub struct CallbackParams {
    /// The authorization code.
    pub code: String,
    /// The echoed `state` value, when present.
    pub state: Option<String>,
}

/// Binds `127.0.0.1:port` and blocks until `path` is requested with a code.
///
/// # Errors
///
/// Returns [`AuthError::Callback`] when the port cannot be bound, the wait times
/// out, or the redirect carries an OAuth error instead of a code.
pub fn wait_for_code(port: u16, path: &str) -> Result<CallbackParams, AuthError> {
    let server = Server::http(("127.0.0.1", port))
        .map_err(|e| AuthError::Callback(format!("cannot bind port {port}: {e}")))?;
    let deadline = Instant::now() + WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(AuthError::Callback("timed out waiting for browser".into()));
        }
        let request = server
            .recv_timeout(remaining)
            .map_err(|e| AuthError::Callback(e.to_string()))?;
        let Some(request) = request else { continue };
        let url = format!("http://localhost{}", request.url());
        let parsed = url::Url::parse(&url).map_err(|e| AuthError::Callback(e.to_string()))?;
        if parsed.path() != path {
            let _ = request.respond(Response::from_string("not found").with_status_code(404));
            continue;
        }
        let params: HashMap<String, String> = parsed.query_pairs().into_owned().collect();
        let html = Header::from_bytes(&b"Content-Type"[..], &b"text/html"[..])
            .map_err(|()| AuthError::Callback("invalid header".into()))?;
        let _ = request.respond(Response::from_string(DONE_PAGE).with_header(html));
        if let Some(error) = params.get("error") {
            return Err(AuthError::Callback(format!(
                "provider returned error: {error}"
            )));
        }
        let Some(code) = params.get("code") else {
            return Err(AuthError::Callback("redirect carried no code".into()));
        };
        return Ok(CallbackParams {
            code: code.clone(),
            state: params.get("state").cloned(),
        });
    }
}
