//! Reading DuckDuckGo's HTML results page, the search that needs no key.
//!
//! DuckDuckGo offers no search API, so this reads the page its no-script
//! search form shows. It is the last resort: the markup can change, and
//! DuckDuckGo answers automated searches it suspects with a challenge page
//! instead of results, which is reported as a failure.

use super::super::backend::{Backend, Call, Found, Hit, Recency, Spec, plain};
use crate::tools::read::html;

/// DuckDuckGo's no-script results page.
pub(in crate::tools::web_search) const SPEC: Spec = Spec {
    name: "duckduckgo",
    aliases: &["ddg"],
    variables: &[],
    base: "https://html.duckduckgo.com/html/",
    open_base: Some("https://html.duckduckgo.com/html/"),
    call,
    parse: read,
};

/// A form POST to `url` of `query` and, as its first letter, the recency.
///
/// # Errors
///
/// Never fails.
fn call(
    _: &Backend,
    url: reqwest::Url,
    query: &str,
    _: usize,
    recency: Option<Recency>,
) -> Result<Call, String> {
    let mut form =
        reqwest::Url::parse("http://form.invalid/").map_err(|error| error.to_string())?;
    {
        let mut pairs = form.query_pairs_mut();
        pairs.append_pair("q", query);
        if let Some(recency) = recency {
            pairs.append_pair("df", &recency.word()[..1]);
        }
    }
    Ok(Call {
        post: true,
        url,
        headers: vec![("Accept", "text/html".to_owned())],
        body: Some((
            "application/x-www-form-urlencoded",
            form.query().unwrap_or_default().to_owned(),
        )),
    })
}

/// Reads the results page `body` with [`parse`].
///
/// # Errors
///
/// Returns the reason [`parse`] gives.
fn read(_: &Backend, body: &[u8]) -> Result<Found, String> {
    parse(&String::from_utf8_lossy(body))
}

/// Reads the results out of the DuckDuckGo results page `page`, leaving out
/// ads and unwrapping DuckDuckGo's redirect links.
///
/// # Errors
///
/// Returns the reason when the page is a challenge rather than results, or is
/// not a results page at all.
fn parse(page: &str) -> Result<Found, String> {
    let tree = html::parse(page).ok_or("the results page did not parse")?;
    let challenged = html::find(&tree, &|node| {
        html::attr(node, "id").as_deref() == Some("challenge-form")
            || html::has_class(node, "anomaly-modal__modal")
            || html::has_class(node, "anomaly-modal")
    })
    .is_some();
    if challenged {
        return Err(
            "DuckDuckGo answered with a bot challenge instead of results; set a search API key \
             for a dependable service"
                .to_owned(),
        );
    }
    let results = html::find_all(&tree, &|node| {
        html::tag(node) == Some("div") && html::has_class(node, "result")
    });
    let page_has_results = html::find(&tree, &|node| {
        html::attr(node, "id").as_deref() == Some("links") || html::has_class(node, "no-results")
    })
    .is_some();
    if results.is_empty() && !page_has_results {
        return Err("the answer is not a DuckDuckGo results page".to_owned());
    }
    let hits = results
        .iter()
        .filter(|result| !html::has_class(result, "result--ad"))
        .filter_map(|result| {
            let link = html::find(result, &|node| html::has_class(node, "result__a"))?;
            let url = unwrap_link(&html::attr(&link, "href")?)?;
            let snippet = html::find(result, &|node| html::has_class(node, "result__snippet"))
                .map(|node| plain(&html::text_of(&node)))
                .unwrap_or_default();
            Some(Hit {
                title: html::collapse(&html::text_of(&link)),
                url,
                snippet,
                date: None,
            })
        })
        .collect();
    Ok(Found { answer: None, hits })
}

/// The address a DuckDuckGo result link `href` leads to: the `uddg` target of
/// a `/l/` redirect link, or the link itself when it is a plain address;
/// nothing for links back into DuckDuckGo, such as ads.
fn unwrap_link(href: &str) -> Option<String> {
    let absolute = if href.starts_with("//") {
        format!("https:{href}")
    } else {
        href.to_owned()
    };
    let url = reqwest::Url::parse(&absolute).ok()?;
    let host = url.host_str().unwrap_or_default();
    if host == "duckduckgo.com" || host.ends_with(".duckduckgo.com") {
        let target = url
            .query_pairs()
            .find(|(key, _)| key == "uddg")
            .map(|(_, value)| value.into_owned())?;
        return reqwest::Url::parse(&target).ok().map(String::from);
    }
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded page's results come out in order, without the ad, with
    /// redirect links unwrapped and snippets as plain text.
    #[test]
    fn reads_results() {
        let found = parse(include_str!("../fixtures/duckduckgo.html")).unwrap();
        let urls: Vec<&str> = found.hits.iter().map(|hit| hit.url.as_str()).collect();
        assert_eq!(
            urls,
            [
                "https://docs.rs/reqwest/latest/reqwest/struct.ClientBuilder.html",
                "https://stackoverflow.com/questions/76196208/rust-request-with-timeout",
                "https://docs.rs/reqwest/latest/reqwest/struct.RequestBuilder.html",
                "https://example.org/a?b=1",
            ]
        );
        assert_eq!(
            found.hits[0].title,
            "ClientBuilder in reqwest - Rust - Docs.rs"
        );
        assert!(
            found.hits[0]
                .snippet
                .starts_with("Source pub fn timeout (self, timeout: Duration) -> ClientBuilder"),
            "{}",
            found.hits[0].snippet
        );
        assert_eq!(found.hits[3].title, "Wrapped & redirected");
        assert_eq!(found.hits[3].snippet, "A wrapped link.");
    }

    /// A challenge page fails, and so does a page that is not DuckDuckGo's.
    #[test]
    fn refuses_challenges() {
        let challenge = "<html><body><div class=\"anomaly-modal__modal\">Select all ducks</div>\
            <form id=\"challenge-form\"></form></body></html>";
        assert!(parse(challenge).unwrap_err().contains("bot challenge"));
        assert!(parse("<html><body>hello</body></html>").is_err());
        let empty = "<html><body><div id=\"links\"><div class=\"no-results\">No results.</div>\
            </div></body></html>";
        assert!(parse(empty).unwrap().hits.is_empty());
    }
}
