//! Providers the user defines in `providers.json`, such as a company gateway or a local server crab has no definition for.
//!
//! The file lives beside `auth.json` in `~/.config/oh-my-crab`, or wherever
//! `OMC_PROVIDERS_FILE` points, and is read once, the first time the providers
//! are listed. Each entry becomes a [`Def`] like a built-in one, so a custom
//! provider is listed, signed in to and talked to as the built-in ones are.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::OnceLock;

use omc_auth::Auth;
use serde::Deserialize;

use crate::provider::{Def, Wire};
use crate::providers::Provider;

/// The environment variable that names another file to read the custom providers from.
const FILE_ENV: &str = "OMC_PROVIDERS_FILE";

/// A provider the user defined in `providers.json`, known by its place in that file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Custom(u16);

impl Custom {
    /// Every provider defined in configuration, in the order the file lists them.
    pub(crate) fn all() -> impl Iterator<Item = Custom> {
        (0..registry().len())
            .filter_map(|index| u16::try_from(index).ok())
            .map(Custom)
    }

    /// Returns the definition this provider was given in configuration.
    pub(crate) fn def(self) -> &'static Def {
        &registry()[usize::from(self.0)]
    }
}

/// The custom providers of this process, read from the configuration on first use.
fn registry() -> &'static [Def] {
    static REGISTRY: OnceLock<Vec<Def>> = OnceLock::new();
    REGISTRY.get_or_init(|| match load() {
        Ok(defs) => defs,
        Err(reason) => {
            eprintln!("custom providers ignored: {reason}");
            Vec::new()
        }
    })
}

/// Returns the path custom providers are read from: `OMC_PROVIDERS_FILE` when set, else `providers.json` in crab's config folder, or nothing when there is no config folder.
pub fn custom_providers_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(FILE_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|base| base.join("oh-my-crab").join("providers.json"))
}

/// Reads and checks the custom providers file and turns its entries into definitions; a missing file defines none.
///
/// # Errors
///
/// Fails with the reason when the file cannot be read, is not valid, or defines a provider wrongly.
fn load() -> Result<Vec<Def>, String> {
    let Some(path) = custom_providers_path() else {
        return Ok(Vec::new());
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let entries = parse(&text).map_err(|reason| format!("{}: {reason}", path.display()))?;
    Ok(entries.into_iter().map(CustomProvider::into_def).collect())
}

/// The shape of `providers.json`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// The providers the user defines, in the order they are offered.
    #[serde(default)]
    providers: Vec<CustomProvider>,
}

/// The environment variables an entry reads its API key from: one name or a list, tried in order.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(untagged)]
enum KeyEnv {
    /// A single variable.
    One(String),
    /// Several variables, the first set one wins.
    Many(Vec<String>),
}

impl KeyEnv {
    /// The variable names, in the order they are tried.
    fn names(self) -> Vec<String> {
        match self {
            KeyEnv::One(name) => vec![name],
            KeyEnv::Many(names) => names,
        }
    }
}

/// One provider as written in `providers.json`.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CustomProvider {
    /// The id it is chosen by, in lowercase letters, digits, `-` and `_`.
    id: String,
    /// The name clients show; the id when left out.
    #[serde(default)]
    label: Option<String>,
    /// The API address that `/chat/completions` and `/models` are appended to.
    base_url: String,
    /// The protocol it speaks; only `chat_completions`, the default, is accepted.
    #[serde(default)]
    wire: Option<String>,
    /// The variables the API key is read from; no key is sent when left out.
    #[serde(default)]
    api_key_env: Option<KeyEnv>,
    /// Headers sent on every request.
    #[serde(default)]
    headers: BTreeMap<String, String>,
    /// The model used when the user picks none.
    #[serde(default)]
    default_model: Option<String>,
    /// Whether reasoning goes back to the server on later requests, as [`Def::replay_reasoning`] says.
    #[serde(default)]
    replay_reasoning: bool,
}

/// Turns `text` into a `'static` string; the definitions of custom providers live as long as the process, and are made once.
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

impl CustomProvider {
    /// Returns the definition this entry describes; the entry must have passed [`parse`].
    fn into_def(self) -> Def {
        let auth = match self.api_key_env.map(KeyEnv::names) {
            Some(names) if !names.is_empty() => {
                let names: Vec<&'static str> = names.into_iter().map(leak).collect();
                Auth::ApiKey {
                    env: Box::leak(names.into_boxed_slice()),
                }
            }
            _ => Auth::None,
        };
        let headers: Vec<(&'static str, &'static str)> = self
            .headers
            .into_iter()
            .map(|(name, value)| (leak(name), leak(value)))
            .collect();
        let id = leak(self.id);
        Def {
            id,
            label: self.label.map_or(id, leak),
            wire: Wire::ChatCompletions,
            default_model: self.default_model.map(leak),
            auth,
            base_url: leak(self.base_url.trim_end_matches('/').to_owned()),
            base_url_env: None,
            headers: Box::leak(headers.into_boxed_slice()),
            replay_reasoning: self.replay_reasoning,
        }
    }

    /// Checks the entry on its own: its id, address, wire and key variables.
    ///
    /// # Errors
    ///
    /// Fails with the reason when a field is not usable.
    fn check(&self) -> Result<(), String> {
        let id = &self.id;
        let allowed =
            |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
        if id.is_empty() || !id.chars().all(allowed) {
            return Err(format!(
                "provider id {id:?} must be lowercase letters, digits, - and _"
            ));
        }
        if Provider::ALL.iter().any(|provider| provider.def().id == id) {
            return Err(format!("provider id {id} is already a built-in provider"));
        }
        if !(self.base_url.starts_with("http://") || self.base_url.starts_with("https://")) {
            return Err(format!(
                "{id}: base_url must start with http:// or https://"
            ));
        }
        match self.wire.as_deref() {
            None | Some("chat_completions") => {}
            Some(other) => {
                return Err(format!(
                    "{id}: wire {other:?} is not supported, only \"chat_completions\" is"
                ));
            }
        }
        let names = self.api_key_env.clone().map(KeyEnv::names);
        if names
            .iter()
            .flatten()
            .any(|name| name.trim().is_empty() || name.contains('='))
        {
            return Err(format!(
                "{id}: api_key_env names an empty or invalid variable"
            ));
        }
        Ok(())
    }
}

/// Reads the entries of a `providers.json` text and checks them.
///
/// # Errors
///
/// Fails with the reason when the text is not valid JSON of the expected shape,
/// an entry is not usable, or two entries share an id.
pub(crate) fn parse(text: &str) -> Result<Vec<CustomProvider>, String> {
    let file: File = serde_json::from_str(text).map_err(|error| error.to_string())?;
    let mut seen = HashSet::new();
    for entry in &file.providers {
        entry.check()?;
        if !seen.insert(entry.id.as_str()) {
            return Err(format!("provider id {} is defined twice", entry.id));
        }
    }
    Ok(file.providers)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full entry and a minimal one are read, in order, and become the definitions they describe.
    #[test]
    fn reads_custom_providers() {
        let text = r#"{"providers": [
            {
                "id": "work",
                "label": "Work gateway",
                "base_url": "https://llm.example.com/v1/",
                "wire": "chat_completions",
                "api_key_env": ["WORK_LLM_KEY", "LLM_KEY"],
                "headers": {"X-Team": "infra"},
                "default_model": "qwen-coder",
                "replay_reasoning": true
            },
            {"id": "box_2", "base_url": "http://192.168.1.9:8000/v1", "api_key_env": "BOX_KEY"},
            {"id": "open", "base_url": "http://localhost:5000/v1"}
        ]}"#;
        let defs: Vec<Def> = parse(text)
            .unwrap()
            .into_iter()
            .map(CustomProvider::into_def)
            .collect();
        assert_eq!(defs.len(), 3);
        let work = &defs[0];
        assert_eq!(work.id, "work");
        assert_eq!(work.label, "Work gateway");
        assert_eq!(work.wire, Wire::ChatCompletions);
        assert_eq!(work.base_url(), "https://llm.example.com/v1");
        assert_eq!(work.default_model, Some("qwen-coder"));
        assert_eq!(work.headers, &[("X-Team", "infra")]);
        assert!(work.replay_reasoning);
        assert!(matches!(work.auth, Auth::ApiKey { env } if env == ["WORK_LLM_KEY", "LLM_KEY"]));
        let small = &defs[1];
        assert_eq!(small.label, "box_2");
        assert_eq!(small.default_model, None);
        assert!(!small.replay_reasoning);
        assert!(matches!(small.auth, Auth::ApiKey { env } if env == ["BOX_KEY"]));
        assert!(matches!(defs[2].auth, Auth::None));
        assert!(parse("{}").unwrap().is_empty());
    }

    /// Entries that cannot work are refused with a reason naming what is wrong.
    #[test]
    fn refuses_bad_entries() {
        let refused =
            |entries: &str| parse(&format!(r#"{{"providers": [{entries}]}}"#)).unwrap_err();
        assert!(refused(r#"{"id": "Bad Id", "base_url": "http://x"}"#).contains("lowercase"));
        assert!(refused(r#"{"id": "claude", "base_url": "http://x"}"#).contains("built-in"));
        assert!(refused(r#"{"id": "a", "base_url": "ftp://x"}"#).contains("base_url"));
        assert!(
            refused(r#"{"id": "a", "base_url": "http://x", "wire": "anthropic_messages"}"#)
                .contains("not supported")
        );
        assert!(
            refused(r#"{"id": "a", "base_url": "http://x", "api_key_env": ""}"#)
                .contains("api_key_env")
        );
        assert!(
            refused(r#"{"id": "a", "base_url": "http://x"}, {"id": "a", "base_url": "http://y"}"#)
                .contains("twice")
        );
        assert!(
            refused(r#"{"id": "a", "base_url": "http://x", "key": "k"}"#).contains("unknown field")
        );
        assert!(refused(r#"{"id": "a"}"#).contains("base_url"));
    }
}
