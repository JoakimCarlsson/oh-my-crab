//! High-level login and credential retrieval used by the UI.

use crate::store::{self, AuthFile};
use crate::{Auth, AuthError, Credentials, LoginPrompt, device, flow};

/// Returns the first of the environment variables `names` that holds a key.
fn env_key(names: &[&str]) -> Option<String> {
    names
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|value| value.trim().to_string())
        .find(|value| !value.is_empty())
}

/// Signs in to the provider `id` the way `auth` says and stores the result as active.
///
/// An OAuth provider runs the browser flow and `notify` is told the
/// authorization URL; a device-code provider is told the page and the code.
/// An API-key provider reads its key from the environment, and a provider
/// without credentials just becomes active.
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
        Auth::DeviceCode(spec) => device::login(spec, notify).await?,
        Auth::ApiKey { env } => Credentials::static_key(
            env_key(env).ok_or_else(|| AuthError::MissingKey(env.join(" or ")))?,
        ),
        Auth::None => Credentials::static_key(String::new()),
    };
    let mut file: AuthFile = store::load()?;
    file.credentials.insert(id.to_string(), credentials);
    file.active = Some(id.to_string());
    store::save(&file)
}

/// Returns credentials for the provider `id` that are valid right now.
///
/// Expired OAuth tokens are refreshed and the refreshed credentials are saved.
/// An API key from the environment is preferred over a stored one.
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
        Auth::DeviceCode(_) => {
            return store::load()?
                .credentials
                .remove(id)
                .ok_or(AuthError::NotLoggedIn);
        }
        Auth::None => return Ok(Credentials::static_key(String::new())),
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

/// Returns the id of the provider messages are currently sent to, if any.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when `auth.json` cannot be read.
pub fn active_id() -> Result<Option<String>, AuthError> {
    Ok(store::load()?.active)
}
