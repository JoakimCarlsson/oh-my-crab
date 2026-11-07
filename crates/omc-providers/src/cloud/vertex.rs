//! Claude on Google Vertex AI through `streamRawPredict`.
//!
//! Vertex takes the Anthropic Messages body with `anthropic_version` in place
//! of the model, which goes in the address, and streams the same server-sent
//! events Anthropic's own API does.

use serde_json::{Value, json};

use crate::Provider;

/// The Messages API version Vertex is asked for.
const VERSION: &str = "vertex-2023-10-16";

/// Makes `body`, built for Anthropic's own API, into a Vertex body: no `model`, and the API version.
pub(crate) fn adapt_body(body: &mut Value) {
    if let Some(object) = body.as_object_mut() {
        object.remove("model");
        object.insert("anthropic_version".to_owned(), json!(VERSION));
    }
}

/// The address of Vertex AI in `region`: the provider's address with the region put in, without one for the global endpoint, or the override its address variable sets.
pub(crate) fn base_url(region: &str) -> String {
    let base = Provider::Vertex.def().base_url();
    if region == "global" {
        base.replace("{region}-", "")
    } else {
        base.replace("{region}", region)
    }
}

/// The address `model` is asked at under `base`, for `project` in `region`, streamed or not.
pub(crate) fn predict_url(
    base: &str,
    project: &str,
    region: &str,
    model: &str,
    stream: bool,
) -> String {
    let action = if stream {
        "streamRawPredict"
    } else {
        "rawPredict"
    };
    format!(
        "{}/v1/projects/{project}/locations/{region}/publishers/anthropic/models/{model}:{action}",
        base.trim_end_matches('/')
    )
}

/// The Claude models Vertex serves, as the catalog lists them, without the `@default` version tag.
pub(crate) fn models() -> Vec<String> {
    crate::catalog::listed("google-vertex-anthropic")
        .into_iter()
        .map(|id| {
            id.strip_suffix("@default")
                .map_or(id.clone(), str::to_owned)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The body loses its model and gains the version, keeping the stream flag.
    #[test]
    fn adapts_the_messages_body() {
        let mut body = json!({ "model": "m", "stream": true, "messages": [] });
        adapt_body(&mut body);
        assert!(body.get("model").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(body["anthropic_version"], VERSION);
    }

    /// Regional and global endpoints and the predict address take the project, region and model.
    #[test]
    fn builds_addresses() {
        if std::env::var_os("OMC_VERTEX_URL").is_none() {
            assert_eq!(base_url("global"), "https://aiplatform.googleapis.com");
            assert_eq!(
                base_url("us-east5"),
                "https://us-east5-aiplatform.googleapis.com"
            );
        }
        assert_eq!(
            predict_url(
                "https://aiplatform.googleapis.com",
                "p-1",
                "global",
                "claude-opus-5-5",
                true
            ),
            "https://aiplatform.googleapis.com/v1/projects/p-1/locations/global/publishers/anthropic/models/claude-opus-5-5:streamRawPredict"
        );
        assert!(
            predict_url("http://x/", "p", "r", "m@1", false).ends_with("/models/m@1:rawPredict")
        );
    }

    /// The listed models are Claude, without the default tag.
    #[test]
    fn lists_claude_models() {
        let listed = models();
        assert!(listed.contains(&"claude-opus-5-5".to_owned()));
        assert!(
            listed
                .iter()
                .all(|id| id.starts_with("claude-") && !id.ends_with("@default"))
        );
    }
}
