//! High-level login and credential retrieval used by the UI.

use crate::store::{self, AuthFile};
use crate::{Auth, AuthError, Credentials, LoginPrompt, device, flow};

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
        Auth::Cloud { env, check } => {
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
    let mut file = store::load()?;
    let stored = file
        .credentials
        .get(id)
        .cloned()
        .ok_or(AuthError::NotLoggedIn)?;
    if !stored.is_expired() {
        return Ok(stored);
    }
    let renewed = flow::refresh(spec, &stored).await?;
    file.credentials.insert(id.to_string(), renewed.clone());
    store::save(&file)?;
    Ok(renewed)
}

/// The credentials stored for the provider `id`, which signs in with `device` or a saved key, renewed and saved again when they are a login whose token expired; `missing` when nothing is stored.
///
/// # Errors
///
/// Returns `missing` when nothing is stored, or another [`AuthError`] when renewing or saving fails.
async fn device_credentials(
    id: &str,
    device: &'static crate::Device,
    missing: AuthError,
) -> Result<Credentials, AuthError> {
    let mut file = store::load()?;
    let stored = file.credentials.get(id).cloned().ok_or(missing)?;
    if !stored.is_expired() {
        return Ok(stored);
    }
    let renewed = device::refresh(device, &stored).await?;
    file.credentials.insert(id.to_string(), renewed.clone());
    store::save(&file)?;
    Ok(renewed)
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
    };

    /// A cloud whose credentials are there.
    static PRESENT: Auth = Auth::Cloud {
        env: &["CRAB_TEST_CLOUD_KEY_NEVER_SET"],
        check: found,
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
}
