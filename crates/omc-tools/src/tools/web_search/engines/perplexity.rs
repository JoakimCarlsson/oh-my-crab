//! Perplexity's Search API, which returns results without an answer.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// Perplexity's Search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "perplexity",
    aliases: &["pplx"],
    variables: &["PERPLEXITY_API_KEY", "PPLX_API_KEY"],
    base: "https://api.perplexity.ai/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query`, at most 20 results and the recency
/// filter.
///
/// # Errors
///
/// Never fails.
fn call(
    backend: &Backend,
    url: reqwest::Url,
    query: &str,
    limit: usize,
    recency: Option<Recency>,
) -> Result<Call, String> {
    let mut body = json!({ "query": query, "max_results": limit.min(20) });
    if let Some(recency) = recency {
        body["search_recency_filter"] = json!(recency.word());
    }
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
        hits: hits(&value["results"], &["snippet"], &["date", "last_updated"]),
    })
}
