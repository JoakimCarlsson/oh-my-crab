//! The `web_search` tool: one query to a web search service, answered with
//! ranked results and, from services that write one, a short answer.
//!
//! Which services are asked, and in what order, comes from the environment:
//! see [`backend::Plan`]. The first service that answers wins; one that fails
//! is noted and the next is asked.

mod backend;
mod engines;
mod mcp;

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Tool, ToolError, ToolOutput};
use backend::{Backend, Engine, Found, Plan, Recency};

/// How long one service may take to answer.
const TIMEOUT: Duration = Duration::from_secs(20);

/// How many results a search returns when the call does not say.
const DEFAULT_LIMIT: usize = 8;

/// The most results a search returns.
const MAX_LIMIT: usize = 20;

/// Most bytes of a service's answer read.
const MAX_BODY: usize = 4 << 20;

/// Longest snippet shown for one result, in characters.
const MAX_SNIPPET: usize = 400;

/// The `User-Agent` sent with every search.
const USER_AGENT: &str = concat!("crab/", env!("CARGO_PKG_VERSION"));

/// Searches the web through the services the environment sets up.
pub struct WebSearch {
    /// Reads the environment; replaced in tests.
    env: fn(&str) -> Option<String>,
}

impl Default for WebSearch {
    /// A search reading the process's environment.
    fn default() -> Self {
        Self {
            env: |name| std::env::var(name).ok(),
        }
    }
}

impl WebSearch {
    /// Creates the tool, reading its services from the environment at each
    /// call so that a key set later is picked up.
    pub fn new() -> Self {
        Self::default()
    }
}

/// Arguments of a `web_search` call.
#[derive(Deserialize)]
struct Args {
    /// What to search for.
    query: String,
    /// How many results to return.
    limit: Option<usize>,
    /// How recent results must be: `day`, `week`, `month` or `year`.
    recency: Option<String>,
}

#[async_trait]
impl Tool for WebSearch {
    /// Returns `web_search`.
    fn name(&self) -> &'static str {
        "web_search"
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../../prompts/web_search.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to search for, as you would type it into a search engine."
                },
                "limit": {
                    "type": "integer",
                    "description": "How many results to return, 1 to 20; 8 when left out."
                },
                "recency": {
                    "type": "string",
                    "enum": ["day", "week", "month", "year"],
                    "description": "Only results published within this time, for news and releases."
                }
            },
            "required": ["query"]
        })
    }

    /// Searching changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// A search sends the query to a service outside the machine, so the user
    /// approves searching, as they approve shell commands; one "always allow"
    /// covers every later search.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shows the query.
    fn describe(&self, args: &Value) -> String {
        args["query"].as_str().unwrap_or("?").to_owned()
    }

    /// Asks the services in order until one answers, and lists its results.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(ToolError::InvalidArguments("the query is empty".to_owned()));
        }
        let limit = args.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
        let recency = match args.recency.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(name) => Some(Recency::named(name).ok_or_else(|| {
                ToolError::InvalidArguments(format!(
                    "recency is {name}; it takes day, week, month or year"
                ))
            })?),
        };
        search(&Plan::from_env(self.env), query, limit, recency).await
    }
}

/// Asks the services of `plan` in order for `limit` results for `query`,
/// limited to `recency`, until one answers, and lists what it found, noting
/// the services that failed before it.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] when there is no service to ask, saying what
/// to set, or when every service failed, saying why each did.
async fn search(
    plan: &Plan,
    query: &str,
    limit: usize,
    recency: Option<Recency>,
) -> Result<ToolOutput, ToolError> {
    if plan.backends.is_empty() {
        return Err(ToolError::Failed(unconfigured(&plan.skipped)));
    }
    let mut failures = Vec::new();
    for backend in &plan.backends {
        match ask(backend, query, limit, recency).await {
            Ok(found) => {
                return Ok(ToolOutput::new(listing(
                    backend.engine,
                    query,
                    &found,
                    &failures,
                    &plan.skipped,
                )));
            }
            Err(reason) => failures.push(format!("{} failed: {reason}", backend.engine.name())),
        }
    }
    let mut message = format!("every web search service failed: {}", failures.join("; "));
    if plan.backends.iter().all(|backend| {
        backend.engine != Engine::Searxng
            && (backend.key.is_empty() || backend.engine.variables().is_empty())
    }) {
        message.push_str(&format!("; {}", setup_hint()));
    }
    Err(ToolError::Failed(message))
}

/// The variables that set up a service, as a sentence.
fn setup_hint() -> String {
    let keys: Vec<&str> = Engine::ALL
        .iter()
        .filter(|engine| !matches!(engine, Engine::Searxng | Engine::DuckDuckGo))
        .filter_map(|engine| engine.variables().first().copied())
        .collect();
    format!(
        "for a dependable search set one of {} to an API key, or SEARXNG_URL to a SearXNG \
         instance with JSON output turned on",
        keys.join(", ")
    )
}

/// Why no service can be asked: the reasons in `skipped`, and what to set.
fn unconfigured(skipped: &[String]) -> String {
    let mut message = "no web search service is set up".to_owned();
    if !skipped.is_empty() {
        message.push_str(&format!(" ({})", skipped.join("; ")));
    }
    message.push_str(&format!(
        ": {}; or add parallel, exa, keenable, firecrawl or duckduckgo, which answer \
         without a key, to {}",
        setup_hint(),
        backend::ORDER_VARIABLE
    ));
    message
}

/// Asks the service `backend` for `limit` results for `query`, limited to
/// `recency`.
///
/// # Errors
///
/// Returns the reason when the request fails, the service answers with an
/// error, or its answer cannot be read.
async fn ask(
    backend: &Backend,
    query: &str,
    limit: usize,
    recency: Option<Recency>,
) -> Result<Found, String> {
    let call = backend.call(query, limit, recency)?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = if call.post {
        client.post(call.url.clone())
    } else {
        client.get(call.url.clone())
    };
    for (name, value) in &call.headers {
        request = request.header(*name, value);
    }
    if let Some((content_type, body)) = call.body {
        request = request
            .header(reqwest::header::CONTENT_TYPE, content_type)
            .body(body);
    }
    let mut response = request.send().await.map_err(|error| {
        let mut text = error.to_string();
        let mut cause = std::error::Error::source(&error);
        while let Some(inner) = cause {
            text.push_str(&format!(": {inner}"));
            cause = inner.source();
        }
        text
    })?;
    let status = response.status();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        body.extend_from_slice(&chunk);
        if body.len() > MAX_BODY {
            return Err(format!("the answer is larger than {} MB", MAX_BODY >> 20));
        }
    }
    if status != reqwest::StatusCode::OK {
        let said = String::from_utf8_lossy(&body[..body.len().min(300)]);
        let said = backend::plain(&said);
        return Err(match status.as_u16() {
            401 | 403 if backend.key.is_empty() => format!("{status}, refused: {said}"),
            401 | 403 => format!(
                "{status}, the key in {} was refused: {said}",
                backend
                    .engine
                    .variables()
                    .first()
                    .unwrap_or(&"the environment")
            ),
            202 | 429 => format!("{status}, rate limited: {said}"),
            _ => format!("{status}: {said}"),
        });
    }
    backend.parse(&body, limit)
}

/// The text of a search for `query` that `engine` answered with `found`,
/// after the services in `failures` failed; services named in `OMC_SEARCH`
/// that could not be asked, `skipped`, are noted too.
fn listing(
    engine: Engine,
    query: &str,
    found: &Found,
    failures: &[String],
    skipped: &[String],
) -> String {
    let count = found.hits.len();
    let noun = if count == 1 { "result" } else { "results" };
    let mut out = format!("[web search via {}, {count} {noun}", engine.name());
    for note in failures.iter().chain(skipped) {
        out.push_str(&format!("; {note}"));
    }
    out.push_str("]\n");
    if let Some(answer) = &found.answer {
        out.push_str(&format!("Answer: {}\n\n", answer.trim()));
    }
    if found.hits.is_empty() {
        out.push_str(&format!("Nothing was found for {query}.\n"));
    }
    for (index, hit) in found.hits.iter().enumerate() {
        let title = if hit.title.is_empty() {
            hit.url.as_str()
        } else {
            hit.title.as_str()
        };
        out.push_str(&format!("{}. {title}\n   {}", index + 1, hit.url));
        if let Some(date) = &hit.date {
            out.push_str(&format!(" · {date}"));
        }
        out.push('\n');
        if !hit.snippet.is_empty() {
            out.push_str(&format!("   {}\n", clip(&hit.snippet)));
        }
    }
    out
}

/// `text` cut to [`MAX_SNIPPET`] characters, with an ellipsis when cut.
fn clip(text: &str) -> String {
    match text.char_indices().nth(MAX_SNIPPET) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{Received, serve};

    /// The backend for `engine` at `base` with the key `k`.
    fn at(engine: Engine, base: String) -> Backend {
        Backend {
            engine,
            base,
            key: "k".to_owned(),
        }
    }

    /// Answers like a Brave server that refuses the key, and a Tavily server
    /// that answers from the fixture.
    fn answer(request: &Received) -> crate::testutil::Answer {
        if request.target.starts_with("/brave") {
            return (401, "application/json", br#"{"error":"bad key"}"#.to_vec());
        }
        (
            200,
            "application/json",
            include_bytes!("fixtures/tavily.json").to_vec(),
        )
    }

    /// A failing service is noted and the next one answers.
    #[tokio::test]
    async fn falls_through_to_the_next_service() {
        let (address, received) = serve(answer).await;
        let plan = Plan {
            backends: vec![
                at(Engine::Brave, format!("http://{address}/brave")),
                at(Engine::Tavily, format!("http://{address}/tavily")),
            ],
            skipped: vec!["exa needs EXA_API_KEY".to_owned()],
        };
        let text = search(&plan, "tokio runtime", 5, Some(Recency::Week))
            .await
            .unwrap()
            .text;
        assert!(
            text.starts_with(
                "[web search via tavily, 2 results; brave failed: 401 Unauthorized, the key in \
                 BRAVE_API_KEY was refused"
            ),
            "{text}"
        );
        assert!(text.contains("; exa needs EXA_API_KEY]\n"), "{text}");
        assert!(
            text.contains("Answer: Tokio is the most widely used async runtime for Rust.\n\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "1. Tokio\n   https://tokio.rs/\n   Build reliable network applications.\n"
            ),
            "{text}"
        );
        let requests = received.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].target,
            "/brave?q=tokio+runtime&count=5&freshness=pw"
        );
        assert_eq!(requests[0].header("x-subscription-token"), Some("k"));
        assert_eq!(
            (requests[0].method.as_str(), requests[1].method.as_str()),
            ("GET", "POST")
        );
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["query"], "tokio runtime");
        assert_eq!(requests[1].header("authorization"), Some("Bearer k"));
    }

    /// With no service set up, or every one failing, the error says what to set.
    #[tokio::test]
    async fn says_what_to_set() {
        let none = search(&Plan::default(), "q", 5, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(none.contains("no web search service is set up"), "{none}");
        assert!(none.contains("BRAVE_API_KEY"), "{none}");
        assert!(none.contains("SEARXNG_URL"), "{none}");
        let (address, _) = serve(|_| (500, "text/plain", b"down".to_vec())).await;
        let plan = Plan {
            backends: vec![at(Engine::DuckDuckGo, format!("http://{address}/html/"))],
            skipped: Vec::new(),
        };
        let failed = search(&plan, "q", 5, None).await.unwrap_err().to_string();
        assert!(failed.contains("duckduckgo failed: 500"), "{failed}");
        assert!(failed.contains("TAVILY_API_KEY"), "{failed}");
    }

    /// Bad arguments are refused before any service is asked.
    #[tokio::test]
    async fn checks_arguments() {
        let tool = WebSearch { env: |_| None };
        let empty = tool.call(json!({ "query": "  " })).await;
        assert!(matches!(empty, Err(ToolError::InvalidArguments(_))));
        let recency = tool
            .call(json!({ "query": "q", "recency": "decade" }))
            .await;
        assert!(matches!(recency, Err(ToolError::InvalidArguments(_))));
        let unset = WebSearch {
            env: |name| (name == "OMC_SEARCH").then(|| "kagi".to_owned()),
        };
        let error = unset
            .call(json!({ "query": "q" }))
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("kagi needs KAGI_API_KEY"), "{error}");
        assert_eq!(tool.describe(&json!({ "query": "rust 2027" })), "rust 2027");
        assert_eq!(
            tool.approval_scope(&json!({ "query": "x" })).as_deref(),
            Some("web_search")
        );
    }

    /// Long snippets are cut with an ellipsis.
    #[test]
    fn clips_snippets() {
        let long = "word ".repeat(200);
        let clipped = clip(&long);
        assert!(clipped.ends_with('…'));
        assert!(clipped.chars().count() <= MAX_SNIPPET + 1);
        assert_eq!(clip("short"), "short");
    }
}
