//! Kagi's Search API.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json, text};

/// Kagi's Search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "kagi",
    aliases: &[],
    variables: &["KAGI_API_KEY"],
    base: "https://kagi.com/api/v1/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query`, `limit` and, except for a year, which
/// Kagi has no filter for, a lens limiting results to the recency.
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
    let mut body = json!({ "query": query, "limit": limit });
    if let Some(recency) = recency.filter(|recency| *recency != Recency::Year) {
        body["lens"] = json!({ "time_relative": recency.word() });
    }
    Ok(Call::post_json(url, vec![backend.bearer()], &body))
}

/// Reads `data.search` of the answer `body`, or the error Kagi refused the
/// search with.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON or holds an error.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    if let Some(error) = value["error"].as_array().and_then(|errors| errors.first()) {
        return Err(format!(
            "Kagi refused the search: {}",
            text(&error["message"])
                .or_else(|| text(&error["code"]))
                .unwrap_or_default()
        ));
    }
    Ok(Found {
        answer: None,
        hits: hits(&value["data"]["search"], &["snippet"], &["time"]),
    })
}
