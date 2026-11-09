//! Jina's search API.

use super::super::backend::{Backend, Call, Found, Recency, Spec, hits, json, text};

/// Jina's search API.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "jina",
    aliases: &[],
    variables: &["JINA_API_KEY"],
    base: "https://s.jina.ai/",
    open_base: None,
    call,
    parse,
};

/// A GET of `url` with `query` as its path and at most 20 results, asking
/// for JSON without the pages' content or images; Jina has no time filter.
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
    url.path_segments_mut()
        .map_err(|()| "Jina's address cannot take a path".to_owned())?
        .pop_if_empty()
        .push(query);
    url.query_pairs_mut()
        .append_pair("count", &limit.min(20).to_string());
    Ok(Call::get(
        url,
        vec![
            backend.bearer(),
            ("Accept", "application/json".to_owned()),
            ("X-Respond-With", "no-content".to_owned()),
            ("X-Retain-Images", "none".to_owned()),
        ],
    ))
}

/// Reads the `data` of the answer `body`, or the error Jina answered with.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON or its `code` is not 200.
fn parse(_: &Backend, body: &[u8]) -> Result<Found, String> {
    let value = json(body)?;
    if let Some(code) = value["code"].as_u64().filter(|code| *code != 200) {
        return Err(format!(
            "Jina answered {code}: {}",
            text(&value["readableMessage"])
                .or_else(|| text(&value["message"]))
                .unwrap_or_default()
        ));
    }
    Ok(Found {
        answer: None,
        hits: hits(&value["data"], &["description", "content"], &["date"]),
    })
}
