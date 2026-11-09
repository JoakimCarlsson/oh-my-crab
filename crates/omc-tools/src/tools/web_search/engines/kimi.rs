//! Kimi Code's search API, signed with a Kimi Code key rather than a Moonshot
//! platform key.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// Kimi Code's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "kimi",
    aliases: &[],
    variables: &["MOONSHOT_SEARCH_API_KEY", "KIMI_SEARCH_API_KEY"],
    base: "https://api.kimi.com/coding/v1/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query` and at most 20 results, without
/// crawling the pages; Kimi has no time filter.
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
    let body = json!({
        "text_query": query,
        "limit": limit.min(20),
        "enable_page_crawling": false,
        "timeout_seconds": 30,
    });
    Ok(Call::post_json(url, vec![backend.bearer()], &body))
}

/// Reads the `search_results` of the answer `body`.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: None,
        hits: hits(&value["search_results"], &["snippet", "content"], &["date"]),
    })
}
