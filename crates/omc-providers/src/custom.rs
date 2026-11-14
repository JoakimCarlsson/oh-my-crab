//! OpenAI-compatible servers the user declares in `providers.json`, such as a company gateway, LiteLLM, vLLM, LM Studio or llama.cpp.
//!
//! The file lives beside `auth.json` in `~/.config/oh-my-crab`, or wherever
//! `OMC_PROVIDERS_FILE` points. It is read the first time the providers are
//! listed, and again whenever [`save_server`] or [`remove_server`] changes it.
//! Each entry becomes a [`Def`] like a built-in one, so a declared server is
//! listed, signed in to and talked to as the built-in providers are.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use omc_auth::{Auth, Credentials};
use omc_wire::{Server, ServerWire};
use serde::{Deserialize, Deserializer, Serialize};

use crate::provider::{Def, Wire};
use crate::providers::Provider;
use crate::wires::chat_completions;

/// The environment variable that names another file to read the custom providers from.
const FILE_ENV: &str = "OMC_PROVIDERS_FILE";

/// A server the user declared in `providers.json`, as it was read.
pub struct Loaded {
    /// Its definition, as a built-in provider has one.
    def: Def,
    /// The models it was given to offer; empty to ask the server.
    models: Vec<String>,
}

/// A provider the user defined in `providers.json`; two are the same when their ids are.
///
/// One read before the file changed keeps the definition it was read with.
#[derive(Clone, Copy)]
pub struct Custom(&'static Loaded);

impl Custom {
    /// Every provider defined in configuration, in the order the file lists them.
    pub(crate) fn all() -> impl Iterator<Item = Custom> {
        registry().into_iter()
    }

    /// Returns the definition this provider was given in configuration.
    pub(crate) fn def(self) -> &'static Def {
        &self.0.def
    }

    /// The models this provider was given to offer, in order, or nothing when its models are asked of the server.
    pub fn fixed_models(self) -> Option<Vec<String>> {
        (!self.0.models.is_empty()).then(|| self.0.models.clone())
    }
}

impl std::fmt::Debug for Custom {
    /// Writes the provider as `Custom("id")`.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("Custom")
            .field(&self.0.def.id)
            .finish()
    }
}

impl PartialEq for Custom {
    /// Whether both name the provider with the same id.
    fn eq(&self, other: &Self) -> bool {
        self.0.def.id == other.0.def.id
    }
}

impl Eq for Custom {}

impl PartialOrd for Custom {
    /// Orders the providers by id.
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Custom {
    /// Orders the providers by id.
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.def.id.cmp(other.0.def.id)
    }
}

impl Hash for Custom {
    /// Hashes the provider's id.
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.0.def.id.hash(state);
    }
}

/// The custom providers of this process, read from the configuration on first use and again after it changes.
static REGISTRY: RwLock<Option<Vec<Custom>>> = RwLock::new(None);

/// The custom providers of this process, reading the configuration when it was not read yet.
fn registry() -> Vec<Custom> {
    if let Some(loaded) = REGISTRY
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
    {
        return loaded.clone();
    }
    REGISTRY
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .get_or_insert_with(load_logged)
        .clone()
}

/// Reads the configuration again, so that the providers listed from now on are those it declares.
fn reload() {
    *REGISTRY.write().unwrap_or_else(PoisonError::into_inner) = Some(load_logged());
}

/// Reads the custom providers, saying on standard error why none are used when the file cannot be.
fn load_logged() -> Vec<Custom> {
    match load() {
        Ok(entries) => entries
            .into_iter()
            .map(|entry| Custom(Box::leak(Box::new(entry.into_loaded()))))
            .collect(),
        Err(reason) => {
            eprintln!("custom providers ignored: {reason}");
            Vec::new()
        }
    }
}

/// Returns the path custom providers are read from: `OMC_PROVIDERS_FILE` when set, else `providers.json` in crab's config folder, or nothing when there is no config folder.
pub fn custom_providers_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(FILE_ENV).filter(|path| !path.is_empty()) {
        return Some(PathBuf::from(path));
    }
    dirs::config_dir().map(|base| base.join("oh-my-crab").join("providers.json"))
}

/// Reads and checks the custom providers file; a missing file, or no config folder, defines none.
///
/// # Errors
///
/// Fails with the reason when the file cannot be read, is not valid, or defines a provider wrongly.
fn load() -> Result<Vec<Entry>, String> {
    match custom_providers_path() {
        Some(path) => read_file(&path),
        None => Ok(Vec::new()),
    }
}

/// Reads and checks the entries of the file at `path`; a missing file defines none.
///
/// # Errors
///
/// Fails with the reason, naming the file, when it cannot be read, is not valid, or defines a provider wrongly.
fn read_file(path: &Path) -> Result<Vec<Entry>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    parse(&text).map_err(|reason| format!("{}: {reason}", path.display()))
}

/// Checks `entries` and writes them to the file at `path`, making its folder when there is none.
///
/// # Errors
///
/// Fails with the reason when an entry is not usable, two share an id, or the file cannot be written.
fn write_file(path: &Path, entries: Vec<Entry>) -> Result<(), String> {
    check_all(&entries)?;
    let file = File { providers: entries };
    let text = serde_json::to_string_pretty(&file).map_err(|error| error.to_string())?;
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        std::fs::create_dir_all(folder)
            .map_err(|error| format!("{}: {error}", folder.display()))?;
    }
    std::fs::write(path, format!("{text}\n"))
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The shape of `providers.json`.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct File {
    /// The providers the user defines, in the order they are offered.
    #[serde(default)]
    providers: Vec<Entry>,
}

/// The environment variables an entry reads its API key from, written as one name or a list.
#[derive(Deserialize)]
#[serde(untagged)]
enum KeyEnv {
    /// A single variable.
    One(String),
    /// Several variables, the first set one wins.
    Many(Vec<String>),
}

/// Reads `api_key_env` as one variable name or a list of them, tried in order.
///
/// # Errors
///
/// Fails when the value is neither a string nor a list of strings.
fn one_or_many<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    Ok(match KeyEnv::deserialize(deserializer)? {
        KeyEnv::One(name) => vec![name],
        KeyEnv::Many(names) => names,
    })
}

/// Whether `value` is false, so that a flag left at its default is not written.
fn is_false(value: &bool) -> bool {
    !value
}

/// One provider as written in `providers.json`.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    /// The id it is chosen by, in lowercase letters, digits, `-` and `_`.
    id: String,
    /// The name clients show; the id when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
    /// The API address that `/chat/completions`, `/responses` and `/models` are appended to.
    base_url: String,
    /// The protocol it speaks, `chat_completions` (the default) or `responses`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    wire: Option<String>,
    /// The variables the API key is read from, in order.
    #[serde(
        default,
        deserialize_with = "one_or_many",
        skip_serializing_if = "Vec::is_empty"
    )]
    api_key_env: Vec<String>,
    /// The API key itself, used when no variable is named.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api_key: Option<String>,
    /// Headers sent on every request.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    headers: BTreeMap<String, String>,
    /// The model used when the user picks none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_model: Option<String>,
    /// The models offered, in order; empty to ask the server.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    models: Vec<String>,
    /// Whether reasoning goes back to the server on later requests, as [`Def::replay_reasoning`] says.
    #[serde(default, skip_serializing_if = "is_false")]
    replay_reasoning: bool,
}

/// Turns `text` into a `'static` string; the definitions of custom providers live as long as the process.
fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

/// The wire `name` in `providers.json` stands for, when it names one.
fn wire_named(name: Option<&str>) -> Option<Wire> {
    match name {
        None | Some("chat_completions") => Some(Wire::ChatCompletions),
        Some("responses") => Some(Wire::OpenAiResponses),
        Some(_) => None,
    }
}

impl Entry {
    /// Returns the provider this entry describes; the entry must have passed [`check_all`].
    fn into_loaded(self) -> Loaded {
        let auth = if self.api_key_env.is_empty() {
            Auth::None
        } else {
            let names: Vec<&'static str> = self.api_key_env.into_iter().map(leak).collect();
            Auth::ApiKey {
                env: Box::leak(names.into_boxed_slice()),
            }
        };
        let key = self
            .api_key
            .filter(|key| !key.is_empty() && matches!(auth, Auth::None))
            .map(|key| ("Authorization".to_owned(), format!("Bearer {key}")));
        let headers: Vec<(&'static str, &'static str)> = self
            .headers
            .into_iter()
            .chain(key)
            .map(|(name, value)| (leak(name), leak(value)))
            .collect();
        let id = leak(self.id);
        Loaded {
            def: Def {
                id,
                label: self.label.map_or(id, leak),
                wire: wire_named(self.wire.as_deref()).unwrap_or(Wire::ChatCompletions),
                default_model: self.default_model.map(leak),
                auth,
                base_url: leak(self.base_url.trim_end_matches('/').to_owned()),
                base_url_env: None,
                headers: Box::leak(headers.into_boxed_slice()),
                replay_reasoning: self.replay_reasoning,
            },
            models: self.models,
        }
    }

    /// Checks the entry on its own: its id, address, wire, key and models.
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
        if wire_named(self.wire.as_deref()).is_none() {
            return Err(format!(
                "{id}: wire {:?} is not supported, only \"chat_completions\" and \"responses\" are",
                self.wire.as_deref().unwrap_or_default()
            ));
        }
        if self
            .api_key_env
            .iter()
            .any(|name| name.trim().is_empty() || name.contains('='))
        {
            return Err(format!(
                "{id}: api_key_env names an empty or invalid variable"
            ));
        }
        if !self.api_key_env.is_empty()
            && self.api_key.as_deref().is_some_and(|key| !key.is_empty())
        {
            return Err(format!("{id}: give api_key or api_key_env, not both"));
        }
        if self.models.iter().any(|model| model.trim().is_empty()) {
            return Err(format!("{id}: models names an empty model"));
        }
        Ok(())
    }

    /// The entry `server` describes; a server sent without a key keeps the one `saved` has, and one sent with an empty key has none.
    fn from_wire(server: Server, saved: Option<&Entry>) -> Entry {
        let api_key = match server.api_key {
            None => saved.and_then(|saved| saved.api_key.clone()),
            Some(key) if key.trim().is_empty() => None,
            Some(key) => Some(key.trim().to_owned()),
        };
        let filled = |text: Option<String>| text.filter(|text| !text.trim().is_empty());
        Entry {
            id: server.id.trim().to_owned(),
            label: filled(server.label),
            base_url: server.base_url.trim().trim_end_matches('/').to_owned(),
            wire: match server.wire {
                ServerWire::ChatCompletions => None,
                ServerWire::Responses => Some("responses".to_owned()),
            },
            api_key_env: server.api_key_env,
            api_key,
            headers: server.headers.into_iter().collect(),
            default_model: filled(server.default_model),
            models: server.models,
            replay_reasoning: server.replay_reasoning,
        }
    }

    /// The server this entry describes, as a client is told it: without its key, saying whether it has one.
    fn to_wire(&self) -> Server {
        Server {
            id: self.id.clone(),
            label: self.label.clone(),
            base_url: self.base_url.clone(),
            wire: match self.wire.as_deref() {
                Some("responses") => ServerWire::Responses,
                _ => ServerWire::ChatCompletions,
            },
            api_key_env: self.api_key_env.clone(),
            api_key: None,
            has_api_key: self.api_key.as_deref().is_some_and(|key| !key.is_empty()),
            headers: self
                .headers
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect(),
            default_model: self.default_model.clone(),
            models: self.models.clone(),
            replay_reasoning: self.replay_reasoning,
        }
    }

    /// The API key requests carry: the first set variable of `api_key_env`, else `api_key`, else none.
    fn key(&self) -> String {
        self.api_key_env
            .iter()
            .find_map(|name| {
                std::env::var(name)
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .or_else(|| self.api_key.clone())
            .unwrap_or_default()
    }
}

/// Checks every entry and that no two share an id.
///
/// # Errors
///
/// Fails with the reason when an entry is not usable or an id is defined twice.
fn check_all(entries: &[Entry]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for entry in entries {
        entry.check()?;
        if !seen.insert(entry.id.as_str()) {
            return Err(format!("provider id {} is defined twice", entry.id));
        }
    }
    Ok(())
}

/// Reads the entries of a `providers.json` text and checks them.
///
/// # Errors
///
/// Fails with the reason when the text is not valid JSON of the expected shape,
/// an entry is not usable, or two entries share an id.
fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let file: File = serde_json::from_str(text).map_err(|error| error.to_string())?;
    check_all(&file.providers)?;
    Ok(file.providers)
}

/// The path servers are saved to, or why there is none.
///
/// # Errors
///
/// Fails when there is no config folder and `OMC_PROVIDERS_FILE` is not set.
fn file_path() -> Result<PathBuf, String> {
    custom_providers_path().ok_or_else(|| {
        format!("there is no config folder; set {FILE_ENV} to a file to keep servers in")
    })
}

/// The servers the user declared, in the order they are offered, without their keys.
///
/// # Errors
///
/// Fails with the reason when the file cannot be read or is not valid.
pub fn servers() -> Result<Vec<Server>, String> {
    Ok(load()?.iter().map(Entry::to_wire).collect())
}

/// Adds `server` to the file at `path`, or puts it in place of the entry with the same id, and returns the servers it then holds.
///
/// # Errors
///
/// Fails with the reason, writing nothing, when the file cannot be read, the server is not valid, or the file cannot be written.
fn save_to(path: &Path, server: Server) -> Result<Vec<Server>, String> {
    let mut entries = read_file(path)?;
    let id = server.id.trim().to_owned();
    let position = entries.iter().position(|entry| entry.id == id);
    let entry = Entry::from_wire(server, position.map(|at| &entries[at]));
    match position {
        Some(at) => entries[at] = entry,
        None => entries.push(entry),
    }
    let listed = entries.iter().map(Entry::to_wire).collect();
    write_file(path, entries)?;
    Ok(listed)
}

/// Takes the entry `id` out of the file at `path` and returns the servers left.
///
/// # Errors
///
/// Fails with the reason when the file cannot be read or written, or no server has the id.
fn remove_from(path: &Path, id: &str) -> Result<Vec<Server>, String> {
    let mut entries = read_file(path)?;
    let before = entries.len();
    entries.retain(|entry| entry.id != id);
    if entries.len() == before {
        return Err(format!("no server is called {id}"));
    }
    let listed = entries.iter().map(Entry::to_wire).collect();
    write_file(path, entries)?;
    Ok(listed)
}

/// Adds `server` to the configuration, or replaces the one with the same id, and returns the servers it then declares; providers listed from now on include it.
///
/// A server given without a key keeps the key saved for its id, and one given an empty key has none.
///
/// # Errors
///
/// Fails with the reason, changing nothing, when the configuration cannot be read or written or the server is not valid.
pub fn save_server(server: Server) -> Result<Vec<Server>, String> {
    let listed = save_to(&file_path()?, server)?;
    reload();
    Ok(listed)
}

/// Takes the server `id` out of the configuration and returns the servers left; providers listed from now on leave it out.
///
/// # Errors
///
/// Fails with the reason when the configuration cannot be read or written, or no server has the id.
pub fn remove_server(id: &str) -> Result<Vec<Server>, String> {
    let listed = remove_from(&file_path()?, id.trim())?;
    reload();
    Ok(listed)
}

/// The models `server` offers, saved or not: those it was given, or those it lists at `GET {base_url}/models`.
///
/// A server given without a key is asked with the key saved for its id.
///
/// # Errors
///
/// Fails with the reason when the server is not valid, cannot be reached, or answers with an error.
pub async fn find_models(server: Server) -> Result<Vec<String>, String> {
    let saved = load().unwrap_or_default();
    let id = server.id.trim().to_owned();
    let entry = Entry::from_wire(server, saved.iter().find(|entry| entry.id == id));
    entry.check()?;
    discover(&entry).await
}

/// The models `entry` offers: those it was given, or those its server lists.
///
/// # Errors
///
/// Fails with the reason when the server cannot be reached or answers with an error.
async fn discover(entry: &Entry) -> Result<Vec<String>, String> {
    if !entry.models.is_empty() {
        return Ok(entry.models.clone());
    }
    let credentials = Credentials::static_key(entry.key());
    let headers: Vec<(&str, &str)> = entry
        .headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    chat_completions::list_models(chat_completions::Target {
        credentials: &credentials,
        base_url: entry.base_url.trim_end_matches('/'),
        headers: &headers,
        replay_reasoning: false,
    })
    .await
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Turns the entries of `text` into the providers they describe.
    fn loaded(text: &str) -> Vec<Loaded> {
        parse(text)
            .unwrap()
            .into_iter()
            .map(Entry::into_loaded)
            .collect()
    }

    /// A full entry and minimal ones are read, in order, and become the definitions they describe.
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
            {"id": "open", "base_url": "http://localhost:5000/v1"},
            {"id": "lite", "base_url": "http://localhost:4000", "wire": "responses",
             "api_key": "sk-1", "models": ["gpt-5", "o4-mini"]}
        ]}"#;
        let loaded = loaded(text);
        assert_eq!(loaded.len(), 4);
        let work = &loaded[0].def;
        assert_eq!(work.id, "work");
        assert_eq!(work.label, "Work gateway");
        assert_eq!(work.wire, Wire::ChatCompletions);
        assert_eq!(work.base_url(), "https://llm.example.com/v1");
        assert_eq!(work.default_model, Some("qwen-coder"));
        assert_eq!(work.headers, &[("X-Team", "infra")]);
        assert!(work.replay_reasoning);
        assert!(matches!(work.auth, Auth::ApiKey { env } if env == ["WORK_LLM_KEY", "LLM_KEY"]));
        let small = &loaded[1].def;
        assert_eq!(small.label, "box_2");
        assert_eq!(small.default_model, None);
        assert!(!small.replay_reasoning);
        assert!(matches!(small.auth, Auth::ApiKey { env } if env == ["BOX_KEY"]));
        assert!(matches!(loaded[2].def.auth, Auth::None));
        assert!(loaded[2].models.is_empty());
        let lite = &loaded[3];
        assert_eq!(lite.def.wire, Wire::OpenAiResponses);
        assert!(matches!(lite.def.auth, Auth::None));
        assert_eq!(lite.def.headers, &[("Authorization", "Bearer sk-1")]);
        assert_eq!(lite.models, ["gpt-5", "o4-mini"]);
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
            refused(r#"{"id": "a", "base_url": "http://x", "api_key_env": "K", "api_key": "k"}"#)
                .contains("not both")
        );
        assert!(
            refused(r#"{"id": "a", "base_url": "http://x", "models": [""]}"#).contains("models")
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

    /// A fresh file under the build folder for the test `name`, removed if a run before left it.
    fn scratch(name: &str) -> PathBuf {
        let folder = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-providers")
            .join(name);
        let _ = std::fs::remove_dir_all(&folder);
        folder.join("providers.json")
    }

    /// A server with only an id and an address.
    fn server(id: &str, base_url: &str) -> Server {
        Server {
            id: id.to_owned(),
            label: None,
            base_url: base_url.to_owned(),
            wire: ServerWire::ChatCompletions,
            api_key_env: Vec::new(),
            api_key: None,
            has_api_key: false,
            headers: Vec::new(),
            default_model: None,
            models: Vec::new(),
            replay_reasoning: false,
        }
    }

    /// Servers are added, edited in place keeping their key, and removed, and the file reads back as written; bad ones change nothing.
    #[test]
    fn saves_edits_and_removes_servers() {
        let path = scratch("save");
        let mut lite = server("lite", "http://localhost:4000/");
        lite.wire = ServerWire::Responses;
        lite.api_key = Some("sk-1".to_owned());
        lite.models = vec!["gpt-5".to_owned()];
        let listed = save_to(&path, lite.clone()).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].base_url, "http://localhost:4000");
        assert!(listed[0].has_api_key && listed[0].api_key.is_none());
        save_to(&path, server("gpu-box", "http://localhost:8000/v1")).unwrap();
        let mut edited = listed[0].clone();
        edited.label = Some("LiteLLM".to_owned());
        let listed = save_to(&path, edited).unwrap();
        assert_eq!(
            listed.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            ["lite", "gpu-box"]
        );
        assert_eq!(listed[0].label.as_deref(), Some("LiteLLM"));
        assert!(listed[0].has_api_key);
        let written = read_file(&path).unwrap();
        assert_eq!(written[0].api_key.as_deref(), Some("sk-1"));
        assert_eq!(written[0].wire.as_deref(), Some("responses"));
        assert!(
            save_to(&path, server("claude", "http://x"))
                .unwrap_err()
                .contains("built-in")
        );
        assert_eq!(read_file(&path).unwrap().len(), 2);
        let mut cleared = listed[0].clone();
        cleared.api_key = Some(String::new());
        assert!(!save_to(&path, cleared).unwrap()[0].has_api_key);
        let left = remove_from(&path, "lite").unwrap();
        assert_eq!(left.len(), 1);
        assert!(
            remove_from(&path, "lite")
                .unwrap_err()
                .contains("no server")
        );
    }

    /// Serves `body` as JSON once on a local port and returns the address and the request it received.
    fn serve_once(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            let mut buffer = [0u8; 4096];
            while !String::from_utf8_lossy(&received).contains("\r\n\r\n") {
                let read = stream.read(&mut buffer).unwrap();
                received.extend_from_slice(&buffer[..read]);
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).unwrap();
            String::from_utf8_lossy(&received).to_string()
        });
        (address, handle)
    }

    /// Finding models asks the server's `/models` with its key and headers, and a fixed list is given without asking.
    #[tokio::test]
    async fn finds_models() {
        let (address, served) = serve_once(
            r#"{"object":"list","data":[{"id":"qwen3-coder"},{"id":"text-embedding-3"},{"id":"llama-3.3"}]}"#,
        );
        let mut entry = Entry::from_wire(server("box", &format!("{address}/v1")), None);
        entry.api_key = Some("sk-box".to_owned());
        entry
            .headers
            .insert("X-Team".to_owned(), "infra".to_owned());
        assert_eq!(
            discover(&entry).await.unwrap(),
            ["qwen3-coder", "llama-3.3"]
        );
        let received = served.join().unwrap().to_ascii_lowercase();
        assert!(received.starts_with("get /v1/models"), "{received}");
        assert!(received.contains("authorization: bearer sk-box"));
        assert!(received.contains("x-team: infra"));
        entry.models = vec!["fixed".to_owned()];
        assert_eq!(discover(&entry).await.unwrap(), ["fixed"]);
    }
}
