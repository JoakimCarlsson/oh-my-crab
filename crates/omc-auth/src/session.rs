//! High-level login and credential retrieval used by the UI.

use crate::store::{self, AuthFile};
use crate::{AuthError, Credentials, Provider, flow};

/// Runs the browser login for `provider` and stores the result as active.
///
/// `notify` is called with the authorization URL.
///
/// # Errors
///
/// Returns [`AuthError`] when the flow fails or credentials cannot be saved.
pub async fn login(provider: Provider, notify: impl Fn(String)) -> Result<(), AuthError> {
    let credentials = flow::login(provider.spec(), notify).await?;
    let mut file: AuthFile = store::load()?;
    file.credentials.insert(provider, credentials);
    file.active = Some(provider);
    store::save(&file)
}

/// Returns the active provider with credentials that are valid right now.
///
/// Expired tokens are refreshed and the refreshed credentials are saved.
///
/// # Errors
///
/// Returns [`AuthError::NotLoggedIn`] when nothing is stored, or another
/// [`AuthError`] when refreshing or saving fails.
pub async fn active_credentials() -> Result<(Provider, Credentials), AuthError> {
    let mut file = store::load()?;
    let provider = file.active.ok_or(AuthError::NotLoggedIn)?;
    let stored = file
        .credentials
        .get(&provider)
        .cloned()
        .ok_or(AuthError::NotLoggedIn)?;
    if !stored.is_expired() {
        return Ok((provider, stored));
    }
    let renewed = flow::refresh(provider.spec(), &stored).await?;
    file.credentials.insert(provider, renewed.clone());
    store::save(&file)?;
    Ok((provider, renewed))
}

/// Returns the provider messages are currently sent to, if any.
///
/// # Errors
///
/// Returns [`AuthError::Storage`] when `auth.json` cannot be read.
pub fn active_provider() -> Result<Option<Provider>, AuthError> {
    Ok(store::load()?.active)
}
