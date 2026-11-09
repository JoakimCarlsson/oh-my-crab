//! Firecrawl's search API, which also answers without a key from addresses
//! that do not look like a VPN or proxy.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json, text};

/// Firecrawl's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "firecrawl",
    aliases: &[],
    variables: &["FIRECRAWL_API_KEY"],
    base: "https://api.firecrawl.dev/v2/search",
    open_base: Some("https://api.firecrawl.dev/v2/search"),
    call,
    parse,
};

/// A JSON POST to `url` searching the web for `query`, at most 100 results,
/// with the recency as a `tbs` time filter, signed with the key when there
/// is one.
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
    let mut body = json!({
        "query": query,
        "limit": limit.min(100),
        "sources": [{ "type": "web" }],
    });
    if let Some(recency) = recency {
        let tbs = match recency {
            Recency::Day => "qdr:d",
            Recency::Week => "qdr:w",
            Recency::Month => "qdr:m",
            Recency::Year => "qdr:y",
        };
        body["tbs"] = json!(tbs);
    }
    let headers = if backend.key.is_empty() {
        Vec::new()
    } else {
        vec![backend.bearer()]
    };
    Ok(Call::post_json(url, headers, &body))
}

/// Reads `data.web` of the answer `body`, or `data` when it is a list, or the
/// error Firecrawl refused the search with.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON or says it failed.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    if value["success"] == json!(false) {
        return Err(format!(
            "Firecrawl refused the search: {}",
            text(&value["error"]).unwrap_or_default()
        ));
    }
    let items = if value["data"].is_array() {
        &value["data"]
    } else {
        &value["data"]["web"]
    };
    Ok(Found {
        answer: None,
        hits: hits(items, &["description", "snippet", "markdown"], &[]),
    })
}
