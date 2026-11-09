//! A SearXNG instance the user runs, with JSON output turned on.

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json, list, text};

/// A SearXNG instance, at the address its variable holds.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "searxng",
    aliases: &["searx"],
    variables: &["SEARXNG_URL", "OMC_SEARXNG_URL"],
    base: "",
    open_base: None,
    call,
    parse,
};

/// A GET of `url` asking for JSON results for `query`; SearXNG has no week,
/// so a week widens to a month.
///
/// # Errors
///
/// Never fails.
fn call(
    _: &Backend,
    mut url: reqwest::Url,
    query: &str,
    _: usize,
    recency: Option<Recency>,
) -> Result<Call, String> {
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("q", query);
        pairs.append_pair("format", "json");
        if let Some(recency) = recency {
            let range = match recency {
                Recency::Day => "day",
                Recency::Week | Recency::Month => "month",
                Recency::Year => "year",
            };
            pairs.append_pair("time_range", range);
        }
    }
    Ok(Call::get(
        url,
        vec![("Accept", "application/json".to_owned())],
    ))
}

/// Reads the `answers`, joined, and the `results` of the answer `body`.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    Ok(Found {
        answer: list(&value["answers"])
            .iter()
            .filter_map(|answer| text(answer).or_else(|| text(&answer["answer"])))
            .reduce(|all, next| format!("{all}\n{next}")),
        hits: hits(&value["results"], &["content"], &["publishedDate"]),
    })
}
