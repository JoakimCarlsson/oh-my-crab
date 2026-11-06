//! Turning an HTML page into the Markdown a reader wants: the page's main
//! content, with navigation, banners, sidebars and other boilerplate left out,
//! and links made absolute so that they can be read in turn.
//!
//! Nothing on the page runs: scripts are dropped unread, and only the markup
//! the server sent is converted.

use html5ever::tendril::StrTendril;
use markup5ever_rcdom::{Handle, NodeData};

use super::media;

/// Elements whose content is never text a reader sees.
const SKIPPED_TAGS: [&str; 13] = [
    "head", "script", "style", "noscript", "template", "svg", "canvas", "iframe", "object",
    "button", "select", "dialog", "input",
];

/// Elements that hold a site's chrome rather than a page's content.
const CHROME_TAGS: [&str; 3] = ["nav", "footer", "aside"];

/// ARIA roles of a site's chrome rather than a page's content.
const CHROME_ROLES: [&str; 8] = [
    "navigation",
    "banner",
    "contentinfo",
    "complementary",
    "search",
    "menu",
    "menubar",
    "dialog",
];

/// Words in a class or id that mark an element as chrome, such as the `sidebar`
/// of `left-sidebar` or the `cookie` of `cookie_notice`.
const CHROME_WORDS: [&str; 18] = [
    "nav",
    "navbar",
    "navigation",
    "menu",
    "sidebar",
    "breadcrumb",
    "breadcrumbs",
    "footer",
    "cookie",
    "cookies",
    "consent",
    "advert",
    "advertisement",
    "ads",
    "share",
    "social",
    "newsletter",
    "popup",
];

/// Fewest characters of text a `main` or `article` element must hold to be
/// taken for the page's content.
const MIN_CONTENT: usize = 200;

/// A page converted to Markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Converted {
    /// The Markdown, with a `Title:` line first when the page has a title.
    pub(super) text: String,
    /// Whether only the main content was kept, leaving the rest of the page out.
    pub(super) narrowed: bool,
}

/// Converts the HTML page `html` into Markdown, keeping only its main content
/// unless `full` is set.
///
/// The main content is the page's `main` element, or its one `article`, once
/// navigation, headers, footers, sidebars, cookie banners and hidden elements
/// are taken out; without such an element the whole body is kept, still
/// without that chrome. When narrowing would leave next to nothing of a page
/// that has text, the whole page is converted instead. Links and images are
/// made absolute against `base`, and `javascript:` links lose their target.
/// A `Title:` line naming the page's `<title>` comes first, and runs of blank
/// lines are squeezed into one.
pub(super) fn to_markdown(html: &str, base: Option<&reqwest::Url>, full: bool) -> Converted {
    let converter = htmd::HtmlToMarkdown::builder()
        .skip_tags(SKIPPED_TAGS.to_vec())
        .build();
    let Ok(tree) = converter.html_to_tree(html) else {
        return Converted {
            text: String::new(),
            narrowed: false,
        };
    };
    let title = find(&tree, &|node| tag(node) == Some("title"))
        .map(|node| collapse(&text_of(&node)))
        .filter(|title| !title.is_empty());
    absolutize(&tree, base);
    let whole = text_len(&tree);
    let mut narrowed = false;
    let mut root = tree.clone();
    if !full {
        prune(&tree, false);
        let content = content_root(&tree);
        let kept = text_len(content.as_ref().unwrap_or(&tree));
        if kept >= MIN_CONTENT.min(whole / 2) {
            narrowed = kept < whole;
            if let Some(content) = content {
                root = content;
            }
        } else {
            root = converter.html_to_tree(html).unwrap_or(tree);
            absolutize(&root, base);
        }
    }
    let body = compact_tables(&media::tidy(&converter.tree_to_markdown(&root)));
    let text = match title {
        Some(title) => format!("Title: {title}\n\n{body}"),
        None => body,
    };
    Converted { text, narrowed }
}

/// `markdown` with the padding taken out of its table rows: cells are
/// separated by one space each side of `|`, and separator rows use `---`, so
/// that a wide table does not fill a read with spaces and dashes.
fn compact_tables(markdown: &str) -> String {
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.lines() {
        if line.starts_with('|') && line.trim_end().ends_with('|') {
            let cells: Vec<String> = cells(line.trim())
                .into_iter()
                .map(|cell| {
                    let cell = squeeze_spaces(cell);
                    if !cell.is_empty() && cell.chars().all(|c| matches!(c, '-' | ':')) {
                        let left = if cell.starts_with(':') { ":" } else { "" };
                        let right = if cell.len() > 1 && cell.ends_with(':') {
                            ":"
                        } else {
                            ""
                        };
                        format!("{left}---{right}")
                    } else {
                        cell
                    }
                })
                .collect();
            out.push_str(&format!("| {} |", cells.join(" | ")));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// The cells of the Markdown table row `row`, split at every `|` that is not
/// escaped as `\|`.
fn cells(row: &str) -> Vec<&str> {
    let inner = row.strip_prefix('|').unwrap_or(row);
    let inner = inner.strip_suffix('|').unwrap_or(inner);
    let mut cells = Vec::new();
    let mut start = 0;
    let mut previous = '\0';
    for (index, c) in inner.char_indices() {
        if c == '|' && previous != '\\' {
            cells.push(&inner[start..index]);
            start = index + 1;
        }
        previous = c;
    }
    cells.push(&inner[start..]);
    cells
}

/// `cell` trimmed, with runs of spaces inside it turned into one.
fn squeeze_spaces(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    for word in cell.split(' ').filter(|word| !word.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// The lowercase tag name of `node` when it is an element.
pub(crate) fn tag(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(&name.local),
        _ => None,
    }
}

/// The value of the attribute `name` of `node`, when it is an element that has
/// one.
pub(crate) fn attr(node: &Handle, name: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|attribute| &*attribute.name.local == name)
            .map(|attribute| attribute.value.to_string()),
        _ => None,
    }
}

/// Whether `node` is an element whose class list holds `class`.
pub(crate) fn has_class(node: &Handle, class: &str) -> bool {
    attr(node, "class").is_some_and(|value| value.split_whitespace().any(|name| name == class))
}

/// The first node under `node`, `node` included, in document order that
/// `matches`.
pub(crate) fn find(node: &Handle, matches: &dyn Fn(&Handle) -> bool) -> Option<Handle> {
    if matches(node) {
        return Some(node.clone());
    }
    node.children
        .borrow()
        .iter()
        .find_map(|child| find(child, matches))
}

/// Every node under `node`, `node` included, in document order that
/// `matches`; the nodes under a match are not searched.
pub(crate) fn find_all(node: &Handle, matches: &dyn Fn(&Handle) -> bool) -> Vec<Handle> {
    let mut found = Vec::new();
    collect(node, matches, &mut found);
    found
}

/// Adds to `found` the nodes under `node` that `matches`, as [`find_all`] does.
fn collect(node: &Handle, matches: &dyn Fn(&Handle) -> bool, found: &mut Vec<Handle>) {
    if matches(node) {
        found.push(node.clone());
        return;
    }
    for child in node.children.borrow().iter() {
        collect(child, matches, found);
    }
}

/// The text under `node`, as written, leaving out elements no reader sees.
pub(crate) fn text_of(node: &Handle) -> String {
    let mut out = String::new();
    gather(node, &mut out);
    out
}

/// Appends the text under `node` to `out`, as [`text_of`] does.
fn gather(node: &Handle, out: &mut String) {
    match &node.data {
        NodeData::Text { contents } => out.push_str(&contents.borrow()),
        NodeData::Element { name, .. }
            if SKIPPED_TAGS.contains(&&*name.local) && &*name.local != "head" => {}
        _ => {
            for child in node.children.borrow().iter() {
                gather(child, out);
            }
        }
    }
}

/// `text` with every run of whitespace turned into one space, and trimmed.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// How many characters of visible text are under `node`, not counting
/// whitespace or the content of `head` and the other skipped elements.
fn text_len(node: &Handle) -> usize {
    match &node.data {
        NodeData::Text { contents } => contents
            .borrow()
            .chars()
            .filter(|c| !c.is_whitespace())
            .count(),
        NodeData::Element { name, .. } if SKIPPED_TAGS.contains(&&*name.local) => 0,
        _ => node.children.borrow().iter().map(text_len).sum(),
    }
}

/// Removes the chrome under `node`: navigation, footers, sidebars, hidden
/// elements, and headers or class-marked chrome outside the content.
///
/// `in_content` says whether `node` is inside a `main` or `article`, where a
/// `header` holds the article's own heading and is kept.
fn prune(node: &Handle, in_content: bool) {
    let in_content = in_content || matches!(tag(node), Some("main" | "article"));
    node.children
        .borrow_mut()
        .retain(|child| !is_chrome(child, in_content));
    for child in node.children.borrow().iter() {
        prune(child, in_content);
    }
}

/// Whether `node` is chrome to drop, `in_content` saying whether it is inside
/// the page's `main` or an `article`.
///
/// Elements are only dropped for their class, id or `header` tag when they hold
/// no `main`, `article` or `h1`, so that a wrapper such as
/// `<div class="with-sidebar">` around the whole page is kept.
fn is_chrome(node: &Handle, in_content: bool) -> bool {
    let Some(name) = tag(node) else {
        return false;
    };
    if matches!(name, "html" | "body" | "main" | "article") {
        return false;
    }
    if attr(node, "hidden").is_some()
        || attr(node, "aria-hidden").as_deref() == Some("true")
        || attr(node, "style").is_some_and(|style| {
            style
                .to_ascii_lowercase()
                .replace(' ', "")
                .contains("display:none")
        })
    {
        return true;
    }
    if CHROME_TAGS.contains(&name) {
        return true;
    }
    if let Some(role) = attr(node, "role")
        && CHROME_ROLES.contains(&role.trim().to_ascii_lowercase().as_str())
        && !(in_content && role.trim().eq_ignore_ascii_case("banner"))
    {
        return true;
    }
    let marked = (name == "header" && !in_content)
        || ["class", "id"]
            .iter()
            .filter_map(|key| attr(node, key))
            .any(|value| value.split_whitespace().any(marks_chrome));
    marked && !holds_content(node)
}

/// Whether the class or id `token` names chrome: one of its words, split at
/// `-` and `_`, is one of [`CHROME_WORDS`].
fn marks_chrome(token: &str) -> bool {
    token
        .to_ascii_lowercase()
        .split(['-', '_'])
        .any(|word| CHROME_WORDS.contains(&word))
}

/// Whether `node` holds a `main`, an `article` or an `h1` below it.
fn holds_content(node: &Handle) -> bool {
    node.children.borrow().iter().any(|child| {
        find(child, &|inner| {
            matches!(tag(inner), Some("main" | "article" | "h1"))
                || attr(inner, "role").as_deref() == Some("main")
        })
        .is_some()
    })
}

/// The element holding the page's main content: the `main` element, or one
/// with the `main` role, with the most text, or else the `article` with the
/// most text when it holds most of the page's text; nothing when the page has
/// neither or they hold under [`MIN_CONTENT`] characters.
fn content_root(tree: &Handle) -> Option<Handle> {
    let largest = |nodes: Vec<Handle>| {
        nodes
            .into_iter()
            .map(|node| (text_len(&node), node))
            .max_by_key(|(len, _)| *len)
    };
    let mains = find_all(tree, &|node| {
        tag(node) == Some("main") || attr(node, "role").as_deref() == Some("main")
    });
    if let Some((len, main)) = largest(mains)
        && len >= MIN_CONTENT
    {
        return Some(main);
    }
    let articles = find_all(tree, &|node| tag(node) == Some("article"));
    let (len, article) = largest(articles)?;
    (len >= MIN_CONTENT && len * 2 >= text_len(tree)).then_some(article)
}

/// Makes the `href` of every link and the `src` of every image under `node`
/// absolute against `base`, and drops the target of `javascript:` links.
fn absolutize(node: &Handle, base: Option<&reqwest::Url>) {
    if let NodeData::Element { name, attrs, .. } = &node.data {
        let key = match &*name.local {
            "a" => Some("href"),
            "img" => Some("src"),
            _ => None,
        };
        if let Some(key) = key {
            let mut attrs = attrs.borrow_mut();
            attrs.retain(|attribute| {
                !(&*attribute.name.local == key
                    && attribute
                        .value
                        .trim_start()
                        .to_ascii_lowercase()
                        .starts_with("javascript:"))
            });
            if let Some(base) = base {
                for attribute in attrs.iter_mut() {
                    if &*attribute.name.local == key
                        && let Ok(joined) = base.join(attribute.value.trim())
                    {
                        attribute.value = StrTendril::from(joined.to_string());
                    }
                }
            }
        }
    }
    for child in node.children.borrow().iter() {
        absolutize(child, base);
    }
}

/// Parses `html` into a tree without converting it, for callers that pick
/// things out of a page themselves.
pub(crate) fn parse(html: &str) -> Option<Handle> {
    htmd::HtmlToMarkdown::builder()
        .build()
        .html_to_tree(html)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A documentation page with the usual chrome around its content.
    const DOCS: &str = include_str!("fixtures/docs_page.html");

    /// The main content is kept, the chrome around it is not, and links are
    /// made absolute.
    #[test]
    fn keeps_the_main_content() {
        let base = reqwest::Url::parse("https://docs.example.com/guide/start").unwrap();
        let converted = to_markdown(DOCS, Some(&base), false);
        let text = &converted.text;
        assert!(converted.narrowed);
        assert!(
            text.starts_with("Title: Getting started | Example Docs\n\n"),
            "{text}"
        );
        assert!(text.contains("# Getting started"), "{text}");
        assert!(text.contains("Install the tool with"), "{text}");
        assert!(text.contains("```"), "{text}");
        assert!(
            text.contains("[configuration](https://docs.example.com/guide/config)"),
            "{text}"
        );
        assert!(
            text.contains("[the FAQ](https://docs.example.com/faq)"),
            "{text}"
        );
        for chrome in [
            "Products",
            "Accept cookies",
            "On this page",
            "Copyright",
            "Subscribe to our newsletter",
            "Hidden note",
            "alert(",
            "Sign in",
        ] {
            assert!(!text.contains(chrome), "{chrome} kept in {text}");
        }
    }

    /// With `full` the whole page is converted, chrome included.
    #[test]
    fn converts_the_whole_page_on_request() {
        let converted = to_markdown(DOCS, None, true);
        assert!(!converted.narrowed);
        assert!(converted.text.contains("Products"), "{}", converted.text);
        assert!(converted.text.contains("Install the tool with"));
        assert!(!converted.text.contains("alert("));
    }

    /// A page without a main element keeps its body, minus the chrome.
    #[test]
    fn keeps_the_body_without_a_main_element() {
        let html = "<html><head><title>Plain</title></head><body>\
            <nav><a href=\"/\">Home</a> <a href=\"/blog\">Blog</a></nav>\
            <div class=\"wrapper with-sidebar\"><h1>Plain page</h1>\
            <p>Some words that matter, written for the reader to see.</p></div>\
            <div class=\"left-sidebar\">Elsewhere links</div>\
            <footer>All rights reserved</footer></body></html>";
        let text = to_markdown(html, None, false).text;
        assert!(text.contains("# Plain page"), "{text}");
        assert!(text.contains("Some words that matter"), "{text}");
        assert!(!text.contains("Blog"), "{text}");
        assert!(!text.contains("Elsewhere"), "{text}");
        assert!(!text.contains("rights reserved"), "{text}");
    }

    /// When pruning would leave next to nothing, the whole page is kept.
    #[test]
    fn falls_back_when_pruning_leaves_nothing() {
        let html = "<html><body><nav><p>This page is only a list of links, and its text \
            should not vanish just because it sits in a nav element.</p></nav></body></html>";
        let converted = to_markdown(html, None, false);
        assert!(!converted.narrowed);
        assert!(
            converted.text.contains("only a list of links"),
            "{}",
            converted.text
        );
    }

    /// Tables lose the padding that lines up their columns.
    #[test]
    fn compacts_tables() {
        let html = "<table><tr><th>Name</th><th>Value</th></tr>\
            <tr><td>a</td><td>a much longer value than the heading</td></tr></table>";
        let text = to_markdown(html, None, false).text;
        assert!(
            text.contains(
                "| Name | Value |\n| --- | --- |\n| a | a much longer value than the heading |"
            ),
            "{text}"
        );
        assert!(!text.contains("  "), "{text}");
        assert_eq!(compact_tables("| a\\|b |  c  |\n"), "| a\\|b | c |\n");
    }

    /// `javascript:` links lose their target and keep their text.
    #[test]
    fn drops_script_links() {
        let html = "<p>Click <a href=\"javascript:void(0)\">here</a> or \
            <a href=\"page2\">there</a>.</p>";
        let base = reqwest::Url::parse("https://example.com/a/b").unwrap();
        let text = to_markdown(html, Some(&base), false).text;
        assert!(!text.contains("javascript"), "{text}");
        assert!(text.contains("here"), "{text}");
        assert!(
            text.contains("[there](https://example.com/a/page2)"),
            "{text}"
        );
    }
}
