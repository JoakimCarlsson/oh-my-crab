//! The authorization-code-with-PKCE login and refresh flows, built on `oauth2`.

use std::time::Duration;

use oauth2::basic::{BasicClient, BasicTokenResponse};
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, EndpointNotSet, EndpointSet, ErrorResponse,
    PkceCodeChallenge, RedirectUrl, RefreshToken, RequestTokenError, Scope, TokenResponse,
    TokenUrl,
};

use crate::AuthError;
use crate::callback;
use crate::credentials::{Credentials, now_ms};
use crate::http::ProviderHttp;
use crate::jwt;
use crate::provider::Spec;

/// Random bytes behind the authorization `state`, which encodes to 43 characters.
const STATE_BYTES: u32 = 32;

/// Random bytes behind the PKCE verifier, which encodes to 128 characters.
const PKCE_VERIFIER_BYTES: u32 = 96;

/// Margin subtracted from token lifetimes so refresh happens early.
const EXPIRY_SKEW: Duration = Duration::from_secs(300);

/// Lifetime assumed when the token response omits `expires_in`.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(3600);

/// An oauth2 client with authorization and token endpoints configured.
type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Builds the oauth2 client for `spec`.
fn client(spec: &Spec) -> Result<ConfiguredClient, AuthError> {
    let bad = |e: oauth2::url::ParseError| AuthError::Token(e.to_string());
    Ok(BasicClient::new(ClientId::new(spec.client_id.to_string()))
        .set_auth_uri(AuthUrl::new(spec.authorize_url.to_string()).map_err(bad)?)
        .set_token_uri(TokenUrl::new(spec.token_url.to_string()).map_err(bad)?)
        .set_redirect_uri(RedirectUrl::new(spec.redirect_uri.to_string()).map_err(bad)?))
}

/// Builds a readable [`AuthError::Token`] from a failed token request.
///
/// The oauth2 crate's own messages for unreadable responses and transport
/// failures hide the cause, so the raw body or the error chain is added.
fn describe<RE, T>(error: RequestTokenError<RE, T>) -> AuthError
where
    RE: std::error::Error + 'static,
    T: ErrorResponse + 'static,
{
    let detail = match error {
        RequestTokenError::ServerResponse(response) => response.to_string(),
        RequestTokenError::Parse(cause, body) => format!(
            "unreadable token response ({cause}): {}",
            String::from_utf8_lossy(&body)
        ),
        RequestTokenError::Request(cause) => error_chain(&cause),
        RequestTokenError::Other(message) => message,
    };
    AuthError::Token(detail)
}

/// Joins `error` and its sources into one line separated by `: `.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

/// Builds the HTTP adapter for `spec` with the given extra headers.
fn http_for(
    spec: &Spec,
    headers: &'static [(&'static str, &'static str)],
) -> Result<ProviderHttp, AuthError> {
    ProviderHttp::new(spec.json_body, headers).map_err(|e| AuthError::Token(e.to_string()))
}

/// Converts a token response into credentials, keeping `previous` values the
/// response omits.
fn credentials_from(
    spec: &Spec,
    token: &BasicTokenResponse,
    previous: Option<&Credentials>,
) -> Result<Credentials, AuthError> {
    let refresh = token
        .refresh_token()
        .map(|t| t.secret().clone())
        .or_else(|| previous.map(|p| p.refresh.clone()))
        .ok_or_else(|| AuthError::Token("response carried no refresh token".into()))?;
    let lifetime = token.expires_in().unwrap_or(DEFAULT_LIFETIME);
    let usable = lifetime.saturating_sub(EXPIRY_SKEW);
    let access = token.access_token().secret().clone();
    let from_jwt = spec
        .account_from_jwt
        .then(|| jwt::chatgpt_account_id(&access))
        .flatten();
    Ok(Credentials {
        account_id: from_jwt.or_else(|| previous.and_then(|p| p.account_id.clone())),
        access,
        refresh,
        expires_at_ms: now_ms() + usable.as_millis() as u64,
    })
}

/// Runs the browser login for `spec` and returns the new credentials.
///
/// `notify` receives the authorization URL; the browser is also opened.
///
/// # Errors
///
/// Returns [`AuthError`] when the callback fails, `state` does not match, or the
/// token exchange is rejected.
pub async fn login(spec: &'static Spec, notify: impl Fn(String)) -> Result<Credentials, AuthError> {
    let client = client(spec)?;
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256_len(PKCE_VERIFIER_BYTES);
    let mut request = client
        .authorize_url(|| CsrfToken::new_random_len(STATE_BYTES))
        .set_pkce_challenge(challenge);
    for scope in spec.scopes {
        request = request.add_scope(Scope::new((*scope).to_string()));
    }
    for (name, value) in spec.authorize_params {
        request = request.add_extra_param(*name, *value);
    }
    let (url, state) = request.url();
    let listener =
        tokio::task::spawn_blocking(move || callback::wait_for_code(spec.port, spec.path));
    notify(url.to_string());
    let _ = open::that(url.as_str());
    let params = listener
        .await
        .map_err(|e| AuthError::Callback(e.to_string()))??;
    if params.state.as_deref() != Some(state.secret().as_str()) {
        return Err(AuthError::Callback("state mismatch".into()));
    }
    let mut exchange = client
        .exchange_code(AuthorizationCode::new(params.code))
        .set_pkce_verifier(verifier);
    if spec.state_in_exchange {
        exchange = exchange.add_extra_param("state", state.secret().clone());
    }
    let token = exchange
        .request_async(&http_for(spec, &[])?)
        .await
        .map_err(describe)?;
    credentials_from(spec, &token, None)
}

/// Exchanges the refresh token in `stored` for fresh credentials.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the endpoint rejects the refresh token.
pub async fn refresh(spec: &'static Spec, stored: &Credentials) -> Result<Credentials, AuthError> {
    let token = client(spec)?
        .exchange_refresh_token(&RefreshToken::new(stored.refresh.clone()))
        .request_async(&http_for(spec, spec.refresh_headers)?)
        .await
        .map_err(describe)?;
    credentials_from(spec, &token, Some(stored))
}
