//! Keenable's search API, or without a key its public endpoint, which asks
//! callers to name themselves.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// Keenable's search API, and its public endpoint.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "keenable",
    aliases: &[],
    variables: &["KEENABLE_API_KEY"],
    base: "https://api.keenable.ai/v1/search",
    open_base: Some("https://api.keenable.ai/v1/search/public"),
    call,
    parse,
};

/// A JSON POST to `url` with `query`, at most 50 results and the recency as
/// the window results are published in, signed with the key or, without one,
/// naming crab.
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
    let mut body = json!({ "query": query, "max_results": limit.min(50) });
    if let Some(recency) = recency {
        let window = match recency {
            Recency::Day => "1d",
            Recency::Week => "7d",
            Recency::Month => "1mo",
            Recency::Year => "1y",
        };
        body["published_after"] = json!(window);
    }
    let header = if backend.key.is_empty() {
        ("X-Keenable-Title", "crab".to_owned())
    } else {
        ("X-API-Key", backend.key.clone())
    };
    Ok(Call::post_json(url, vec![header], &body))
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
        hits: hits(
            &value["results"],
            &["description", "snippet"],
            &["published_at", "date"],
        ),
    })
}
