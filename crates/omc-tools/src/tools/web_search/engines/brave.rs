//! The Brave Search API.

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json};

/// The Brave Search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "brave",
    aliases: &[],
    variables: &["BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"],
    base: "https://api.search.brave.com/res/v1/web/search",
    open_base: None,
    call,
    parse,
};

/// A GET of `url` with the query, at most 20 results and the freshness the
/// recency asks for, signed with the key.
///
/// # Errors
///
/// Never fails.
fn call(
    backend: &Backend,
    mut url: reqwest::Url,
    query: &str,
    limit: usize,
    recency: Option<Recency>,
) -> Result<Call, String> {
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        pairs.append_pair("count", &limit.min(20).to_string());
        if let Some(recency) = recency {
            let freshness = match recency {
                Recency::Day => "pd",
                Recency::Week => "pw",
                Recency::Month => "pm",
                Recency::Year => "py",
            };
            pairs.append_pair("freshness", freshness);
        }
    }
    Ok(Call::get(
        url,
        vec![
            ("X-Subscription-Token", backend.key.clone()),
            ("Accept", "application/json".to_owned()),
        ],
    ))
}

/// Reads the `web.results` of the answer `body`.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: None,
        hits: hits(
            &value["web"]["results"],
            &["description"],
            &["page_age", "age"],
        ),
    })
}
