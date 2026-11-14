//! The model catalog: what the models of each provider take in, put out, can do and cost.
//!
//! A snapshot of the catalog is compiled in from `catalog/models.json`, which
//! `make catalog` regenerates from <https://models.dev/api.json> (MIT licensed;
//! see `catalog/LICENSE`). A fresher copy in the same format, kept in crab's
//! cache folder as `models.json`, takes the place of the snapshot's entries for
//! every provider it describes. [`refresh`] downloads that copy at most once a
//! day, unless `OMC_MODELS_REFRESH` turns it off; nothing ever waits on it, and
//! crab works offline with the snapshot alone.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Effort, Provider};

/// The snapshot compiled into crab.
const BUNDLED: &str = include_str!("../catalog/models.json");

/// Where the catalog is downloaded from when `OMC_MODELS_URL` does not say.
pub(crate) const SOURCE_URL: &str = "https://models.dev/api.json";

/// The licence notice the catalog's data is used under, kept with every copy of it.
const LICENSE: &str =
    "MIT License, Copyright (c) 2025 models.dev; see https://github.com/anomalyco/models.dev";

/// How old the cached copy may get before [`refresh`] downloads it again.
const REFRESH_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// How long [`refresh`] waits for the download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(20);

/// The catalog's names of the providers crab has definitions for; only these are kept in a snapshot.
pub(crate) const SOURCES: &[&str] = &[
    "amazon-bedrock",
    "anthropic",
    "cerebras",
    "deepseek",
    "fireworks-ai",
    "github-copilot",
    "google",
    "google-vertex-anthropic",
    "groq",
    "kimi-code-plan-global",
    "minimax",
    "mistral",
    "moonshotai",
    "openai",
    "openrouter",
    "togetherai",
    "xai",
    "zai",
    "zai-coding-plan",
];

/// The catalog's names for the built-in provider `id`, searched in order; none for a provider the catalog does not describe.
///
/// Codex is left out on purpose: the ChatGPT backend serves smaller context
/// windows than the OpenAI API the catalog describes, and its use is paid by
/// subscription, so its own heuristics stay in charge.
fn sources_of(id: &str) -> &'static [&'static str] {
    match id {
        "claude" | "anthropic" => &["anthropic"],
        "bedrock" => &["amazon-bedrock"],
        "vertex" => &["google-vertex-anthropic"],
        "copilot" => &["github-copilot"],
        "openrouter" => &["openrouter"],
        "kimi-code" => &["kimi-code-plan-global"],
        "moonshot" => &["moonshotai"],
        "zai-coding" => &["zai-coding-plan"],
        "zai" => &["zai"],
        "minimax" => &["minimax"],
        "deepseek" => &["deepseek"],
        "xai" => &["xai"],
        "groq" => &["groq"],
        "cerebras" => &["cerebras"],
        "mistral" => &["mistral"],
        "openai" => &["openai"],
        "gemini" => &["google"],
        "together" => &["togetherai"],
        "fireworks" => &["fireworks-ai"],
        _ => &[],
    }
}

/// What one model of one provider takes in, puts out, can do and costs, as the catalog has it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Entry {
    /// The context window, in tokens.
    pub context: u64,
    /// The most input tokens a request may carry, when the provider limits them below the context window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_limit: Option<u64>,
    /// The most output tokens one reply may have.
    pub output: u64,
    /// Whether the model reasons before it answers.
    #[serde(default)]
    pub reasoning: bool,
    /// The reasoning efforts the model takes, as the catalog names them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
    /// The kinds of input the model takes, such as `text`, `image` and `pdf`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input: Vec<String>,
    /// What the model costs, when the catalog says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
    /// Whether the model takes tool calls, when the catalog says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools: Option<bool>,
    /// Whether the catalog marks the model as deprecated.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

impl Entry {
    /// How many tokens of context a request may hold: the input limit when the provider sets one, else the context window.
    pub fn window(&self) -> u64 {
        self.input_limit
            .filter(|limit| *limit > 0)
            .unwrap_or(self.context)
    }

    /// The efforts of [`Entry::efforts`] that crab knows, lightest first.
    pub fn levels(&self) -> Vec<Effort> {
        Effort::ALL
            .into_iter()
            .filter(|level| self.efforts.iter().any(|name| name == level.as_str()))
            .collect()
    }
}

/// What a model costs, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Cost {
    /// Input that is neither read from nor written to a cache.
    pub input: f64,
    /// Output, reasoning included.
    pub output: f64,
    /// Input read from a cache, when the catalog says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Input written to a cache, when the catalog says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// A whole catalog: the models of each provider by id, under the catalog's names of the providers.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    /// Where the data came from.
    #[serde(default)]
    pub source: String,
    /// The licence the data is used under.
    #[serde(default)]
    pub license: String,
    /// The models of each provider by id.
    #[serde(default)]
    pub providers: BTreeMap<String, BTreeMap<String, Entry>>,
}

/// The catalog in use: the bundled snapshot with the cached copy's providers put in place of its own.
fn loaded() -> &'static Snapshot {
    static CATALOG: OnceLock<Snapshot> = OnceLock::new();
    CATALOG.get_or_init(|| {
        let mut catalog: Snapshot = serde_json::from_str(BUNDLED).unwrap_or_default();
        if let Some(cached) = cache_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice::<Snapshot>(&bytes).ok())
        {
            catalog.providers.extend(cached.providers);
        }
        catalog
    })
}

/// The file the downloaded copy of the catalog is kept in: `models.json` in crab's cache folder.
pub(crate) fn cache_path() -> Option<PathBuf> {
    Some(dirs::cache_dir()?.join("oh-my-crab").join("models.json"))
}

/// The ids `model` may be listed under: as given, then without a version tag after `@`, with `@default`, without a trailing date, and without a Bedrock version suffix such as `-v1:0`.
fn candidates(model: &str) -> Vec<String> {
    let mut found = vec![model.to_owned()];
    let mut push = |id: String| {
        if !id.is_empty() && !found.contains(&id) {
            found.push(id);
        }
    };
    let untagged = model.split('@').next().unwrap_or(model);
    push(untagged.to_owned());
    push(format!("{untagged}@default"));
    let unversioned = strip_bedrock_version(untagged);
    push(unversioned.to_owned());
    push(strip_date(unversioned).to_owned());
    found
}

/// `id` without a trailing Bedrock version such as `-v1:0` or `-v1`.
fn strip_bedrock_version(id: &str) -> &str {
    let Some((head, tail)) = id.rsplit_once("-v") else {
        return id;
    };
    let version = tail.split(':').collect::<Vec<_>>();
    let numeric = !version.is_empty()
        && version
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
    if numeric { head } else { id }
}

/// `id` without a trailing eight-digit date such as `-20250929`.
fn strip_date(id: &str) -> &str {
    match id.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.chars().all(|c| c.is_ascii_digit()) => head,
        _ => id,
    }
}

/// The Claude model id at the heart of `id`, as the Anthropic API names it: without a Bedrock region and vendor prefix such as `us.anthropic.`, a Vertex version tag after `@`, or a Bedrock version suffix.
pub(crate) fn claude_id(id: &str) -> &str {
    let id = id.rsplit_once("anthropic.").map_or(id, |(_, rest)| rest);
    let id = id.split('@').next().unwrap_or(id);
    strip_bedrock_version(id)
}

/// The entry of `model` in the catalog's provider `source`, trying the forms of [`candidates`] and then a case-insensitive match.
fn find_in(catalog: &Snapshot, source: &str, model: &str) -> Option<Entry> {
    let models = catalog.providers.get(source)?;
    candidates(model)
        .iter()
        .find_map(|id| models.get(id))
        .or_else(|| {
            models
                .iter()
                .find(|(id, _)| id.eq_ignore_ascii_case(model))
                .map(|(_, entry)| entry)
        })
        .cloned()
}

/// The catalog's entry for `model` of `provider`, when the catalog describes it.
pub(crate) fn find(provider: Provider, model: &str) -> Option<Entry> {
    let catalog = loaded();
    sources_of(provider.def().id)
        .iter()
        .find_map(|source| find_in(catalog, source, model))
}

/// The catalog's entry for the Claude model at the heart of `id` as Anthropic itself serves it, whichever host's form `id` is written in.
pub(crate) fn claude(id: &str) -> Option<Entry> {
    let catalog = loaded();
    find_in(catalog, "anthropic", id).or_else(|| find_in(catalog, "anthropic", claude_id(id)))
}

/// The ids the catalog lists for the provider `source`, in the catalog's order.
pub(crate) fn listed(source: &str) -> Vec<String> {
    loaded()
        .providers
        .get(source)
        .map(|models| models.keys().cloned().collect())
        .unwrap_or_default()
}

/// The models.dev description of one model, of which only what crab uses is read.
#[derive(Deserialize)]
struct SourceModel {
    /// The kinds of input and output the model handles.
    #[serde(default)]
    modalities: Modalities,
    /// The model's token limits.
    #[serde(default)]
    limit: Limit,
    /// Whether the model reasons.
    #[serde(default)]
    reasoning: bool,
    /// How the model's reasoning is steered.
    #[serde(default)]
    reasoning_options: Vec<Value>,
    /// What the model costs.
    #[serde(default)]
    cost: Option<SourceCost>,
    /// Whether the model takes tool calls.
    #[serde(default)]
    tool_call: Option<bool>,
    /// Where the model stands, such as `deprecated`.
    #[serde(default)]
    status: Option<String>,
}

/// The kinds of input and output a models.dev model handles.
#[derive(Deserialize, Default)]
struct Modalities {
    /// The kinds of input.
    #[serde(default)]
    input: Vec<String>,
    /// The kinds of output.
    #[serde(default)]
    output: Vec<String>,
}

/// The token limits of a models.dev model.
#[derive(Deserialize, Default)]
struct Limit {
    /// The context window.
    #[serde(default)]
    context: u64,
    /// The most input tokens, when limited below the context window.
    #[serde(default)]
    input: Option<u64>,
    /// The most output tokens.
    #[serde(default)]
    output: u64,
}

/// The prices of a models.dev model, of which the base tier is read.
#[derive(Deserialize)]
struct SourceCost {
    /// Input price.
    #[serde(default)]
    input: f64,
    /// Output price.
    #[serde(default)]
    output: f64,
    /// Cache read price.
    #[serde(default)]
    cache_read: Option<f64>,
    /// Cache write price.
    #[serde(default)]
    cache_write: Option<f64>,
}

/// The provider part of a models.dev catalog.
#[derive(Deserialize)]
struct SourceProvider {
    /// The provider's models by id.
    #[serde(default)]
    models: BTreeMap<String, Value>,
}

/// The entry crab keeps for the models.dev model `model`, or none when it puts out no text or has no context window.
fn entry_of(model: &Value) -> Option<Entry> {
    let model: SourceModel = serde_json::from_value(model.clone()).ok()?;
    if model.limit.context == 0 || !model.modalities.output.iter().any(|kind| kind == "text") {
        return None;
    }
    let efforts = model
        .reasoning_options
        .iter()
        .filter(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        .filter_map(|option| option.get("values").and_then(Value::as_array))
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect();
    Some(Entry {
        context: model.limit.context,
        input_limit: model.limit.input.filter(|limit| *limit > 0),
        output: model.limit.output,
        reasoning: model.reasoning,
        efforts,
        input: model.modalities.input,
        cost: model.cost.map(|cost| Cost {
            input: cost.input,
            output: cost.output,
            cache_read: cost.cache_read,
            cache_write: cost.cache_write,
        }),
        tools: model.tool_call,
        deprecated: model.status.as_deref() == Some("deprecated"),
    })
}

/// The models the catalog lists for `provider` that are not deprecated and do not say they refuse tool calls, by id, in the catalog's order.
pub(crate) fn current(provider: Provider) -> Vec<String> {
    let catalog = loaded();
    sources_of(provider.def().id)
        .iter()
        .filter_map(|source| catalog.providers.get(*source))
        .flat_map(|models| models.iter())
        .filter(|(_, entry)| !entry.deprecated && entry.tools != Some(false))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Builds the catalog crab keeps from the models.dev catalog `raw`: the providers of [`SOURCES`], with what crab reads of each text model.
///
/// # Errors
///
/// Fails when `raw` is not JSON or not shaped as a models.dev catalog.
pub(crate) fn from_models_dev(raw: &str) -> Result<Snapshot, serde_json::Error> {
    let all: BTreeMap<String, SourceProvider> = serde_json::from_str(raw)?;
    let providers = all
        .into_iter()
        .filter(|(id, _)| SOURCES.contains(&id.as_str()))
        .map(|(id, provider)| {
            let models = provider
                .models
                .iter()
                .filter_map(|(model, value)| Some((model.clone(), entry_of(value)?)))
                .collect();
            (id, models)
        })
        .collect();
    Ok(Snapshot {
        source: SOURCE_URL.to_owned(),
        license: LICENSE.to_owned(),
        providers,
    })
}

/// Writes `snapshot` as JSON with one model a line, so that a regenerated snapshot differs from the last one only where the models did.
pub(crate) fn render(snapshot: &Snapshot) -> String {
    let text = |value: &str| serde_json::to_string(value).unwrap_or_default();
    let mut out = String::from("{\n");
    out.push_str(&format!("  \"source\": {},\n", text(&snapshot.source)));
    out.push_str(&format!("  \"license\": {},\n", text(&snapshot.license)));
    out.push_str("  \"providers\": {");
    for (index, (provider, models)) in snapshot.providers.iter().enumerate() {
        out.push_str(if index == 0 { "\n" } else { ",\n" });
        out.push_str(&format!("    {}: {{", text(provider)));
        for (at, (model, entry)) in models.iter().enumerate() {
            out.push_str(if at == 0 { "\n" } else { ",\n" });
            let entry = serde_json::to_string(entry).unwrap_or_default();
            out.push_str(&format!("      {}: {entry}", text(model)));
        }
        out.push_str(if models.is_empty() { "}" } else { "\n    }" });
    }
    out.push_str(if snapshot.providers.is_empty() {
        "}\n}\n"
    } else {
        "\n  }\n}\n"
    });
    out
}

/// Turns the models.dev catalog `raw` into the snapshot file crab compiles in or caches.
///
/// This is what `make catalog` runs, through the `catalog` example of this crate.
///
/// # Errors
///
/// Fails when `raw` is not JSON or not shaped as a models.dev catalog.
pub fn catalog_snapshot(raw: &str) -> Result<String, serde_json::Error> {
    Ok(render(&from_models_dev(raw)?))
}

/// Whether `value` turns a switch off: `0`, `false`, `off` or `no` in any case.
fn is_off(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

/// Whether the cached copy at `path` is missing or older than [`REFRESH_AFTER`].
fn stale(path: &std::path::Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_none_or(|age| age > REFRESH_AFTER)
}

/// Downloads the catalog into crab's cache folder when the copy there is missing or more than a day old, for the processes started after this one.
///
/// Nothing happens when `OMC_MODELS_REFRESH` is off or there is no cache
/// folder. The catalog is read from `OMC_MODELS_URL` when set, else from
/// models.dev. Every failure is quiet: the copy in use, or the bundled
/// snapshot, stays.
pub async fn refresh() {
    if std::env::var("OMC_MODELS_REFRESH").is_ok_and(|value| is_off(&value)) {
        return;
    }
    let Some(path) = cache_path().filter(|path| stale(path)) else {
        return;
    };
    let url = std::env::var("OMC_MODELS_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
        .unwrap_or_else(|| SOURCE_URL.to_owned());
    let Ok(raw) = download(&url).await else {
        return;
    };
    let Ok(snapshot) = from_models_dev(&raw) else {
        return;
    };
    if snapshot.providers.is_empty() {
        return;
    }
    let _ = write_atomically(&path, render(&snapshot).as_bytes());
}

/// The body of a successful GET of `url`.
///
/// # Errors
///
/// Fails when the request fails or the answer is not a success.
async fn download(url: &str) -> Result<String, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(DOWNLOAD_TIMEOUT)
        .build()?
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await
}

/// Writes `bytes` to `path` through a file beside it, so that a reader never sees half of it.
///
/// # Errors
///
/// Fails when the folder or file cannot be written.
fn write_atomically(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    let staging = path.with_extension("json.part");
    std::fs::write(&staging, bytes)?;
    std::fs::rename(staging, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A models.dev catalog with one Anthropic model, one image model and a provider crab does not know.
    const RAW: &str = r#"{
        "anthropic": { "id": "anthropic", "models": {
            "claude-x-9": {
                "id": "claude-x-9", "reasoning": true,
                "reasoning_options": [{ "type": "toggle" }, { "type": "effort", "values": ["low", "high", "max"] }],
                "modalities": { "input": ["text", "image"], "output": ["text"] },
                "limit": { "context": 500000, "input": 400000, "output": 64000 },
                "cost": { "input": 1, "output": 5, "cache_read": 0.1, "cache_write": 1.25,
                          "tiers": [{ "input": 2, "tier": { "type": "context", "size": 200000 } }] }
            },
            "old-y-1": {
                "id": "old-y-1", "status": "deprecated", "tool_call": true,
                "modalities": { "input": ["text"], "output": ["text"] },
                "limit": { "context": 1000, "output": 100 }
            },
            "painter": {
                "id": "painter", "modalities": { "input": ["text"], "output": ["image"] },
                "limit": { "context": 1000, "output": 0 }
            }
        } },
        "elsewhere": { "id": "elsewhere", "models": { "m": { "limit": { "context": 1, "output": 1 } } } }
    }"#;

    /// Only known providers and text models are kept, with their limits, efforts, input kinds and base prices.
    #[test]
    fn converts_a_models_dev_catalog() {
        let snapshot = from_models_dev(RAW).unwrap();
        assert_eq!(snapshot.providers.keys().collect::<Vec<_>>(), ["anthropic"]);
        let models = &snapshot.providers["anthropic"];
        assert_eq!(models.keys().collect::<Vec<_>>(), ["claude-x-9", "old-y-1"]);
        assert!(models["old-y-1"].deprecated);
        assert_eq!(models["old-y-1"].tools, Some(true));
        assert_eq!(models["claude-x-9"].tools, None);
        let entry = &models["claude-x-9"];
        assert_eq!((entry.context, entry.output), (500_000, 64_000));
        assert_eq!(entry.window(), 400_000);
        assert_eq!(entry.levels(), [Effort::Low, Effort::High, Effort::Max]);
        assert_eq!(entry.input, ["text", "image"]);
        let cost = entry.cost.unwrap();
        assert_eq!((cost.input, cost.cache_write), (1.0, Some(1.25)));
        assert!(snapshot.license.contains("models.dev"));
    }

    /// A rendered snapshot reads back as it was, one model a line.
    #[test]
    fn renders_one_model_a_line() {
        let snapshot = from_models_dev(RAW).unwrap();
        let text = render(&snapshot);
        let again: Snapshot = serde_json::from_str(&text).unwrap();
        assert_eq!(again.providers, snapshot.providers);
        assert!(text.lines().any(|line| {
            line.trim_start()
                .starts_with("\"claude-x-9\": {\"context\"")
        }));
        let empty = render(&Snapshot::default());
        assert!(serde_json::from_str::<Snapshot>(&empty).is_ok());
    }

    /// Hosts' forms of a model id lead to the id the catalog lists.
    #[test]
    fn model_ids_are_tried_in_their_other_forms() {
        assert!(candidates("claude-opus-5-5@default").contains(&"claude-opus-5-5".to_owned()));
        assert!(candidates("claude-opus-5-5").contains(&"claude-opus-5-5@default".to_owned()));
        assert!(
            candidates("anthropic.claude-sonnet-4-5-20250929-v1:0")
                .contains(&"anthropic.claude-sonnet-4-5".to_owned())
        );
        assert_eq!(
            claude_id("us.anthropic.claude-sonnet-4-5-20250929-v1:0"),
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(claude_id("claude-sonnet-4-5@20250929"), "claude-sonnet-4-5");
        assert_eq!(
            claude_id("global.anthropic.claude-opus-4-6-v1"),
            "claude-opus-4-6"
        );
        assert_eq!(claude_id("claude-haiku-5-5"), "claude-haiku-5-5");
        assert_eq!(strip_bedrock_version("gpt-5-very"), "gpt-5-very");
    }

    /// The bundled snapshot parses and describes the providers crab maps to it.
    #[test]
    fn the_bundled_snapshot_describes_known_models() {
        let bundled: Snapshot = serde_json::from_str(BUNDLED).unwrap();
        for source in SOURCES {
            assert!(
                bundled.providers.contains_key(*source),
                "{source} is missing from the snapshot"
            );
        }
        for provider in Provider::ALL {
            for source in sources_of(provider.def().id) {
                assert!(
                    SOURCES.contains(source),
                    "{source} is not a snapshot source"
                );
            }
        }
        let opus = find_in(&bundled, "anthropic", "claude-opus-5-5").unwrap();
        assert!(opus.context >= 200_000 && opus.output > 0);
        assert!(find_in(&bundled, "google-vertex-anthropic", "claude-opus-5-5").is_some());
        assert!(
            find_in(
                &bundled,
                "amazon-bedrock",
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0"
            )
            .is_some()
        );
    }

    /// Codex and local servers have no catalog, and the switch words turn refreshing off.
    #[test]
    fn sources_and_switches() {
        assert!(sources_of("codex").is_empty());
        assert!(sources_of("ollama").is_empty());
        assert_eq!(sources_of("bedrock"), ["amazon-bedrock"]);
        for off in ["0", "false", "OFF", " no "] {
            assert!(is_off(off), "{off}");
        }
        assert!(!is_off("1"));
    }
}
