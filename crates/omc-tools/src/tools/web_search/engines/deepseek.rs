//! DeepSeek's search API.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// DeepSeek's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "deepseek",
    aliases: &[],
    variables: &["DEEPSEEK_API_KEY"],
    base: "https://api.deepseek.com/v1/tools/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query`, the recency an `after:` date in it,
/// and at most 50 results.
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
    let query = match recency {
        Some(recency) => format!("{query} after:{}", recency.since()),
        None => query.to_owned(),
    };
    let body = json!({ "query": query, "count": limit.min(50) });
    Ok(Call::post_json(url, vec![backend.bearer()], &body))
}

/// Reads the `results` of the answer `body`, the summary as the snippet when
/// there is one.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: None,
        hits: hits(&value["results"], &["summary", "snippet"], &["date"]),
    })
}
