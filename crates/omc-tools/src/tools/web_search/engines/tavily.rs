//! Tavily's search API, which also writes a short answer.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json, text};

/// Tavily's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "tavily",
    aliases: &[],
    variables: &["TAVILY_API_KEY"],
    base: "https://api.tavily.com/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` asking for a basic search of `query` with a basic
/// answer, at most 20 results and the time range of the recency.
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
        "max_results": limit.min(20),
        "search_depth": "basic",
        "include_answer": "basic",
    });
    if let Some(recency) = recency {
        body["time_range"] = json!(recency.word());
    }
    Ok(Call::post_json(url, vec![backend.bearer()], &body))
}

/// Reads the `answer` and the `results` of the answer `body`.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: text(&value["answer"]),
        hits: hits(&value["results"], &["content"], &["published_date"]),
    })
}
