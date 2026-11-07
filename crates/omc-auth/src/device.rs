//! The device-code login: the user enters a short code on a web page while this polls for the token.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::credentials::now_ms;
use crate::method::Device;
use crate::{AuthError, Credentials, LoginPrompt, jwt};

/// Grant type the token endpoint is polled with.
const GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Seconds added to the polling interval each time the endpoint says to slow down.
const SLOW_DOWN_STEP: u64 = 5;

/// The most seconds before an access token expires that it is refreshed.
const REFRESH_AHEAD: u64 = 300;

/// What the endpoint returns when a device code has been issued.
#[derive(Deserialize)]
struct DeviceCode {
    /// Secret the poll is made with.
    device_code: String,
    /// Code the user enters.
    user_code: String,
    /// Page the user enters it on.
    #[serde(default)]
    verification_uri: Option<String>,
    /// Page that carries the code already, when the provider gives one.
    #[serde(default)]
    verification_uri_complete: Option<String>,
    /// Seconds to wait between polls.
    #[serde(default = "default_interval")]
    interval: u64,
    /// Seconds until the code stops working.
    #[serde(default = "default_lifetime")]
    expires_in: u64,
}

/// The polling interval assumed when the endpoint gives none.
fn default_interval() -> u64 {
    5
}

/// The code lifetime assumed when the endpoint gives none.
fn default_lifetime() -> u64 {
    900
}

/// What one poll of the token endpoint said.
#[derive(Debug, PartialEq)]
enum Poll {
    /// The user approved; this is the token response.
    Token(Value),
    /// The user has not approved yet.
    Pending,
    /// Poll less often.
    SlowDown,
    /// The flow failed for this reason.
    Failed(String),
}

/// Reads the endpoint's answer to a poll.
fn classify(body: &Value) -> Poll {
    if body.get("access_token").and_then(Value::as_str).is_some() {
        return Poll::Token(body.clone());
    }
    match body.get("error").and_then(Value::as_str) {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some(error) => {
            let detail = body
                .get("error_description")
                .and_then(Value::as_str)
                .map(|text| format!(": {text}"))
                .unwrap_or_default();
            Poll::Failed(format!("{error}{detail}"))
        }
        None => Poll::Failed("unexpected response from the token endpoint".to_string()),
    }
}

/// The headers of the flow's requests: the static ones of `device`, then those of `identity`.
fn flow_headers<'a>(
    device: &'a Device,
    identity: &'a [(String, String)],
) -> impl Iterator<Item = (&'a str, &'a str)> {
    device.headers.iter().copied().chain(
        identity
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    )
}

/// Posts `form` to `url` with the flow's headers, `device`'s and `identity`, and returns the JSON answer.
///
/// An answer with an error status that carries an OAuth `error` is returned as the answer, since some providers report a pending approval that way.
async fn post(
    client: &reqwest::Client,
    device: &Device,
    identity: &[(String, String)],
    url: &str,
    form: &[(&str, &str)],
) -> Result<Value, AuthError> {
    let mut request = client.post(url).header("Accept", "application/json");
    for (name, value) in flow_headers(device, identity) {
        request = request.header(name, value);
    }
    let response = request
        .form(form)
        .send()
        .await
        .map_err(|e| AuthError::Token(e.to_string()))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<Value>(&body).ok();
    if status.is_success() {
        return parsed.ok_or_else(|| AuthError::Token(format!("{status}: {body}")));
    }
    match parsed {
        Some(answer) if status.is_client_error() && answer.get("error").is_some() => Ok(answer),
        _ => Err(AuthError::Token(format!("{status}: {body}"))),
    }
}

/// Asks the provider which API address serves the account of `token`, when it says.
async fn discover_endpoint(
    client: &reqwest::Client,
    device: &Device,
    token: &str,
) -> Option<String> {
    let url = device.endpoint_url?;
    let mut request = client
        .get(url)
        .header("Accept", "application/json")
        .header("Authorization", format!("token {token}"));
    for (name, value) in device.headers {
        request = request.header(*name, *value);
    }
    let body: Value = request.send().await.ok()?.json().await.ok()?;
    let endpoint = body.pointer("/endpoints/api")?.as_str()?;
    let endpoint = endpoint.trim().trim_end_matches('/');
    endpoint
        .starts_with("https://")
        .then(|| endpoint.to_string())
}

/// When an access token that lives `lifetime` seconds from `now` is refreshed, in Unix milliseconds: [`REFRESH_AHEAD`] seconds before it expires, or halfway through a shorter life.
fn refresh_at(now: u64, lifetime: u64) -> u64 {
    let ahead = REFRESH_AHEAD.min(lifetime / 2);
    now + lifetime.saturating_sub(ahead) * 1000
}

/// The credentials a token response `answer` gives, with `identity` kept as their headers and `previous`, the login being refreshed, filling in what the answer leaves out.
///
/// A response with `expires_in` and a `refresh_token` gives credentials that expire and are refreshed; one without, as GitHub's, gives credentials that never expire, refreshed with the token itself.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the response carries no access token.
fn credentials_from(
    answer: &Value,
    identity: Vec<(String, String)>,
    previous: Option<&Credentials>,
) -> Result<Credentials, AuthError> {
    let access = answer
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or_else(|| AuthError::Token("the token response has no access token".to_string()))?
        .to_string();
    let refresh = answer
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(str::to_string);
    let lifetime = answer.get("expires_in").and_then(Value::as_u64);
    let mut credentials = Credentials::static_key(access.clone());
    match (refresh, lifetime) {
        (Some(refresh), Some(lifetime)) => {
            credentials.refresh = refresh;
            credentials.expires_at_ms = refresh_at(now_ms(), lifetime);
        }
        (refresh, _) => {
            credentials.refresh = refresh
                .or_else(|| previous.map(|p| p.refresh.clone()))
                .unwrap_or_else(|| access.clone());
        }
    }
    credentials.account_id = jwt::first_claim(&access, &["user_id", "sub"])
        .or_else(|| previous.and_then(|p| p.account_id.clone()));
    credentials.email = jwt::email(&access).or_else(|| previous.and_then(|p| p.email.clone()));
    credentials.base_url = previous.and_then(|p| p.base_url.clone());
    credentials.headers = identity;
    Ok(credentials)
}

/// Runs the device-code login: shows the user a code through `notify`, polls until
/// the user approves, and returns the login's credentials, with the headers that
/// name the client kept with them. Opening the page is left to the client.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the provider rejects a request, the user
/// denies access, or the code expires before the user approves.
pub async fn login(
    device: &'static Device,
    notify: impl Fn(LoginPrompt),
) -> Result<Credentials, AuthError> {
    let client = reqwest::Client::new();
    let identity = device.identity.map(|make| make()).unwrap_or_default();
    let mut start = vec![("client_id", device.client_id)];
    if !device.scope.is_empty() {
        start.push(("scope", device.scope));
    }
    let issued: DeviceCode =
        serde_json::from_value(post(&client, device, &identity, device.code_url, &start).await?)
            .map_err(|e| AuthError::Token(format!("unusable device code response: {e}")))?;
    let url = issued
        .verification_uri_complete
        .clone()
        .or_else(|| issued.verification_uri.clone())
        .ok_or_else(|| AuthError::Token("the device code response names no page".to_string()))?;
    notify(LoginPrompt::Code {
        url,
        code: issued.user_code.clone(),
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(issued.expires_in);
    let mut interval = issued.interval.max(1);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let answer = post(
            &client,
            device,
            &identity,
            device.token_url,
            &[
                ("client_id", device.client_id),
                ("device_code", &issued.device_code),
                ("grant_type", GRANT),
            ],
        )
        .await?;
        match classify(&answer) {
            Poll::Token(answer) => {
                let mut credentials = credentials_from(&answer, identity, None)?;
                credentials.base_url = match device.api_url {
                    Some(url) => Some(url.to_string()),
                    None => discover_endpoint(&client, device, &credentials.access).await,
                };
                return Ok(credentials);
            }
            Poll::Pending => {}
            Poll::SlowDown => interval += SLOW_DOWN_STEP,
            Poll::Failed(reason) => return Err(AuthError::Token(reason)),
        }
    }
    Err(AuthError::Token("the device code expired".to_string()))
}

/// Renews `stored`, a device login whose access token expired, with its refresh token, keeping its headers and API address.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the provider refuses the refresh token, as after the login was revoked; signing in again is needed then.
pub async fn refresh(device: &Device, stored: &Credentials) -> Result<Credentials, AuthError> {
    let client = reqwest::Client::new();
    let answer = post(
        &client,
        device,
        &stored.headers,
        device.token_url,
        &[
            ("client_id", device.client_id),
            ("grant_type", "refresh_token"),
            ("refresh_token", &stored.refresh),
        ],
    )
    .await?;
    if let Some(error) = answer.get("error").and_then(Value::as_str) {
        return Err(AuthError::Token(format!(
            "the login could not be renewed ({error}); sign in again"
        )));
    }
    credentials_from(&answer, stored.headers.clone(), Some(stored))
}

#[cfg(test)]
/// Tests for the device-code login.
mod tests {
    use super::*;
    use serde_json::json;

    /// An access token ends the poll with the whole response.
    #[test]
    fn a_token_is_returned() {
        let answer = json!({"access_token": "t", "token_type": "bearer"});
        assert_eq!(classify(&answer), Poll::Token(answer.clone()));
    }

    /// Pending and slow-down answers keep the poll going.
    #[test]
    fn waiting_answers_continue() {
        assert_eq!(
            classify(&json!({"error": "authorization_pending"})),
            Poll::Pending
        );
        assert_eq!(classify(&json!({"error": "slow_down"})), Poll::SlowDown);
    }

    /// Any other error ends the flow and carries its description.
    #[test]
    fn other_errors_fail_with_the_description() {
        assert_eq!(
            classify(&json!({"error": "access_denied", "error_description": "no"})),
            Poll::Failed("access_denied: no".into())
        );
        assert!(matches!(classify(&json!({})), Poll::Failed(_)));
    }

    /// A token that never expires, as GitHub's, is kept as its own refresh token and never expires.
    #[test]
    fn a_lasting_token_never_expires() {
        let credentials =
            credentials_from(&json!({"access_token": "gho_x"}), Vec::new(), None).unwrap();
        assert_eq!(credentials.refresh, "gho_x");
        assert_eq!(credentials.expires_at_ms, u64::MAX);
    }

    /// A token with a lifetime and a refresh token expires ahead of time, names its account from its claims and keeps the headers it was made with.
    #[test]
    fn an_expiring_token_is_refreshed_ahead() {
        use base64::Engine;
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"user_id":"u-1","sub":"other"}"#);
        let answer = json!({
            "access_token": format!("h.{payload}.s"),
            "refresh_token": "r",
            "expires_in": 3600,
        });
        let headers = vec![("X-Client".to_owned(), "crab".to_owned())];
        let before = now_ms();
        let credentials = credentials_from(&answer, headers.clone(), None).unwrap();
        assert_eq!(credentials.refresh, "r");
        assert_eq!(credentials.account_id.as_deref(), Some("u-1"));
        assert_eq!(credentials.headers, headers);
        let refreshed_in = credentials.expires_at_ms - before;
        assert!(
            (3_299_000..=3_301_000).contains(&refreshed_in),
            "{refreshed_in}"
        );
        assert_eq!(refresh_at(0, 60), 30_000);
    }

    /// A refresh keeps the address and account of the login it renews when the answer leaves them out.
    #[test]
    fn a_refresh_keeps_what_the_answer_leaves_out() {
        let mut previous = Credentials::static_key("old".into());
        previous.base_url = Some("https://api.example/v1".into());
        previous.account_id = Some("acc".into());
        let answer = json!({"access_token": "new", "refresh_token": "r2", "expires_in": 900});
        let renewed = credentials_from(&answer, Vec::new(), Some(&previous)).unwrap();
        assert_eq!(renewed.base_url.as_deref(), Some("https://api.example/v1"));
        assert_eq!(renewed.account_id.as_deref(), Some("acc"));
        assert_eq!(renewed.refresh, "r2");
    }
}
