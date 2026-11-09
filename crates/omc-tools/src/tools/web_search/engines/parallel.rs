//! Parallel's Search API, or without a key its public MCP server.

use serde_json::{Value, json};

use super::super::backend::{
    Backend, Call, Found, Hit, Recency, Spec, hit, json, list, plain, text,
};
use super::super::mcp;

/// Parallel's Search API, and its public MCP server.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "parallel",
    aliases: &[],
    variables: &["PARALLEL_API_KEY"],
    base: "https://api.parallel.ai/v1beta/search",
    open_base: Some("https://search.parallel.ai/mcp"),
    call,
    parse,
};

/// With a key, a JSON POST to `url` of a fast search for `query`, with short
/// excerpts, at most 20 results and a source policy starting at the recency;
/// without one, a call of the MCP server's `web_search` tool, the recency an
/// `after:` date in the query.
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
    if backend.key.is_empty() {
        let query = match recency {
            Some(recency) => format!("{query} after:{}", recency.since()),
            None => query.to_owned(),
        };
        return Ok(mcp::call(
            url,
            "web_search",
            json!({ "objective": query, "search_queries": [query] }),
        ));
    }
    let mut body = json!({
        "objective": query,
        "search_queries": [query],
        "mode": "fast",
        "max_results": limit.min(20),
        "excerpts": { "max_chars_per_result": 1500 },
    });
    if let Some(recency) = recency {
        body["source_policy"] = json!({ "after_date": recency.since() });
    }
    Ok(Call::post_json(
        url,
        vec![
            ("x-api-key", backend.key.clone()),
            ("parallel-beta", "search-extract-2025-10-10".to_owned()),
        ],
        &body,
    ))
}

/// Reads the `results` of the answer `body`, sent by the API as it is, or by
/// the MCP tool as JSON in its text.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON, the MCP server or tool
/// reports an error, or the tool's text holds no results.
fn parse(backend: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = if backend.key.is_empty() {
        mcp::texts(body)?
            .iter()
            .filter_map(|part| serde_json::from_str::<Value>(part).ok())
            .find(|value| value["results"].is_array())
            .ok_or("Parallel's answer holds no search results")?
    } else {
        json(body)?
    };
    Ok(Found {
        answer: None,
        hits: list(&value["results"]).iter().map(result).collect(),
    })
}

/// The result `item`, with its excerpts joined as the snippet.
fn result(item: &Value) -> Hit {
    let excerpts: Vec<String> = list(&item["excerpts"]).iter().filter_map(text).collect();
    let mut found = hit(item, "title", "url", &[], &["publish_date"]);
    found.snippet = plain(&excerpts.join(" … "));
    found
}
