//! TinyFish's search API.

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// TinyFish's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "tinyfish",
    aliases: &[],
    variables: &["TINYFISH_API_KEY"],
    base: "https://api.search.tinyfish.ai/",
    open_base: None,
    call,
    parse,
};

/// A GET of `url` with `query` and the first page of at most 10 results;
/// TinyFish has no time filter.
///
/// # Errors
///
/// Never fails.
fn call(
    backend: &Backend,
    mut url: reqwest::Url,
    query: &str,
    limit: usize,
    _: Option<Recency>,
) -> Result<Call, String> {
    url.query_pairs_mut()
        .append_pair("query", query)
        .append_pair("num_results", &limit.min(10).to_string())
        .append_pair("page", "0");
    Ok(Call::get(
        url,
        vec![
            ("X-API-Key", backend.key.clone()),
            ("Accept", "application/json".to_owned()),
        ],
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
        hits: hits(&value["results"], &["snippet"], &[]),
    })
}
