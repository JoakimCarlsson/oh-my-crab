//! Signing in to a tool server with OAuth, as the MCP authorization spec describes.
//!
//! A server that wants a token answers `401` with a `WWW-Authenticate` header. Crab reads the server's protected-resource metadata (named by the header, or at `/.well-known/oauth-protected-resource`), then the metadata of the authorization server it names (or of the server's own origin), registers itself as a client when the authorization server lets it and no client id was set up, and opens an authorization-code sign-in with PKCE whose page sends the browser back to a port on `127.0.0.1`. The tokens are kept, and refreshed when they expire.

use std::time::Duration;

use oauth2::{CsrfToken, PkceCodeChallenge};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use url::Url;

use crate::config::OAuthSetup;
use crate::store::{Grant, now_ms};

/// The path on the loopback port the sign-in page sends the browser back to.
const CALLBACK_PATH: &str = "/callback";

/// Page shown in the browser once the sign-in came back.
const DONE_PAGE: &str =
    "<html><body><h3>Signed in.</h3>You can close this tab and return to crab.</body></html>";

/// What a server's `401` asked for.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Challenge {
    /// Where the server's protected-resource metadata is, when it said.
    pub(crate) metadata: Option<String>,
    /// The scopes it wants, when it said.
    pub(crate) scope: Option<String>,
}

/// The challenge in the `WWW-Authenticate` header `header`; an empty one when the header is not a bearer challenge.
pub(crate) fn challenge(header: &str) -> Challenge {
    let scheme = header.split_whitespace().next().unwrap_or_default();
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Challenge::default();
    }
    Challenge {
        metadata: parameter(header, "resource_metadata"),
        scope: parameter(header, "scope"),
    }
}

/// The value of the parameter `name` of the challenge `header`, quoted or not; nothing when it is missing or empty.
fn parameter(header: &str, name: &str) -> Option<String> {
    let lower = header.to_ascii_lowercase();
    let wanted = format!("{name}=");
    let mut from = 0;
    while let Some(found) = lower[from..].find(&wanted) {
        let at = from + found;
        let before = lower[..at].chars().last();
        from = at + wanted.len();
        if before.is_some_and(|c| !(c.is_whitespace() || c == ',')) {
            continue;
        }
        let rest = &header[from..];
        let value = match rest.strip_prefix('"') {
            Some(quoted) => quoted.split('"').next().unwrap_or_default(),
            None => rest
                .split(|c: char| c == ',' || c.is_whitespace())
                .next()
                .unwrap_or_default(),
        };
        return (!value.is_empty()).then(|| value.to_owned());
    }
    None
}

/// The endpoints of an authorization server.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Endpoints {
    /// Where the browser signs in.
    authorization: String,
    /// Where codes and refresh tokens become tokens.
    token: String,
    /// Where clients register themselves, when they may.
    registration: Option<String>,
    /// The scopes the protected resource suggests, when it lists them.
    scopes: Option<String>,
}

/// A sign-in started: the page the reader opens, and what finishes it when the browser comes back.
pub(crate) struct Started {
    /// The page to open in a browser.
    pub(crate) url: String,
    /// What is waited on.
    pub(crate) waiting: Waiting,
}

/// A sign-in waiting for the browser to come back to the loopback port.
pub(crate) struct Waiting {
    /// The port the browser comes back to.
    listener: TcpListener,
    /// The value the browser must bring back, against forgery.
    state: String,
    /// The PKCE verifier the code is exchanged with.
    verifier: String,
    /// The address the browser was told to come back to.
    redirect: String,
    /// The client id signed in as.
    client_id: String,
    /// The client secret, for a confidential client.
    client_secret: Option<String>,
    /// Where the code becomes tokens.
    token_endpoint: String,
    /// The resource the tokens are for.
    resource: String,
}

/// The resource a server at `url` is, as tokens name it: its address without a fragment.
pub(crate) fn resource_of(url: &str) -> String {
    match Url::parse(url) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_owned(),
    }
}

/// Starts signing in to the server at `server`, which answered with `challenge`: finds its authorization server, registers crab as a client unless `setup` names one, and listens on a loopback port for the browser to come back.
///
/// # Errors
///
/// Fails with the reason when the metadata cannot be read, the client cannot be registered, or no port can be listened on.
pub(crate) async fn start(
    http: &reqwest::Client,
    server: &str,
    challenge: &Challenge,
    setup: Option<&OAuthSetup>,
) -> Result<Started, String> {
    let endpoints = discover(http, server, challenge).await?;
    let port = setup.and_then(|setup| setup.callback_port).unwrap_or(0);
    let listener = TcpListener::bind(("127.0.0.1", port))
        .await
        .map_err(|error| format!("cannot listen for the sign-in on port {port}: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    let redirect = format!("http://127.0.0.1:{port}{CALLBACK_PATH}");
    let (client_id, client_secret) = match setup.and_then(|setup| setup.client_id.clone()) {
        Some(id) => (id, setup.and_then(|setup| setup.client_secret.clone())),
        None => register(http, &endpoints, &redirect).await?,
    };
    let (challenge_code, verifier) = PkceCodeChallenge::new_random_sha256();
    let state = CsrfToken::new_random().secret().clone();
    let resource = resource_of(server);
    let scope = setup
        .and_then(|setup| setup.scopes.clone())
        .or_else(|| challenge.scope.clone())
        .or_else(|| endpoints.scopes.clone());
    let mut url = Url::parse(&endpoints.authorization)
        .map_err(|error| format!("bad authorization endpoint: {error}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect)
            .append_pair("code_challenge", challenge_code.as_str())
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &resource);
        if let Some(scope) = scope.filter(|scope| !scope.is_empty()) {
            query.append_pair("scope", &scope);
        }
    }
    Ok(Started {
        url: url.to_string(),
        waiting: Waiting {
            listener,
            state,
            verifier: verifier.secret().clone(),
            redirect,
            client_id,
            client_secret,
            token_endpoint: endpoints.token,
            resource,
        },
    })
}

impl Waiting {
    /// Waits up to `patience` for the browser to come back with a code, and exchanges it for tokens.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the wait runs out, the sign-in page reports an error, the state does not match, or the token request fails.
    pub(crate) async fn finish(
        self,
        http: &reqwest::Client,
        patience: Duration,
    ) -> Result<Grant, String> {
        let code = tokio::time::timeout(patience, self.code())
            .await
            .map_err(|_| "the sign-in was not finished in time".to_owned())??;
        let mut form = vec![
            ("grant_type", "authorization_code".to_owned()),
            ("code", code),
            ("redirect_uri", self.redirect.clone()),
            ("client_id", self.client_id.clone()),
            ("code_verifier", self.verifier.clone()),
            ("resource", self.resource.clone()),
        ];
        if let Some(secret) = &self.client_secret {
            form.push(("client_secret", secret.clone()));
        }
        let answer = token_request(http, &self.token_endpoint, &form).await?;
        granted(
            &answer,
            Grant {
                client_id: self.client_id,
                client_secret: self.client_secret,
                token_endpoint: self.token_endpoint,
                resource: self.resource,
                ..Grant::default()
            },
        )
    }

    /// Answers requests to the loopback port until one comes to the callback path, and returns its code.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the port fails, or the request that came back carries an error, the wrong state or no code.
    async fn code(&self) -> Result<String, String> {
        loop {
            let (mut stream, _) = self
                .listener
                .accept()
                .await
                .map_err(|error| error.to_string())?;
            let Some(target) = request_target(&mut stream).await else {
                continue;
            };
            let Ok(parsed) = Url::parse(&format!("http://127.0.0.1{target}")) else {
                continue;
            };
            if parsed.path() != CALLBACK_PATH {
                respond(&mut stream, "404 Not Found", "not found").await;
                continue;
            }
            let pair = |key: &str| {
                parsed
                    .query_pairs()
                    .find(|(name, _)| name == key)
                    .map(|(_, value)| value.into_owned())
            };
            respond(&mut stream, "200 OK", DONE_PAGE).await;
            if let Some(error) = pair("error") {
                let detail = pair("error_description").unwrap_or_default();
                return Err(format!("the sign-in failed: {error} {detail}")
                    .trim_end()
                    .to_owned());
            }
            if pair("state").as_deref() != Some(self.state.as_str()) {
                return Err("the sign-in came back with the wrong state".to_owned());
            }
            return pair("code").ok_or_else(|| "the sign-in came back without a code".to_owned());
        }
    }
}

/// The target of the HTTP request read from `stream`, such as `/callback?code=x`; nothing when it is not a request line.
async fn request_target(stream: &mut tokio::net::TcpStream) -> Option<String> {
    let mut head = Vec::new();
    let mut buffer = [0u8; 1024];
    while !head.windows(4).any(|window| window == b"\r\n\r\n") && head.len() < 16 * 1024 {
        let read = tokio::time::timeout(Duration::from_secs(10), stream.read(&mut buffer))
            .await
            .ok()?
            .ok()?;
        if read == 0 {
            break;
        }
        head.extend_from_slice(&buffer[..read]);
    }
    let text = String::from_utf8_lossy(&head);
    let mut words = text.lines().next()?.split_whitespace();
    let _method = words.next()?;
    words.next().map(str::to_owned)
}

/// Writes an HTML answer with `status` and `body` to `stream` and closes it.
async fn respond(stream: &mut tokio::net::TcpStream, status: &str, body: &str) {
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(answer.as_bytes()).await;
    let _ = stream.shutdown().await;
}

/// Gets a new access token for `grant` with its refresh token.
///
/// # Errors
///
/// Fails with the reason when there is no refresh token or the token endpoint refuses it.
pub(crate) async fn refresh(http: &reqwest::Client, grant: &Grant) -> Result<Grant, String> {
    let Some(token) = &grant.refresh else {
        return Err("no refresh token".to_owned());
    };
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", token.clone()),
        ("client_id", grant.client_id.clone()),
        ("resource", grant.resource.clone()),
    ];
    if let Some(secret) = &grant.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let answer = token_request(http, &grant.token_endpoint, &form).await?;
    granted(&answer, grant.clone())
}

/// Posts `form` to the token endpoint `endpoint` and returns its JSON answer.
///
/// # Errors
///
/// Fails with the reason when the endpoint cannot be reached or answers with a failure.
async fn token_request(
    http: &reqwest::Client,
    endpoint: &str,
    form: &[(&str, String)],
) -> Result<Value, String> {
    let response = http
        .post(endpoint)
        .header(reqwest::header::ACCEPT, "application/json")
        .form(form)
        .send()
        .await
        .map_err(|error| format!("the token request failed: {error}"))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let error = body["error"].as_str().unwrap_or("refused");
        return Err(format!("the token endpoint answered {status}: {error}"));
    }
    Ok(body)
}

/// `before` with the tokens of the token endpoint's `answer`, keeping the refresh token when the answer brings no new one.
///
/// # Errors
///
/// Fails when the answer has no access token.
fn granted(answer: &Value, before: Grant) -> Result<Grant, String> {
    let access = answer["access_token"]
        .as_str()
        .ok_or("the token endpoint gave no access token")?;
    Ok(Grant {
        access: access.to_owned(),
        refresh: answer["refresh_token"]
            .as_str()
            .map(str::to_owned)
            .or(before.refresh.clone()),
        expires_at_ms: answer["expires_in"]
            .as_u64()
            .map(|seconds| now_ms().saturating_add(seconds.saturating_mul(1000))),
        ..before
    })
}

/// Registers crab as a public client of the authorization server of `endpoints`, coming back to `redirect`, and returns its client id and secret.
///
/// # Errors
///
/// Fails with the reason when the server offers no registration or refuses it.
async fn register(
    http: &reqwest::Client,
    endpoints: &Endpoints,
    redirect: &str,
) -> Result<(String, Option<String>), String> {
    let Some(registration) = &endpoints.registration else {
        return Err(
            "the authorization server does not register clients; set `oauth.clientId` for the server"
                .to_owned(),
        );
    };
    let response = http
        .post(registration)
        .json(&json!({
            "client_name": "crab",
            "redirect_uris": [redirect],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        }))
        .send()
        .await
        .map_err(|error| format!("the client registration failed: {error}"))?;
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        return Err(format!("the client registration answered {status}"));
    }
    let id = body["client_id"]
        .as_str()
        .ok_or("the client registration gave no client id")?;
    Ok((
        id.to_owned(),
        body["client_secret"].as_str().map(str::to_owned),
    ))
}

/// The endpoints of the authorization server of the server at `server`, found from its `challenge`, its protected-resource metadata and the authorization server's metadata; the authorization server's own default paths when it publishes no metadata.
///
/// # Errors
///
/// Fails with the reason when the server's address is not a URL, or metadata that is found names another issuer or no endpoints.
async fn discover(
    http: &reqwest::Client,
    server: &str,
    challenge: &Challenge,
) -> Result<Endpoints, String> {
    let url = Url::parse(server).map_err(|error| format!("bad server address: {error}"))?;
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut candidates: Vec<String> = Vec::new();
    match &challenge.metadata {
        Some(named) => candidates.push(named.clone()),
        None => {
            if !path.is_empty() {
                candidates.push(format!(
                    "{origin}/.well-known/oauth-protected-resource{path}"
                ));
            }
            candidates.push(format!("{origin}/.well-known/oauth-protected-resource"));
        }
    }
    let mut resource = None;
    for candidate in candidates {
        if let Some(found) = metadata(http, &candidate).await {
            resource = Some(found);
            break;
        }
    }
    let scopes = resource.as_ref().and_then(|found| {
        found["scopes_supported"].as_array().map(|scopes| {
            scopes
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
    });
    let issuer = resource
        .as_ref()
        .and_then(|found| found["authorization_servers"][0].as_str())
        .map_or_else(|| origin.clone(), str::to_owned);
    let issuer_url =
        Url::parse(&issuer).map_err(|error| format!("bad authorization server: {error}"))?;
    let issuer_origin = issuer_url.origin().ascii_serialization();
    let issuer_path = issuer_url.path().trim_end_matches('/');
    let mut places = vec![
        format!("{issuer_origin}/.well-known/oauth-authorization-server{issuer_path}"),
        format!("{issuer_origin}/.well-known/openid-configuration{issuer_path}"),
    ];
    if !issuer_path.is_empty() {
        places.push(format!(
            "{issuer_origin}{issuer_path}/.well-known/openid-configuration"
        ));
    }
    for place in places {
        let Some(found) = metadata(http, &place).await else {
            continue;
        };
        let named = found["issuer"].as_str().unwrap_or_default();
        if named.trim_end_matches('/') != issuer.trim_end_matches('/') {
            return Err(format!(
                "the authorization server's metadata names the issuer {named}, not {issuer}"
            ));
        }
        let endpoint = |key: &str| found[key].as_str().map(str::to_owned);
        if let Some(methods) = found["code_challenge_methods_supported"].as_array()
            && !methods.iter().any(|method| method == "S256")
        {
            return Err("the authorization server does not support PKCE with S256".to_owned());
        }
        return Ok(Endpoints {
            authorization: endpoint("authorization_endpoint")
                .ok_or("the authorization server's metadata has no authorization endpoint")?,
            token: endpoint("token_endpoint")
                .ok_or("the authorization server's metadata has no token endpoint")?,
            registration: endpoint("registration_endpoint"),
            scopes,
        });
    }
    Ok(Endpoints {
        authorization: format!("{issuer_origin}/authorize"),
        token: format!("{issuer_origin}/token"),
        registration: Some(format!("{issuer_origin}/register")),
        scopes,
    })
}

/// The JSON document at `url`, when it answers with one.
async fn metadata(http: &reqwest::Client, url: &str) -> Option<Value> {
    let response = http
        .get(url)
        .header(reqwest::header::ACCEPT, "application/json")
        .header("MCP-Protocol-Version", crate::client::VERSION)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    response.json().await.ok()
}

#[cfg(test)]
/// Tests for reading challenges.
mod tests {
    use super::*;

    /// The metadata address and the scope are read, quoted or not, and other schemes give nothing.
    #[test]
    fn challenges() {
        let read = challenge(
            r#"Bearer error="invalid_token", resource_metadata="https://a.example/.well-known/oauth-protected-resource", scope=files:read"#,
        );
        assert_eq!(
            read.metadata.as_deref(),
            Some("https://a.example/.well-known/oauth-protected-resource")
        );
        assert_eq!(read.scope.as_deref(), Some("files:read"));
        assert_eq!(challenge("Basic realm=x"), Challenge::default());
        assert_eq!(challenge("Bearer"), Challenge::default());
    }

    /// A resource is its address without a fragment.
    #[test]
    fn resources() {
        assert_eq!(
            resource_of("https://a.example/mcp#x"),
            "https://a.example/mcp"
        );
    }
}
