//! Ollama's web search API.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// Ollama's web search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "ollama",
    aliases: &[],
    variables: &["OLLAMA_API_KEY", "OLLAMA_CLOUD_API_KEY"],
    base: "https://ollama.com/api/web_search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query` and at most 10 results; Ollama has no
/// time filter.
///
/// # Errors
///
/// Never fails.
fn call(
    backend: &Backend,
    url: reqwest::Url,
    query: &str,
    limit: usize,
    _: Option<Recency>,
) -> Result<Call, String> {
    let body = json!({ "query": query, "max_results": limit.min(10) });
    Ok(Call::post_json(url, vec![backend.bearer()], &body))
}

/// Reads the `results` of the answer `body`.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: None,
        hits: hits(&value["results"], &["content"], &[]),
    })
}
