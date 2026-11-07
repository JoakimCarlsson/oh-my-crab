//! Reading `http://` and `https://` URLs: fetching, then turning HTML into
//! Markdown, PDFs into text, JSON into indented JSON and images into images.

use std::time::Duration;

use super::media::{self, Media};
use super::selector::Selector;
use super::{guard, html, lines};
use crate::{Spill, ToolError, ToolOutput};

/// Most bytes of a response body read; the rest is dropped.
const MAX_BODY: usize = 10 << 20;

/// How long a fetch may take, connecting and reading together.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Most same-origin redirects a fetch follows.
const MAX_REDIRECTS: usize = 10;

/// The `User-Agent` sent with every fetch.
const USER_AGENT: &str = concat!("crab/", env!("CARGO_PKG_VERSION"));

/// Whether `path` is an `http://` or `https://` URL rather than a file path.
pub(super) fn is_url(path: &str) -> bool {
    let lower = path.get(..8).unwrap_or(path).to_ascii_lowercase();
    lower.starts_with("http://") || lower.starts_with("https://")
}

/// The URL a `read` of `raw` fetches, parsed once, and the selector after it.
///
/// Approval, the prompt and the fetch all use this one parsed URL, so what the
/// user approves is exactly what is reached.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the address does not parse, is
/// not `http` or `https`, or has no host written right after `://`, as in
/// `http:///host`, which parsers disagree on.
pub(super) fn target(raw: &str) -> Result<(reqwest::Url, Selector), ToolError> {
    let (address, selector) = super::selector::split_url(raw);
    let invalid = |why: &str| ToolError::InvalidArguments(format!("{address}: {why}"));
    let authority = address
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_default();
    if authority.is_empty() || authority.starts_with(['/', '\\']) {
        return Err(invalid("the URL has no host"));
    }
    let url = reqwest::Url::parse(address).map_err(|error| invalid(&error.to_string()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none_or(str::is_empty) {
        return Err(invalid("only http and https URLs with a host can be read"));
    }
    Ok((url, selector))
}

/// The approval scope of reading `raw`: the origin of the URL it fetches, such
/// as `https://example.com:8443`, or all of `raw` when it is not a URL that
/// can be read, which a call then fails on.
pub(super) fn scope(raw: &str) -> String {
    match target(raw) {
        Ok((url, _)) => url.origin().ascii_serialization(),
        Err(_) => raw.to_owned(),
    }
}

/// The host a `read` of `raw` reaches, lowercased, for permission rules about sites; all of `raw` when it is not a URL that can be read, which matches no site.
pub(super) fn host(raw: &str) -> String {
    match target(raw) {
        Ok((url, _)) => url.host_str().unwrap_or_default().to_ascii_lowercase(),
        Err(_) => raw.to_owned(),
    }
}

/// A fetched response, as far as reading it needs.
pub(super) struct Page {
    /// The URL the response came from, after redirects.
    pub(super) url: String,
    /// The URL that was asked for, when redirects led away from it.
    pub(super) redirected_from: Option<String>,
    /// The `Content-Type` header, or empty when there was none.
    pub(super) content_type: String,
    /// The body, cut at [`MAX_BODY`].
    pub(super) body: Vec<u8>,
    /// Whether the body was cut.
    pub(super) truncated: bool,
}

/// Fetches `url` and reads it as `selector` asks, saving long text in `spill`.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the request fails, the server answers
/// with an error status, or the body is binary data `read` cannot show.
pub(super) async fn read(
    url: &str,
    selector: Selector,
    spill: Spill,
) -> Result<ToolOutput, ToolError> {
    let page = fetch(url).await?;
    tokio::task::spawn_blocking(move || render(&page, &selector, &spill))
        .await
        .map_err(|e| ToolError::Failed(e.to_string()))?
}

/// `error` followed by each error that caused it, so that the reason a fetch
/// failed, such as an address refused by [`guard::PublicOnly`], is not hidden
/// behind a general "error sending request".
fn with_causes(error: &dyn std::error::Error) -> String {
    let mut text = error.to_string();
    let mut cause = error.source();
    while let Some(inner) = cause {
        let said = inner.to_string();
        if !text.contains(&said) {
            text.push_str(": ");
            text.push_str(&said);
        }
        cause = inner.source();
    }
    text
}

/// The redirect policy of a fetch: hops that stay on the same origin are
/// followed, up to [`MAX_REDIRECTS`], and any other hop stops the fetch.
///
/// The user approves reading one URL, so a hop to another scheme, host or
/// port is not followed silently; [`fetch`] reports it instead, and reading
/// the new URL asks for approval again.
fn redirect_policy() -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(|attempt| {
        let same_origin = attempt
            .previous()
            .last()
            .is_some_and(|previous| previous.origin() == attempt.url().origin());
        if !same_origin {
            attempt.stop()
        } else if attempt.previous().len() > MAX_REDIRECTS {
            attempt.error("too many redirects")
        } else {
            attempt.follow()
        }
    })
}

/// The error for a response that redirects to another origin, naming where it
/// points so that the model can read that URL, with fresh approval, instead.
fn redirected(url: &str, response: &reqwest::Response) -> ToolError {
    let target = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|location| response.url().join(location).ok());
    match target {
        Some(target) => ToolError::Failed(format!(
            "{url} redirects to {target}, on another origin; read that URL to follow it"
        )),
        None => ToolError::Failed(format!(
            "{url} answered {} without a usable Location",
            response.status()
        )),
    }
}

/// Fetches `url`, following same-origin redirects, keeping at most
/// [`MAX_BODY`] bytes.
///
/// The fetch connects directly and ignores any proxy set in the environment:
/// through a proxy the proxy would resolve host names, and
/// [`guard::PublicOnly`] could not keep them from local addresses.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the request fails, the server redirects
/// to another origin, or it answers with a status other than success.
async fn fetch(url: &str) -> Result<Page, ToolError> {
    let failed = |error: reqwest::Error| {
        ToolError::Failed(format!("fetching {url} failed: {}", with_causes(&error)))
    };
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(TIMEOUT)
        .redirect(redirect_policy())
        .dns_resolver(guard::PublicOnly)
        .no_proxy()
        .build()
        .map_err(failed)?;
    let mut response = client
        .get(url)
        .header(
            reqwest::header::ACCEPT,
            "text/html,application/xhtml+xml,text/markdown,text/plain;q=0.9,*/*;q=0.8",
        )
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if status.is_redirection() {
        return Err(redirected(url, &response));
    }
    if !status.is_success() {
        return Err(ToolError::Failed(format!("{url} answered {status}")));
    }
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let final_url = response.url().to_string();
    let mut body = Vec::new();
    let mut truncated = false;
    while let Some(chunk) = response.chunk().await.map_err(failed)? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            body.truncate(MAX_BODY);
            truncated = true;
            break;
        }
    }
    let redirected_from = (final_url != url).then(|| url.to_owned());
    Ok(Page {
        url: final_url,
        redirected_from,
        content_type,
        body,
        truncated,
    })
}

/// Turns a fetched `page` into what the model reads, as `selector` asks,
/// saving text too long to show whole in `spill`.
///
/// Images are handed over as images; PDFs give their text; HTML gives the
/// Markdown of its main content, of the whole page when the selector says
/// `full`, or its source when it says `raw`; JSON is pretty-printed unless the
/// selector says `raw`; other text is shown as it is. Text comes back with
/// numbered lines under a header naming the URL.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when the body is binary data of another kind,
/// a cut-off image, or a PDF without text, and
/// [`ToolError::InvalidArguments`] when lines are picked from an image.
pub(super) fn render(
    page: &Page,
    selector: &Selector,
    spill: &Spill,
) -> Result<ToolOutput, ToolError> {
    let mime = page
        .content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let label = page.url.as_str();
    let mut notes: Vec<String> = page
        .redirected_from
        .iter()
        .map(|from| format!("redirected from {from}"))
        .collect();
    notes.push(if mime.is_empty() {
        "no content type".to_string()
    } else {
        mime.clone()
    });
    if page.truncated {
        notes.push(format!("cut at {}", media::size(MAX_BODY)));
    }
    let text = match media::sniff(&page.body) {
        Some(Media::Image(media_type)) => {
            if selector.picks_lines() {
                return Err(ToolError::InvalidArguments(format!(
                    "{label} is an image; line selectors apply to text"
                )));
            }
            if page.truncated {
                return Err(ToolError::Failed(format!(
                    "{label} is an image larger than {}",
                    media::size(MAX_BODY)
                )));
            }
            return media::image(&page.body, media_type, label);
        }
        Some(Media::Pdf) => {
            let (text, pages) = media::pdf_text(&page.body, label)?;
            notes.push(media::pages(pages));
            text
        }
        None if is_html(&mime, &page.body) => {
            let html = String::from_utf8_lossy(&page.body);
            if selector.raw {
                notes.push("raw HTML".to_string());
                html.into_owned()
            } else {
                let base = reqwest::Url::parse(&page.url).ok();
                let converted = html::to_markdown(&html, base.as_ref(), selector.full);
                notes.push(if converted.narrowed {
                    "main content as Markdown; :full for the whole page".to_string()
                } else {
                    "converted to Markdown".to_string()
                });
                converted.text
            }
        }
        None if is_text(&mime, &page.body) => {
            let text = String::from_utf8_lossy(&page.body).into_owned();
            let pretty = (!selector.raw && !page.truncated && is_json(&mime, &text))
                .then(|| pretty_json(&text))
                .flatten();
            match pretty {
                Some(pretty) => {
                    notes.push("pretty-printed".to_string());
                    pretty
                }
                None => text,
            }
        }
        None => {
            return Err(ToolError::Failed(format!(
                "{label} is {}, which read cannot show",
                if mime.is_empty() {
                    "binary data"
                } else {
                    mime.as_str()
                }
            )));
        }
    };
    let numbered = number_saved(&text, selector, label, spill, "read-url")?;
    Ok(ToolOutput::new(format!(
        "[url {label}, {}]\n{numbered}",
        notes.join(", ")
    )))
}

/// Numbers the lines of `text`, called `label`, that `selector` picks, as a
/// file's lines are numbered.
///
/// When `text` is longer than one read shows, it is saved whole in `spill`
/// under a name made from `kind`, a note names the file, and the note saying
/// where to go on points into it, so that the rest is read from there with
/// line suffixes instead of being fetched again.
///
/// # Errors
///
/// Returns [`ToolError`] when the text cannot be numbered.
pub(super) fn number_saved(
    text: &str,
    selector: &Selector,
    label: &str,
    spill: &Spill,
    kind: &str,
) -> Result<String, ToolError> {
    let mut numbered = lines::number(
        || Ok(std::io::Cursor::new(text.as_bytes())),
        selector,
        label,
    )?;
    let total = text.lines().count();
    if (total > lines::MAX_LINES || text.len() > lines::MAX_BYTES)
        && let Ok(path) = spill.save(kind, text.as_bytes())
    {
        let path = path.display().to_string();
        numbered = numbered.replace(
            &format!("[more lines remain; continue with {label}:"),
            &format!("[more lines remain; continue with {path}:"),
        );
        numbered.push_str(&format!(
            "[all {total} lines are saved in {path}; read the rest there with line suffixes \
             instead of fetching again]\n"
        ));
    }
    Ok(numbered)
}

/// Whether a text body of type `mime` reading `text` is JSON: served as JSON,
/// or served as plain text or with no type and starting like a JSON object or
/// array.
fn is_json(mime: &str, text: &str) -> bool {
    if mime == "application/json" || mime.ends_with("+json") || mime.contains("/json") {
        return true;
    }
    matches!(mime, "" | "text/plain" | "application/octet-stream")
        && text.trim_start().starts_with(['{', '['])
}

/// `text` re-indented two spaces a level, with keys in the order written,
/// when it is valid JSON; nothing when it is not.
pub(super) fn pretty_json(text: &str) -> Option<String> {
    serde_json::from_str::<serde::de::IgnoredAny>(text).ok()?;
    let mut out = String::with_capacity(text.len() * 2);
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut chars = text.trim().chars().peekable();
    let newline = |out: &mut String, depth: usize| {
        out.push('\n');
        out.push_str(&"  ".repeat(depth));
    };
    while let Some(c) = chars.next() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                while chars.peek().is_some_and(|next| next.is_whitespace()) {
                    chars.next();
                }
                if chars.peek().is_some_and(|next| matches!(next, '}' | ']')) {
                    out.extend(chars.next());
                } else {
                    depth += 1;
                    newline(&mut out, depth);
                }
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                newline(&mut out, depth);
                out.push(c);
            }
            ',' => {
                out.push(c);
                newline(&mut out, depth);
            }
            ':' => out.push_str(": "),
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    out.push('\n');
    Some(out)
}

/// Whether a body of type `mime` starting with `body` is an HTML page.
fn is_html(mime: &str, body: &[u8]) -> bool {
    if mime == "text/html" || mime == "application/xhtml+xml" {
        return true;
    }
    if !mime.is_empty() && mime != "text/plain" && mime != "application/octet-stream" {
        return false;
    }
    let start = String::from_utf8_lossy(&body[..body.len().min(512)]).to_ascii_lowercase();
    let start = start.trim_start_matches('\u{feff}').trim_start();
    start.starts_with("<!doctype html") || start.starts_with("<html")
}

/// Whether a body of type `mime` starting with `body` is text worth showing.
fn is_text(mime: &str, body: &[u8]) -> bool {
    let textual = mime.starts_with("text/")
        || [
            "json",
            "xml",
            "javascript",
            "yaml",
            "toml",
            "csv",
            "markdown",
        ]
        .iter()
        .any(|kind| mime.contains(kind));
    textual
        || (matches!(mime, "" | "application/octet-stream")
            && !body[..body.len().min(8192)].contains(&0))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page served as `content_type` with `body`.
    fn page(content_type: &str, body: &[u8]) -> Page {
        Page {
            redirected_from: None,
            url: "https://example.com/doc".to_string(),
            content_type: content_type.to_string(),
            body: body.to_vec(),
            truncated: false,
        }
    }

    /// The HTML used by the conversion tests.
    const HTML: &str = "<!DOCTYPE html><html><head><title>Fish &amp; Chips</title>\
        <style>body { color: red }</style><script>alert('x')</script></head>\
        <body><h1>Menu</h1><p>Fresh <b>cod</b> and <a href=\"/chips\">chips</a>.</p>\
        <ul><li>Salt</li><li>Vinegar</li></ul><noscript>enable js</noscript></body></html>";

    /// Only http and https addresses count as URLs.
    #[test]
    fn recognizes_urls() {
        assert!(is_url("https://example.com"));
        assert!(is_url("HTTP://example.com/a"));
        assert!(!is_url("ftp://example.com"));
        assert!(!is_url("src/http.rs"));
        assert!(!is_url("http"));
    }

    /// An error with a cause, for checking that causes are reported.
    #[derive(Debug)]
    struct Outer(std::io::Error);

    impl std::fmt::Display for Outer {
        /// Says only that sending failed, as a request error does.
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("error sending request")
        }
    }

    impl std::error::Error for Outer {
        /// The underlying error.
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            Some(&self.0)
        }
    }

    /// The reason under a general error is spelled out.
    #[test]
    fn reports_causes() {
        let error = Outer(std::io::Error::other(
            "x resolves only to local or private addresses",
        ));
        assert_eq!(
            with_causes(&error),
            "error sending request: x resolves only to local or private addresses"
        );
    }

    /// A URL is parsed once into what is fetched, with its selector split off,
    /// and addresses without a plain host are refused.
    #[test]
    fn parses_targets() {
        let (url, selector) = target("https://example.com/a/b:10-20").unwrap();
        assert_eq!(url.as_str(), "https://example.com/a/b");
        assert!(selector.picks_lines());
        let (url, _) = target("http://localhost:8080").unwrap();
        assert_eq!(url.origin().ascii_serialization(), "http://localhost:8080");
        for bad in [
            "http:///evil.example/",
            "http://\\evil.example/",
            "https://",
            "http://:80/",
        ] {
            assert!(target(bad).is_err(), "{bad}");
        }
        assert_eq!(scope("https://Example.COM/x"), "https://example.com");
    }

    /// HTML becomes Markdown with the title first and scripts and styles gone.
    #[test]
    fn converts_html_to_markdown() {
        let markdown = html::to_markdown(HTML, None, false).text;
        assert!(
            markdown.starts_with("Title: Fish & Chips\n\n"),
            "{markdown}"
        );
        assert!(markdown.contains("# Menu"), "{markdown}");
        assert!(markdown.contains("**cod**"), "{markdown}");
        assert!(markdown.contains("[chips](/chips)"), "{markdown}");
        assert!(markdown.contains("Salt"), "{markdown}");
        assert!(!markdown.contains("alert"), "{markdown}");
        assert!(!markdown.contains("color: red"), "{markdown}");
        assert!(!markdown.contains("enable js"), "{markdown}");
    }

    /// A page is numbered Markdown by default and its source with `raw`.
    #[test]
    fn renders_html_pages() {
        let html = page("text/html; charset=utf-8", HTML.as_bytes());
        let text = render(&html, &Selector::default(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(
            text.starts_with("[url https://example.com/doc, text/html, converted to Markdown]\n"),
            "{text}"
        );
        assert!(text.contains("\tTitle: Fish & Chips\n"), "{text}");
        let raw = render(&html, &Selector::parse("raw").unwrap(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(raw.contains("raw HTML"), "{raw}");
        assert!(raw.contains("<script>alert('x')</script>"), "{raw}");
        let first = render(&html, &Selector::parse("1").unwrap(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(first.contains("     1\tTitle: Fish & Chips\n"), "{first}");
        let sniffed = render(
            &page("", HTML.as_bytes()),
            &Selector::default(),
            &Spill::disabled(),
        )
        .unwrap();
        assert!(sniffed.text.contains("converted to Markdown"));
    }

    /// Images come back as images, other binary data is refused, text passes.
    #[test]
    fn renders_other_bodies() {
        let png = page("image/png", b"\x89PNG\r\n\x1a\nrest");
        let output = render(&png, &Selector::default(), &Spill::disabled()).unwrap();
        assert_eq!(output.images.len(), 1);
        assert!(render(&png, &Selector::parse("1-2").unwrap(), &Spill::disabled()).is_err());
        let zip = page("application/zip", b"PK\x03\x04\0\0");
        assert!(matches!(
            render(&zip, &Selector::default(), &Spill::disabled()),
            Err(ToolError::Failed(m)) if m.contains("application/zip")
        ));
        let json = page("application/json", b"{\"a\": 1}\n");
        let text = render(&json, &Selector::default(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(text.contains("pretty-printed"), "{text}");
        assert!(
            text.ends_with("     1\t{\n     2\t  \"a\": 1\n     3\t}\n"),
            "{text}"
        );
        let raw = render(&json, &Selector::parse("raw").unwrap(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(raw.ends_with("     1\t{\"a\": 1}\n"), "{raw}");
    }

    /// A PDF served over HTTP gives its text.
    #[test]
    fn renders_pdfs() {
        let pdf = page("application/pdf", &media::tests::pdf("Served PDF"));
        let text = render(&pdf, &Selector::default(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(text.contains("application/pdf, 1 page]"), "{text}");
        assert!(text.contains("Served PDF"), "{text}");
    }

    /// JSON is re-indented with its keys in the order written, strings left
    /// alone, and anything that is not JSON is refused.
    #[test]
    fn pretty_prints_json() {
        let text = r#"{"z":1,"a":[1,{"s":"x,{y}: \"q\""}],"e":{},"l":[ ]}"#;
        assert_eq!(
            pretty_json(text).unwrap(),
            "{\n  \"z\": 1,\n  \"a\": [\n    1,\n    {\n      \"s\": \"x,{y}: \\\"q\\\"\"\n    }\n  ],\n  \
             \"e\": {},\n  \"l\": []\n}\n"
        );
        assert_eq!(pretty_json("{\"a\": "), None);
        assert!(is_json("text/plain", " [1]"));
        assert!(is_json("application/vnd.api+json", "x"));
        assert!(!is_json("text/plain", "plain"));
    }

    /// A page longer than one read shows is saved whole, and the result says
    /// where.
    #[test]
    fn saves_long_pages() {
        let dir = crate::testutil::scratch("web-spill");
        let spill = Spill::new(&dir);
        let body: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
        let text = render(
            &page("text/plain", body.as_bytes()),
            &Selector::default(),
            &spill,
        )
        .unwrap()
        .text;
        let saved = std::fs::read_dir(&dir)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let shown = saved.display();
        assert!(
            text.contains(&format!(
                "[more lines remain; continue with {shown}:2001]\n"
            )),
            "{text}"
        );
        let note = text.lines().last().unwrap();
        assert!(
            note.starts_with(&format!("[all 2500 lines are saved in {shown};")),
            "{note}"
        );
        assert_eq!(std::fs::read_to_string(&saved).unwrap(), body);
        let short = render(&page("text/plain", b"one\n"), &Selector::default(), &spill)
            .unwrap()
            .text;
        assert!(!short.contains("saved in"), "{short}");
    }

    /// An HTML page keeps its main content and says so.
    #[test]
    fn renders_main_content() {
        let html = page("text/html", include_bytes!("fixtures/docs_page.html"));
        let text = render(&html, &Selector::default(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(
            text.contains("main content as Markdown; :full for the whole page"),
            "{text}"
        );
        assert!(!text.contains("Products"), "{text}");
        let full = render(&html, &Selector::parse("full").unwrap(), &Spill::disabled())
            .unwrap()
            .text;
        assert!(full.contains("Products"), "{full}");
    }
}
