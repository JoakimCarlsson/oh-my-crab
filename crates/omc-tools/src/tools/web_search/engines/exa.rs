//! Exa's search API, or without a key its public MCP server.

use serde_json::json;

use super::super::backend::{
    Backend, Call, Found, Hit, Recency, Spec, hit, json, list, plain, text,
};
use super::super::mcp;

/// Exa's search API, and its public MCP server.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "exa",
    aliases: &[],
    variables: &["EXA_API_KEY"],
    base: "https://api.exa.ai/search",
    open_base: Some("https://mcp.exa.ai/mcp"),
    call,
    parse,
};

/// With a key, a JSON POST to `url` asking for highlights of at most 100
/// results published since the recency began; without one, a call of the MCP
/// server's `web_search_exa` tool, the recency an `after:` date in the query.
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
    if backend.key.is_empty() {
        let query = match recency {
            Some(recency) => format!("{query} after:{}", recency.since()),
            None => query.to_owned(),
        };
        url.query_pairs_mut().append_pair("tools", "web_search_exa");
        return Ok(mcp::call(
            url,
            "web_search_exa",
            json!({ "query": query, "numResults": limit.min(20) }),
        ));
    }
    let mut body = json!({
        "query": query,
        "numResults": limit.min(100),
        "type": "auto",
        "contents": { "highlights": { "maxCharacters": 600 } },
    });
    if let Some(recency) = recency {
        body["startPublishedDate"] = json!(format!("{}T00:00:00.000Z", recency.since()));
    }
    Ok(Call::post_json(
        url,
        vec![("x-api-key", backend.key.clone())],
        &body,
    ))
}

/// Reads the answer `body`: the API's `results`, with their highlights as the
/// snippet, else their summary, else their text; or the MCP tool's text.
///
/// # Errors
///
/// Returns the reason when the answer is not JSON, or the MCP server or tool
/// reports an error.
fn parse(backend: &Backend, body: &[u8]) -> Result<Found, String> {
    if backend.key.is_empty() {
        return Ok(Found {
            answer: None,
            hits: listed(&mcp::texts(body)?.join("\n\n")),
        });
    }
    let value = json(body)?;
    Ok(Found {
        answer: None,
        hits: list(&value["results"])
            .iter()
            .map(|item| {
                let mut found = hit(item, "title", "url", &["summary"], &["publishedDate"]);
                let highlights: Vec<String> =
                    list(&item["highlights"]).iter().filter_map(text).collect();
                if !highlights.is_empty() {
                    found.snippet = plain(&highlights.join(" … "));
                } else if found.snippet.is_empty() {
                    found.snippet = plain(item["text"].as_str().unwrap_or_default());
                }
                found
            })
            .collect(),
    })
}

/// The results the MCP tool listed in `all`, each starting with a `Title:`
/// line, followed by `URL:`, `Published:` and `Author:` lines, then its
/// highlights or text.
fn listed(all: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    let mut current: Option<(Hit, Vec<String>)> = None;
    for line in all.replace("\r\n", "\n").lines() {
        if let Some(title) = line.strip_prefix("Title:") {
            hits.extend(current.take().map(finish));
            let hit = Hit {
                title: title.trim().to_owned(),
                url: String::new(),
                snippet: String::new(),
                date: None,
            };
            current = Some((hit, Vec::new()));
            continue;
        }
        let Some((hit, words)) = current.as_mut() else {
            continue;
        };
        if hit.url.is_empty()
            && let Some(url) = line.strip_prefix("URL:")
        {
            hit.url = url.trim().to_owned();
        } else if let Some(date) = line
            .strip_prefix("Published Date:")
            .or_else(|| line.strip_prefix("Published:"))
        {
            let date = date.trim();
            hit.date = (!date.is_empty() && date != "N/A").then(|| date.to_owned());
        } else if !line.starts_with("Author:") {
            let line = line
                .strip_prefix("Highlights:")
                .or_else(|| line.strip_prefix("Text:"))
                .unwrap_or(line);
            words.push(line.to_owned());
        }
    }
    hits.extend(current.map(finish));
    hits
}

/// The result `hit` with the lines `words` that followed its fields as its
/// snippet, leaving out a repeat of its title and Markdown heading marks.
fn finish((mut hit, words): (Hit, Vec<String>)) -> Hit {
    let words: Vec<&str> = words
        .iter()
        .map(|line| line.trim_start_matches('#').trim())
        .filter(|line| !line.is_empty() && *line != hit.title)
        .collect();
    hit.snippet = plain(&words.join(" "));
    hit
}
