//! The providers a conversation can be opened with, and who each is signed in as, read without opening one.

use std::path::PathBuf;

use omc_auth::Auth;
use omc_providers::Provider;
use omc_wire::{KeySource, ProviderInfo, SignIn};

/// How `auth` is signed in to, as a client is told.
fn sign_in(auth: &Auth) -> SignIn {
    match auth {
        Auth::OAuth(_) => SignIn::Browser,
        Auth::DeviceCode(_) | Auth::DeviceCodeOrKey { .. } => SignIn::DeviceCode,
        Auth::ApiKey { .. } => SignIn::ApiKey,
        Auth::None => SignIn::None,
    }
}

/// The environment variables `auth` reads an API key from, or none when it takes no key; a provider that signs in with a device code and also takes a key lists them too.
fn key_env(auth: &Auth) -> &'static [&'static str] {
    match auth {
        Auth::ApiKey { env } | Auth::DeviceCodeOrKey { env, .. } => env,
        _ => &[],
    }
}

/// Every provider a conversation can be opened with, in the order a client should offer them: the built-in ones, then those the user defined in `providers.json`.
pub fn providers() -> Vec<ProviderInfo> {
    Provider::all()
        .into_iter()
        .map(|provider| {
            let def = provider.def();
            ProviderInfo {
                id: def.id.to_owned(),
                name: def.label.to_owned(),
                sign_in: sign_in(&def.auth),
                key_env: key_env(&def.auth)
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
            }
        })
        .collect()
}

/// Who the provider `id` is signed in as with the logins kept in `home`, or in the harness's own folder when there is none: the organization or address its login reported.
///
/// Nothing is returned when the provider is unknown, nothing is stored for it, the login named no account, or the logins cannot be read.
pub fn account(id: &str, home: Option<PathBuf>) -> Option<String> {
    Provider::from_id(id)?;
    let stored = omc_auth::stored(id, home).ok()??;
    stored.account_name().map(str::to_owned)
}

/// Whether a login for the provider `id` is kept in `home`, or in the harness's own folder when there is none, whether or not it names an account.
pub fn signed_in(id: &str, home: Option<PathBuf>) -> bool {
    Provider::from_id(id).is_some() && matches!(omc_auth::stored(id, home), Ok(Some(_)))
}

/// Where the API key of the provider `id` comes from, with the logins kept in `home` or in the harness's own folder when there is none: the environment wins over a saved key.
///
/// Nothing is returned when the provider is unknown, takes no key, or has none; a login kept for a provider that also signs in to an account is not a key.
pub fn key_source(id: &str, home: Option<PathBuf>) -> Option<KeySource> {
    let env = key_env(&Provider::from_id(id)?.def().auth);
    if env.is_empty() {
        return None;
    }
    if let Some((name, _)) = omc_auth::env_key_named(env) {
        return Some(KeySource::Environment {
            name: name.to_owned(),
        });
    }
    omc_auth::stored(id, home)
        .ok()
        .flatten()
        .filter(|stored| !stored.is_login())
        .map(|_| KeySource::Saved)
}

/// Keeps `key` as the API key of the provider `id` with the logins in `home`, or in the harness's own folder when there is none, or forgets the saved key when `key` is nothing or blank.
///
/// # Errors
///
/// Fails with the reason when the provider is unknown, takes no key, or the logins cannot be read or written.
pub fn save_key(id: &str, key: Option<&str>, home: Option<PathBuf>) -> Result<(), String> {
    let provider = Provider::from_id(id).ok_or_else(|| format!("no provider is called {id}"))?;
    if key_env(&provider.def().auth).is_empty() {
        return Err(format!("{} takes no API key", provider.def().label));
    }
    omc_auth::save_key(id, key, home).map_err(|error| error.to_string())
}

#[cfg(test)]
/// Tests for the provider listing and the account lookup.
mod tests {
    use super::*;

    /// Every provider is listed once with its name and sign-in, in the harness's order.
    #[test]
    fn lists_every_provider() {
        let listed = providers();
        let ids: Vec<&str> = listed.iter().map(|info| info.id.as_str()).collect();
        let built_in: Vec<&str> = Provider::ALL.iter().map(|p| p.def().id).collect();
        assert_eq!(ids[..built_in.len()], built_in);
        assert_eq!(
            ids[..5],
            ["claude", "codex", "ollama", "openrouter", "copilot"]
        );
        let kimi = listed.iter().find(|info| info.id == "kimi-code").unwrap();
        assert_eq!(kimi.name, "Kimi Code");
        assert_eq!(kimi.sign_in, SignIn::DeviceCode);
        assert_eq!(kimi.key_env, ["KIMI_CODE_API_KEY", "KIMI_API_KEY"]);
        let lmstudio = listed.iter().find(|info| info.id == "lmstudio").unwrap();
        assert_eq!(lmstudio.sign_in, SignIn::None);
        let copilot = listed.iter().find(|info| info.id == "copilot").unwrap();
        assert_eq!(copilot.name, "GitHub Copilot");
        assert_eq!(copilot.sign_in, SignIn::DeviceCode);
        assert!(copilot.key_env.is_empty());
        let openrouter = listed.iter().find(|info| info.id == "openrouter").unwrap();
        assert_eq!(openrouter.key_env, ["OPENROUTER_API_KEY"]);
        assert!(listed.iter().all(|info| !info.name.is_empty()));
    }

    /// The account is read from the logins in the folder given, a login without a name still counts as signed in, and an unknown provider or empty folder has none.
    #[test]
    fn reads_the_account_from_the_folder() {
        let home =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/tests/omc-session/account");
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(account("codex", Some(home.clone())), None);
        assert!(!signed_in("copilot", Some(home.clone())));
        std::fs::write(
            home.join("auth.json"),
            r#"{"active":"codex","credentials":{
                "codex":{"access":"a","refresh":"r","expires_at_ms":1,"account_id":null,"email":"me@example.com"},
                "copilot":{"access":"a","refresh":"a","expires_at_ms":1,"account_id":null}}}"#,
        )
        .unwrap();
        assert_eq!(
            account("codex", Some(home.clone())).as_deref(),
            Some("me@example.com")
        );
        assert_eq!(account("copilot", Some(home.clone())), None);
        assert!(signed_in("copilot", Some(home.clone())));
        assert!(!signed_in("claude", Some(home.clone())));
        assert_eq!(account("nothing", Some(home.clone())), None);
        assert_eq!(key_source("openrouter", Some(home.clone())), None);
        save_key("openrouter", Some(" sk-or-1 "), Some(home.clone())).unwrap();
        let saved = omc_auth::stored("openrouter", Some(home.clone()))
            .unwrap()
            .unwrap();
        assert_eq!(saved.access, "sk-or-1");
        assert!(signed_in("codex", Some(home.clone())));
        save_key("openrouter", None, Some(home.clone())).unwrap();
        assert!(!signed_in("openrouter", Some(home.clone())));
        assert!(save_key("ollama", Some("k"), Some(home.clone())).is_err());
        assert_eq!(key_source("ollama", Some(home.clone())), None);
        if std::env::var_os("KIMI_CODE_API_KEY").is_none()
            && std::env::var_os("KIMI_API_KEY").is_none()
        {
            save_key("kimi-code", Some("sk-kimi"), Some(home.clone())).unwrap();
            assert_eq!(
                key_source("kimi-code", Some(home.clone())),
                Some(KeySource::Saved)
            );
            std::fs::write(
                home.join("auth.json"),
                r#"{"credentials":{"kimi-code":{"access":"a","refresh":"r","expires_at_ms":1,"account_id":"u-1"}}}"#,
            )
            .unwrap();
            assert_eq!(key_source("kimi-code", Some(home.clone())), None);
            assert!(signed_in("kimi-code", Some(home.clone())));
        }
        std::fs::remove_dir_all(home).unwrap();
    }
}
