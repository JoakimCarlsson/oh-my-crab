//! Google OAuth access tokens from an Application Default Credentials file: a service account signs a JWT that is exchanged for a token, and a user login trades its refresh token for one.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde::Deserialize;
use serde_json::json;

/// The scope a token is asked for: every Google Cloud API, Vertex AI among them.
const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

/// Where tokens are asked for when the file does not say.
const TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// How long a signed assertion is valid.
const ASSERTION_LIFETIME: u64 = 3600;

/// How long a token exchange may take.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(30);

/// An Application Default Credentials file, of the two kinds crab reads.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum CredentialFile {
    /// A service account's key.
    ServiceAccount {
        /// The account's address, the assertion's issuer.
        client_email: String,
        /// The account's RSA key, as PKCS #8 PEM.
        private_key: String,
        /// Where the assertion is exchanged for a token.
        #[serde(default)]
        token_uri: Option<String>,
        /// The project the account belongs to.
        #[serde(default)]
        project_id: Option<String>,
    },
    /// A user's login, as `gcloud auth application-default login` saves it.
    AuthorizedUser {
        /// The OAuth client the login was made with.
        client_id: String,
        /// That client's secret.
        client_secret: String,
        /// The login's refresh token.
        refresh_token: String,
        /// The project quota is billed to.
        #[serde(default)]
        quota_project_id: Option<String>,
        /// Where the refresh token is traded, when not Google's usual address.
        #[serde(default)]
        token_uri: Option<String>,
    },
}

impl CredentialFile {
    /// The project the file names, when it names one.
    pub(crate) fn project(&self) -> Option<&str> {
        match self {
            CredentialFile::ServiceAccount { project_id, .. } => project_id.as_deref(),
            CredentialFile::AuthorizedUser {
                quota_project_id, ..
            } => quota_project_id.as_deref(),
        }
    }
}

/// An access token and how long it stays valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Token {
    /// The bearer token.
    pub access: String,
    /// How long it is valid from when it was given.
    pub lifetime: Duration,
}

/// The answer of a token endpoint.
#[derive(Deserialize)]
struct Answer {
    /// The bearer token.
    access_token: String,
    /// Its lifetime in seconds.
    #[serde(default)]
    expires_in: Option<u64>,
}

/// The JWT a service account `email` signs with `key_pem` to ask `audience` for a token, issued at the Unix time `now`.
///
/// # Errors
///
/// Fails when the key is not an RSA key in PKCS #8 PEM.
pub(crate) fn assertion(
    email: &str,
    key_pem: &str,
    audience: &str,
    now: u64,
) -> Result<String, String> {
    let header = URL_SAFE_NO_PAD.encode(json!({ "alg": "RS256", "typ": "JWT" }).to_string());
    let claims = URL_SAFE_NO_PAD.encode(
        json!({
            "iss": email,
            "scope": SCOPE,
            "aud": audience,
            "iat": now,
            "exp": now + ASSERTION_LIFETIME,
        })
        .to_string(),
    );
    let signed = format!("{header}.{claims}");
    let key = RsaKeyPair::from_pkcs8(&pem_der(key_pem)?)
        .map_err(|error| format!("the service account key is not usable: {error}"))?;
    let mut signature = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signed.as_bytes(),
        &mut signature,
    )
    .map_err(|_| "the service account key could not sign".to_owned())?;
    Ok(format!("{signed}.{}", URL_SAFE_NO_PAD.encode(signature)))
}

/// The DER bytes inside the PEM text `pem`.
///
/// # Errors
///
/// Fails when the text between the armour lines is not base64.
fn pem_der(pem: &str) -> Result<Vec<u8>, String> {
    let body: String = pem
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("-----"))
        .collect();
    STANDARD
        .decode(body.as_bytes())
        .map_err(|error| format!("the service account key is not PEM: {error}"))
}

/// Where a token request goes, and the form it posts.
type TokenRequest = (String, Vec<(&'static str, String)>);

/// The form a token request for `file` posts, and where it posts it, at the Unix time `now`.
///
/// # Errors
///
/// Fails when a service account's key cannot sign.
pub(crate) fn token_request(file: &CredentialFile, now: u64) -> Result<TokenRequest, String> {
    match file {
        CredentialFile::ServiceAccount {
            client_email,
            private_key,
            token_uri,
            ..
        } => {
            let uri = token_uri.clone().unwrap_or_else(|| TOKEN_URI.to_owned());
            let jwt = assertion(client_email, private_key, &uri, now)?;
            Ok((
                uri,
                vec![
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:jwt-bearer".to_owned(),
                    ),
                    ("assertion", jwt),
                ],
            ))
        }
        CredentialFile::AuthorizedUser {
            client_id,
            client_secret,
            refresh_token,
            token_uri,
            ..
        } => Ok((
            token_uri.clone().unwrap_or_else(|| TOKEN_URI.to_owned()),
            vec![
                ("grant_type", "refresh_token".to_owned()),
                ("client_id", client_id.clone()),
                ("client_secret", client_secret.clone()),
                ("refresh_token", refresh_token.clone()),
            ],
        )),
    }
}

/// Asks for an access token with the credentials of `file`.
///
/// # Errors
///
/// Fails when the key cannot sign, the request fails, or the answer holds no token.
pub(crate) async fn exchange(file: &CredentialFile) -> Result<Token, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    let (uri, form) = token_request(file, now)?;
    let response = reqwest::Client::builder()
        .timeout(EXCHANGE_TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?
        .post(&uri)
        .form(&form)
        .send()
        .await
        .map_err(|error| format!("{uri}: {error}"))?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("{uri} answered {status}: {body}"));
    }
    let answer: Answer =
        serde_json::from_str(&body).map_err(|error| format!("{uri} gave no token: {error}"))?;
    Ok(Token {
        access: answer.access_token,
        lifetime: Duration::from_secs(answer.expires_in.unwrap_or(3600)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    /// A 2048-bit RSA key in PKCS #8 PEM, made for these tests only.
    const KEY: &str = include_str!("testdata/service-account-key.pem");

    /// A service account's assertion is an RS256 JWT, for the cloud-platform scope, that its key's public half verifies.
    #[test]
    fn signs_a_verifiable_assertion() {
        let jwt = assertion(
            "crab@example.iam.gserviceaccount.com",
            KEY,
            "https://oauth2.googleapis.com/token",
            1_000,
        )
        .unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
        assert_eq!(header["alg"], "RS256");
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["iss"], "crab@example.iam.gserviceaccount.com");
        assert_eq!(claims["scope"], SCOPE);
        assert_eq!(
            (claims["iat"].as_u64(), claims["exp"].as_u64()),
            (Some(1_000), Some(4_600))
        );
        let pair = RsaKeyPair::from_pkcs8(&pem_der(KEY).unwrap()).unwrap();
        let public = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pair.public().as_ref());
        let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
        public
            .verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &signature)
            .unwrap();
        assert!(
            assertion(
                "a",
                "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----",
                "b",
                0
            )
            .is_err()
        );
    }

    /// Both kinds of file read, name their project, and post the form their grant needs.
    #[test]
    fn reads_both_kinds_of_file() {
        let account: CredentialFile = serde_json::from_value(json!({
            "type": "service_account", "project_id": "p-1", "client_email": "a@b",
            "private_key": KEY, "token_uri": "http://127.0.0.1:1/token"
        }))
        .unwrap();
        assert_eq!(account.project(), Some("p-1"));
        let (uri, form) = token_request(&account, 5).unwrap();
        assert_eq!(uri, "http://127.0.0.1:1/token");
        assert_eq!(form[0].1, "urn:ietf:params:oauth:grant-type:jwt-bearer");
        let user: CredentialFile = serde_json::from_value(json!({
            "type": "authorized_user", "client_id": "c", "client_secret": "s",
            "refresh_token": "r", "quota_project_id": "q-1"
        }))
        .unwrap();
        assert_eq!(user.project(), Some("q-1"));
        let (uri, form) = token_request(&user, 5).unwrap();
        assert_eq!(uri, TOKEN_URI);
        assert!(form.contains(&("refresh_token", "r".to_owned())));
        assert!(
            serde_json::from_value::<CredentialFile>(json!({ "type": "external_account" }))
                .is_err()
        );
    }
}
