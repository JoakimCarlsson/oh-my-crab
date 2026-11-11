//! High-level login and credential retrieval used by the UI.

use crate::store::{self, AuthFile};
use crate::{Auth, AuthError, Credentials, Device, LoginPrompt, Spec, device, flow};

/// Returns the first of the environment variables `names` that holds a key.
pub fn env_key(names: &[&str]) -> Option<String> {
    env_key_named(names).map(|(_, key)| key)
}

/// Returns the first of the environment variables `names` that holds a key, with the name of the variable it was read from.
pub fn env_key_named<'a>(names: &[&'a str]) -> Option<(&'a str, String)> {
    names
        .iter()
        .filter_map(|name| Some((*name, std::env::var(name).ok()?)))
        .map(|(name, value)| (name, value.trim().to_string()))
        .find(|(_, value)| !value.is_empty())
}

/// Signs in to the provider `id` the way `auth` says and stores the result as active.
///
/// An OAuth provider runs the browser flow and `notify` is told the
/// authorization URL; a device-code provider, or one that also takes a key, is
/// told the page and the code.
/// An API-key provider reads its key from the environment, and a provider
/// without credentials just becomes active. A cloud provider becomes active
/// when it has a key or its `check` finds the cloud's own credentials.
///
/// # Errors
///
/// Returns [`AuthError`] when the flow fails, the key is not set, or
/// credentials cannot be saved.
pub async fn login(
    id: &str,
    auth: &'static Auth,
    notify: impl Fn(LoginPrompt),
) -> Result<(), AuthError> {
    let credentials = match auth {
        Auth::OAuth(spec) => flow::login(spec, |url| notify(LoginPrompt::Url(url))).await?,
        Auth::DeviceCode(spec) | Auth::DeviceCodeOrKey { device: spec, .. } => {
            device::login(spec, notify).await?
        }
        Auth::ApiKey { env } => Credentials::static_key(
            env_key(env).ok_or_else(|| AuthError::MissingKey(env.join(" or ")))?,
        ),
        Auth::None => Credentials::static_key(String::new()),
        Auth::Cloud { env, check, .. } => {
            let mut file: AuthFile = store::load()?;
            if env_key(env).is_none() && saved_key(&file, id).is_none() {
                check().map_err(AuthError::NoCredentials)?;
            }
            file.credentials
                .entry(id.to_string())
                .or_insert_with(|| Credentials::static_key(String::new()));
            file.active = Some(id.to_string());
            return store::save(&file);
        }
    };
    let mut file: AuthFile = store::load()?;
    file.credentials.insert(id.to_string(), credentials);
    file.active = Some(id.to_string());
    store::save(&file)
}

/// Returns credentials for the provider `id` that are valid right now.
///
/// Expired OAuth and device-login tokens are refreshed and the refreshed
/// credentials are saved. An API key from the environment is preferred over a
/// stored login or key.
///
/// # Errors
///
/// Returns [`AuthError::NotLoggedIn`] when nothing is stored for the provider,
/// [`AuthError::MissingKey`] when it needs a key that is not set, or another
/// [`AuthError`] when refreshing or saving fails.
pub async fn credentials_for(id: &str, auth: &'static Auth) -> Result<Credentials, AuthError> {
    let spec = match auth {
        Auth::OAuth(spec) => spec,
        Auth::ApiKey { env } => {
            if let Some(key) = env_key(env) {
                return Ok(Credentials::static_key(key));
            }
            return store::load()?
                .credentials
                .remove(id)
                .ok_or_else(|| AuthError::MissingKey(env.join(" or ")));
        }
        Auth::DeviceCode(device) => {
            return device_credentials(id, device, AuthError::NotLoggedIn).await;
        }
        Auth::DeviceCodeOrKey { device, env } => {
            if let Some(key) = env_key(env) {
                return Ok(Credentials::static_key(key));
            }
            return device_credentials(id, device, AuthError::MissingKey(env.join(" or "))).await;
        }
        Auth::None => return Ok(Credentials::static_key(String::new())),
        Auth::Cloud { env, .. } => {
            if let Some(key) = env_key(env) {
                return Ok(Credentials::static_key(key));
            }
            let file = store::load()?;
            return Ok(
                saved_key(&file, id).unwrap_or_else(|| Credentials::static_key(String::new()))
            );
        }
    };
    let stored = store::load()?
        .credentials
        .remove(id)
        .ok_or(AuthError::NotLoggedIn)?;
    if !stored.is_expired() {
        return Ok(stored);
    }
    renew(id, &Renewal::OAuth(spec), &stored).await
}

/// The credentials stored for the provider `id`, which signs in with `device` or a saved key, renewed and saved again when they are a login whose token expired; `missing` when nothing is stored.
///
/// # Errors
///
/// Returns `missing` when nothing is stored, or another [`AuthError`] when renewing or saving fails.
async fn device_credentials(
    id: &str,
    device: &'static Device,
    missing: AuthError,
) -> Result<Credentials, AuthError> {
    let stored = store::load()?.credentials.remove(id).ok_or(missing)?;
    if !stored.is_expired() {
        return Ok(stored);
    }
    renew(id, &Renewal::Device(device), &stored).await
}

/// How a login's access token is traded for a new one.
enum Renewal {
    /// With the refresh grant of the browser flow.
    OAuth(&'static Spec),
    /// With the refresh grant of the device-code flow.
    Device(&'static Device),
}

impl Renewal {
    /// The way logins of a provider signed in to as `auth` says are renewed; nothing for a provider that has no logins.
    fn of(auth: &'static Auth) -> Option<Self> {
        match auth {
            Auth::OAuth(spec) => Some(Self::OAuth(spec)),
            Auth::DeviceCode(device) | Auth::DeviceCodeOrKey { device, .. } => {
                Some(Self::Device(device))
            }
            Auth::ApiKey { .. } | Auth::Cloud { .. } | Auth::None => None,
        }
    }

    /// Trades the refresh token of `stored` for new credentials.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::Token`] when the provider refuses the refresh token.
    async fn run(&self, stored: &Credentials) -> Result<Credentials, AuthError> {
        match self {
            Self::OAuth(spec) => flow::refresh(spec, stored).await,
            Self::Device(device) => device::refresh(device, stored).await,
        }
    }
}

/// The login another process saved for the provider `id` in place of `stale`, when one did and its token is still good.
///
/// Renewing a login makes the token it had worthless, so of two processes that hold the same login only the first to renew it succeeds; the other finds what the first one saved here.
fn renewed_elsewhere(id: &str, stale: &Credentials) -> Option<Credentials> {
    store::load()
        .ok()?
        .credentials
        .remove(id)
        .filter(|found| found.is_login() && found.access != stale.access && !found.is_expired())
}

/// Renews `stored`, the login of the provider `id`, the way `renewal` says and saves the result; when the provider refuses because another process renewed the login first, what that process saved is used.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the provider refuses the refresh token and nothing newer was saved, or [`AuthError::Storage`] when the result cannot be saved.
async fn renew(
    id: &str,
    renewal: &Renewal,
    stored: &Credentials,
) -> Result<Credentials, AuthError> {
    let renewed = match renewal.run(stored).await {
        Ok(renewed) => renewed,
        Err(error) => return renewed_elsewhere(id, stored).ok_or(error),
    };
    let mut file = store::load()?;
    file.credentials.insert(id.to_string(), renewed.clone());
    store::save(&file)?;
    Ok(renewed)
}

/// Credentials for the provider `id` to use after it refused the access token of `refused`: the login as another process renewed it meanwhile, or else the login renewed now and saved.
///
/// A provider refuses a token that has not run out when the login was renewed elsewhere, which makes the token it had worthless. Returns nothing when `refused` is a key, or nothing is stored to renew, since asking again would then be refused the same way.
///
/// # Errors
///
/// Returns [`AuthError::Token`] when the provider refuses to renew the login, as after it was revoked, or [`AuthError::Storage`] when the logins cannot be read or saved.
pub async fn credentials_after_refusal(
    id: &str,
    auth: &'static Auth,
    refused: &Credentials,
) -> Result<Option<Credentials>, AuthError> {
    let Some(renewal) = Renewal::of(auth).filter(|_| refused.is_login()) else {
        return Ok(None);
    };
    if let Some(found) = renewed_elsewhere(id, refused) {
        return Ok(Some(found));
    }
    let Some(stored) = store::load()?
        .credentials
        .remove(id)
        .filter(Credentials::is_login)
    else {
        return Ok(None);
    };
    renew(id, &renewal, &stored).await.map(Some)
}

/// The key saved in `file` for the provider `id`, when one is saved and is not empty.
fn saved_key(file: &AuthFile, id: &str) -> Option<Credentials> {
    file.credentials
        .get(id)
        .filter(|stored| !stored.access.trim().is_empty())
        .cloned()
}

/// Returns the id of the provider messages are currently sent to, if any.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when `auth.json` cannot be read.
pub fn active_id() -> Result<Option<String>, AuthError> {
    Ok(store::load()?.active)
}

/// Returns the credentials stored for the provider `id` in the folder `home`, or in the default folder when there is none, as they were saved and without refreshing them.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when `auth.json` cannot be read.
pub fn stored(
    id: &str,
    home: Option<std::path::PathBuf>,
) -> Result<Option<Credentials>, AuthError> {
    Ok(store::load_in(home)?.credentials.remove(id))
}

/// Keeps `key` as the API key of the provider `id` in the folder `home`, or in the default folder when there is none, or forgets the key kept for it when `key` is nothing or blank.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when `auth.json` cannot be read or written.
pub fn save_key(
    id: &str,
    key: Option<&str>,
    home: Option<std::path::PathBuf>,
) -> Result<(), AuthError> {
    let mut file = store::load_in(home.clone())?;
    match key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => {
            file.credentials
                .insert(id.to_string(), Credentials::static_key(key.to_string()));
        }
        None => {
            file.credentials.remove(id);
        }
    }
    store::save_in(home, &file)
}

#[cfg(test)]
/// Tests for signing in to a cloud provider.
mod tests {
    use super::*;

    /// Says the cloud's credentials cannot be found.
    fn missing() -> Result<(), String> {
        Err("set CLOUD_THING".to_owned())
    }

    /// Says the cloud's credentials are there.
    fn found() -> Result<(), String> {
        Ok(())
    }

    /// A cloud whose credentials cannot be found.
    static ABSENT: Auth = Auth::Cloud {
        env: &["CRAB_TEST_CLOUD_KEY_NEVER_SET"],
        check: missing,
        credentials: "test cloud credentials",
    };

    /// A cloud whose credentials are there.
    static PRESENT: Auth = Auth::Cloud {
        env: &["CRAB_TEST_CLOUD_KEY_NEVER_SET"],
        check: found,
        credentials: "test cloud credentials",
    };

    /// Signing in fails with what to set when nothing is found, succeeds when the cloud's credentials are there, and a saved key is then used in their place.
    #[tokio::test]
    async fn cloud_sign_in_follows_the_check_and_the_saved_key() {
        let home = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-auth/cloud");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let refused = crate::with_home(Some(home.clone()), login("aws", &ABSENT, |_| {}))
            .await
            .unwrap_err();
        assert!(matches!(&refused, AuthError::NoCredentials(what) if what.contains("CLOUD_THING")));
        let ambient = crate::with_home(Some(home.clone()), credentials_for("aws", &ABSENT))
            .await
            .unwrap();
        assert!(ambient.access.is_empty());
        crate::with_home(Some(home.clone()), login("aws", &PRESENT, |_| {}))
            .await
            .unwrap();
        assert_eq!(
            crate::with_home(Some(home.clone()), async { active_id() })
                .await
                .unwrap()
                .as_deref(),
            Some("aws")
        );
        save_key("aws", Some("bearer-1"), Some(home.clone())).unwrap();
        let keyed = crate::with_home(Some(home.clone()), credentials_for("aws", &ABSENT))
            .await
            .unwrap();
        assert_eq!(keyed.access, "bearer-1");
        crate::with_home(Some(home.clone()), login("aws", &ABSENT, |_| {}))
            .await
            .unwrap();
        std::fs::remove_dir_all(home).unwrap();
    }

    /// A sign-in whose token endpoint is never reached by these tests.
    static BROWSER: Spec = Spec {
        client_id: "client",
        authorize_url: "https://example.invalid/authorize",
        token_url: "https://example.invalid/token",
        scopes: &[],
        authorize_params: &[],
        port: 1,
        path: "/callback",
        redirect_uri: "http://localhost:1/callback",
        json_body: true,
        state_in_exchange: true,
        refresh_headers: &[],
        account_from_jwt: false,
    };

    /// A provider signed in to through the browser.
    static SIGNED_IN: Auth = Auth::OAuth(&BROWSER);

    /// A login with the access token `access` that does not run out for an hour.
    fn login_with(access: &str) -> Credentials {
        let mut login = Credentials::static_key(access.to_owned());
        login.refresh = format!("refresh-{access}");
        login.expires_at_ms = crate::credentials::now_ms() + 3_600_000;
        login
    }

    /// A refused token is answered with the login another process saved meanwhile, without asking the provider; a refused key is answered with nothing.
    #[tokio::test]
    async fn a_refused_token_is_replaced_by_the_login_saved_meanwhile() {
        let home = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-auth/refused");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let mut file = AuthFile::default();
        file.credentials
            .insert("claude".to_owned(), login_with("new"));
        store::save_in(Some(home.clone()), &file).unwrap();
        let found = crate::with_home(
            Some(home.clone()),
            credentials_after_refusal("claude", &SIGNED_IN, &login_with("old")),
        )
        .await
        .unwrap();
        assert_eq!(found.unwrap().access, "new");
        let key = Credentials::static_key("key".to_owned());
        let none = crate::with_home(
            Some(home.clone()),
            credentials_after_refusal("claude", &SIGNED_IN, &key),
        )
        .await
        .unwrap();
        assert!(none.is_none());
        std::fs::remove_dir_all(home).unwrap();
    }
}
