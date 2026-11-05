//! The device-code login: the user enters a short code on a web page while this polls for the token.

use std::time::Duration;

use serde::Deserialize;
use serde_json::Value;

use crate::method::Device;
use crate::{AuthError, Credentials, LoginPrompt};

/// Grant type the token endpoint is polled with.
const GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Seconds added to the polling interval each time the endpoint says to slow down.
const SLOW_DOWN_STEP: u64 = 5;

/// What the endpoint returns when a device code has been issued.
#[derive(Deserialize)]
struct DeviceCode {
    /// Secret the poll is made with.
    device_code: String,
    /// Code the user enters.
    user_code: String,
    /// Page the user enters it on.
    verification_uri: String,
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
#[derive(Debug, PartialEq, Eq)]
enum Poll {
    /// The user approved; this is the access token.
    Token(String),
    /// The user has not approved yet.
    Pending,
    /// Poll less often.
    SlowDown,
    /// The flow failed for this reason.
    Failed(String),
}

/// Reads the endpoint's answer to a poll.
fn classify(body: &Value) -> Poll {
    if let Some(token) = body.get("access_token").and_then(Value::as_str) {
        return Poll::Token(token.to_string());
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

/// Posts `form` to `url` with the flow's headers and returns the JSON answer.
async fn post(
    client: &reqwest::Client,
    device: &Device,
    url: &str,
    form: &[(&str, &str)],
) -> Result<Value, AuthError> {
    let mut request = client.post(url).header("Accept", "application/json");
    for (name, value) in device.headers {
        request = request.header(*name, *value);
    }
    let response = request
        .form(form)
        .send()
        .await
        .map_err(|e| AuthError::Token(e.to_string()))?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        return Err(AuthError::Token(format!("{status}: {body}")));
    }
    response
        .json()
        .await
        .map_err(|e| AuthError::Token(e.to_string()))
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

/// Runs the device-code login: shows the user a code through `notify`, polls until
/// the user approves, and returns credentials that do not expire. Opening the page
/// is left to the client.
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
    let issued: DeviceCode = serde_json::from_value(
        post(
            &client,
            device,
            device.code_url,
            &[("client_id", device.client_id), ("scope", device.scope)],
        )
        .await?,
    )
    .map_err(|e| AuthError::Token(format!("unusable device code response: {e}")))?;
    notify(LoginPrompt::Code {
        url: issued.verification_uri.clone(),
        code: issued.user_code.clone(),
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(issued.expires_in);
    let mut interval = issued.interval.max(1);
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(interval)).await;
        let answer = post(
            &client,
            device,
            device.token_url,
            &[
                ("client_id", device.client_id),
                ("device_code", &issued.device_code),
                ("grant_type", GRANT),
            ],
        )
        .await?;
        match classify(&answer) {
            Poll::Token(token) => {
                let mut credentials = Credentials::static_key(token.clone());
                credentials.refresh = token.clone();
                credentials.base_url = discover_endpoint(&client, device, &token).await;
                return Ok(credentials);
            }
            Poll::Pending => {}
            Poll::SlowDown => interval += SLOW_DOWN_STEP,
            Poll::Failed(reason) => return Err(AuthError::Token(reason)),
        }
    }
    Err(AuthError::Token("the device code expired".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// An access token ends the poll with that token.
    #[test]
    fn a_token_is_returned() {
        assert_eq!(
            classify(&json!({"access_token": "t", "token_type": "bearer"})),
            Poll::Token("t".into())
        );
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
}
