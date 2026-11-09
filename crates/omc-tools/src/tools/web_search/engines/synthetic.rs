//! Synthetic's search API.

use serde_json::json;

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// Synthetic's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "synthetic",
    aliases: &[],
    variables: &["SYNTHETIC_API_KEY"],
    base: "https://api.synthetic.new/v2/search",
    open_base: None,
    call,
    parse,
};

/// A JSON POST to `url` with `query`; Synthetic takes no count or time
/// filter.
///
/// # Errors
///
/// Never fails.
fn call(
    backend: &Backend,
    url: reqwest::Url,
    query: &str,
    _: usize,
    _: Option<Recency>,
) -> Result<Call, String> {
    Ok(Call::post_json(
        url,
        vec![backend.bearer()],
        &json!({ "query": query }),
    ))
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
        hits: hits(&value["results"], &["text"], &["published"]),
    })
}
