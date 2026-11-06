//! Asking the GitHub REST API, signed in as the user when that is possible.
//!
//! The credentials come from the `gh` command line tool when it is installed
//! and signed in, which reuses the user's own sign-in, else from `GH_TOKEN` or
//! `GITHUB_TOKEN`, else there are none and only public repositories can be
//! read, at a lower rate limit. Nothing is written to disk.

use std::sync::Mutex;
use std::time::Duration;

use serde_json::Value;

use crate::ToolError;

/// The address of the GitHub REST API.
pub(super) const API: &str = "https://api.github.com";

/// How long one API request may take.
const TIMEOUT: Duration = Duration::from_secs(30);

/// How long asking `gh` for its token may take.
const GH_TIMEOUT: Duration = Duration::from_secs(5);

/// Most bytes of a response body read; a larger one fails.
const MAX_BODY: usize = 10 << 20;

/// The API version every request asks for.
const API_VERSION: &str = "2022-11-28";

/// The media type of JSON answers.
pub(super) const JSON: &str = "application/vnd.github+json";

/// The media type that asks for a file's bytes as they are.
pub(super) const RAW: &str = "application/vnd.github.raw+json";

/// The media type that asks for a pull request's unified diff.
pub(super) const DIFF: &str = "application/vnd.github.diff";

/// The variables a token is read from when `gh` gives none, in order.
const TOKEN_VARIABLES: [&str; 2] = ["GH_TOKEN", "GITHUB_TOKEN"];

/// The credentials found last, kept for the rest of the process so that `gh`
/// is not asked on every read; forgotten when GitHub refuses them.
static FOUND: Mutex<Option<Credential>> = Mutex::new(None);

/// What requests are signed with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Credential {
    /// The token the `gh` tool is signed in with.
    Gh(String),
    /// A token from the named environment variable.
    Variable(&'static str, String),
    /// Nothing: requests are anonymous.
    Anonymous,
}

impl Credential {
    /// Finds the credentials to use: `gh`'s token, else a token variable, else
    /// none. Found credentials are remembered for the process.
    pub(super) async fn discover() -> Self {
        if let Some(found) = FOUND.lock().unwrap().clone() {
            return found;
        }
        let found = match gh_token().await {
            Some(token) => Self::Gh(token),
            None => TOKEN_VARIABLES
                .iter()
                .find_map(|name| {
                    std::env::var(name)
                        .ok()
                        .filter(|value| !value.trim().is_empty())
                        .map(|value| Self::Variable(name, value.trim().to_owned()))
                })
                .unwrap_or(Self::Anonymous),
        };
        if found != Self::Anonymous {
            *FOUND.lock().unwrap() = Some(found.clone());
        }
        found
    }

    /// The token, when there is one.
    fn token(&self) -> Option<&str> {
        match self {
            Self::Gh(token) | Self::Variable(_, token) => Some(token),
            Self::Anonymous => None,
        }
    }

    /// Says where the credentials came from, for the header of a result.
    pub(super) fn describe(&self) -> String {
        match self {
            Self::Gh(_) => "signed in through gh".to_owned(),
            Self::Variable(name, _) => format!("signed in with {name}"),
            Self::Anonymous => "not signed in".to_owned(),
        }
    }
}

/// The token `gh` is signed in to github.com with, when `gh` is installed,
/// signed in and answers in time.
async fn gh_token() -> Option<String> {
    let mut command = tokio::process::Command::new("gh");
    command
        .args(["auth", "token", "--hostname", "github.com"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(GH_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()?;
    let token = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (output.status.success() && !token.is_empty()).then_some(token)
}

/// A client of the GitHub REST API at one address, with the credentials it
/// signs requests with.
pub(super) struct Client {
    /// The API's address, such as [`API`].
    base: reqwest::Url,
    /// What requests are signed with.
    pub(super) credential: Credential,
    /// The HTTP client.
    http: reqwest::Client,
}

impl Client {
    /// A client of the API at `base` signing requests with `credential`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when `base` is not a URL or the HTTP
    /// client cannot be built.
    pub(super) fn new(base: &str, credential: Credential) -> Result<Self, ToolError> {
        let base = reqwest::Url::parse(base)
            .map_err(|error| ToolError::Failed(format!("{base}: {error}")))?;
        let http = reqwest::Client::builder()
            .user_agent(concat!("crab/", env!("CARGO_PKG_VERSION")))
            .timeout(TIMEOUT)
            .build()
            .map_err(|error| ToolError::Failed(error.to_string()))?;
        Ok(Self {
            base,
            credential,
            http,
        })
    }

    /// The URL of the API path made of `segments`, each escaped, with `query`.
    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> reqwest::Url {
        let mut url = self.base.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty();
            path.extend(segments);
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        url
    }

    /// Gets the API path made of `segments` with `query`, asking for
    /// `accept`, and returns the body.
    ///
    /// When GitHub refuses the credentials, they are forgotten and the request
    /// is made once more without them, which still reaches public repositories.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the request fails, the answer is not
    /// a success, or the body is larger than [`MAX_BODY`], saying what to do
    /// about a missing repository or a rate limit.
    pub(super) async fn get(
        &mut self,
        segments: &[&str],
        query: &[(&str, &str)],
        accept: &str,
    ) -> Result<Vec<u8>, ToolError> {
        let url = self.url(segments, query);
        let first = self.send(&url, accept).await?;
        let response = if first.status() == reqwest::StatusCode::UNAUTHORIZED
            && self.credential.token().is_some()
        {
            *FOUND.lock().unwrap() = None;
            self.credential = Credential::Anonymous;
            self.send(&url, accept).await?
        } else {
            first
        };
        let status = response.status();
        let limited = response
            .headers()
            .get("x-ratelimit-remaining")
            .is_some_and(|value| value.as_bytes() == b"0");
        let body = read_body(response, &url).await?;
        if status.is_success() {
            return Ok(body);
        }
        Err(ToolError::Failed(explain(
            status.as_u16(),
            limited,
            &body,
            &url,
            &self.credential,
        )))
    }

    /// Gets the API path made of `segments` with `query` as JSON.
    ///
    /// # Errors
    ///
    /// Same as [`Client::get`], and fails when the body is not JSON.
    pub(super) async fn json(
        &mut self,
        segments: &[&str],
        query: &[(&str, &str)],
    ) -> Result<Value, ToolError> {
        let body = self.get(segments, query, JSON).await?;
        serde_json::from_slice(&body).map_err(|error| {
            ToolError::Failed(format!(
                "GitHub answered {} with something that is not JSON: {error}",
                segments.join("/")
            ))
        })
    }

    /// Sends one GET of `url` asking for `accept`, signed when there is a token.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the request cannot be sent.
    async fn send(&self, url: &reqwest::Url, accept: &str) -> Result<reqwest::Response, ToolError> {
        let mut request = self
            .http
            .get(url.clone())
            .header(reqwest::header::ACCEPT, accept)
            .header("X-GitHub-Api-Version", API_VERSION);
        if let Some(token) = self.credential.token() {
            request = request.bearer_auth(token);
        }
        request
            .send()
            .await
            .map_err(|error| ToolError::Failed(format!("asking GitHub for {url} failed: {error}")))
    }
}

/// Reads the body of `response` to `url`, up to [`MAX_BODY`] bytes.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when reading fails or the body is larger.
async fn read_body(
    mut response: reqwest::Response,
    url: &reqwest::Url,
) -> Result<Vec<u8>, ToolError> {
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| ToolError::Failed(format!("reading {url} failed: {error}")))?
    {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err(ToolError::Failed(format!(
                "GitHub's answer to {url} is larger than {} MB",
                MAX_BODY >> 20
            )));
        }
    }
    Ok(body)
}

/// The message for a GitHub answer of `status` with `body` to `url`, made with
/// `credential`; `limited` says the rate limit is used up.
fn explain(
    status: u16,
    limited: bool,
    body: &[u8],
    url: &reqwest::Url,
    credential: &Credential,
) -> String {
    let said = serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| value["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| String::from_utf8_lossy(&body[..body.len().min(200)]).into_owned());
    let sign_in = "sign in with `gh auth login`, or set GH_TOKEN or GITHUB_TOKEN";
    let hint = match status {
        403 | 429 if limited || said.to_ascii_lowercase().contains("rate limit") => {
            if *credential == Credential::Anonymous {
                format!("; the anonymous rate limit is used up: {sign_in}")
            } else {
                "; the rate limit is used up, try again later".to_owned()
            }
        }
        404 if *credential == Credential::Anonymous => {
            format!("; a private repository needs credentials: {sign_in}")
        }
        404 => "; check the owner, repository and number".to_owned(),
        406 if said.contains("diff") => {
            "; the diff is too large for the API, read the pull request's files instead".to_owned()
        }
        _ => String::new(),
    };
    format!("GitHub answered {status} for {}: {said}{hint}", url.path())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Path segments are escaped one by one, and the query is appended.
    #[test]
    fn builds_urls() {
        let client = Client::new("http://127.0.0.1:9/api/", Credential::Anonymous).unwrap();
        let url = client.url(
            &["repos", "o", "r", "contents", "dir", "a b#c?.rs"],
            &[("ref", "main")],
        );
        assert_eq!(
            url.as_str(),
            "http://127.0.0.1:9/api/repos/o/r/contents/dir/a%20b%23c%3F.rs?ref=main"
        );
    }

    /// Errors say what GitHub said and what to do about it.
    #[test]
    fn explains_failures() {
        let url = reqwest::Url::parse("https://api.github.com/repos/o/r/pulls/1").unwrap();
        let body = br#"{"message":"Not Found"}"#;
        let anonymous = explain(404, false, body, &url, &Credential::Anonymous);
        assert!(anonymous.contains("404"), "{anonymous}");
        assert!(anonymous.contains("Not Found"), "{anonymous}");
        assert!(anonymous.contains("gh auth login"), "{anonymous}");
        let signed = explain(404, false, body, &url, &Credential::Gh("t".into()));
        assert!(!signed.contains("gh auth login"), "{signed}");
        let limited = explain(403, true, b"{}", &url, &Credential::Anonymous);
        assert!(limited.contains("rate limit"), "{limited}");
    }
}
