//! The search services `web_search` can ask, how each is chosen from the
//! environment, and what every service shares: the request it is asked with,
//! the results it answers and the helpers that read them.
//!
//! Each service lives in its own module under `engines`, described there by a
//! [`Spec`]: its name, the variables that set it up, where it is asked, and
//! how its request is made and its answer read.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use super::engines;
use crate::tools::read::html;

/// The variable that picks and orders the services, as a comma-separated list
/// of their names.
pub(super) const ORDER_VARIABLE: &str = "OMC_SEARCH";

/// A search service.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Engine {
    /// The Brave Search API.
    Brave,
    /// Tavily's search API, which also writes a short answer.
    Tavily,
    /// Exa's search API, or without a key its public MCP server.
    Exa,
    /// Perplexity's Search API.
    Perplexity,
    /// Kagi's Search API.
    Kagi,
    /// Parallel's Search API, or without a key its public MCP server.
    Parallel,
    /// Jina's search API.
    Jina,
    /// Firecrawl's search API, which also answers without a key from most
    /// addresses.
    Firecrawl,
    /// TinyFish's search API.
    TinyFish,
    /// Kimi Code's search API.
    Kimi,
    /// DeepSeek's search API.
    DeepSeek,
    /// Synthetic's search API.
    Synthetic,
    /// Ollama's web search API.
    Ollama,
    /// Keenable's search API, or without a key its public endpoint.
    Keenable,
    /// A SearXNG instance the user runs, with JSON output turned on.
    Searxng,
    /// DuckDuckGo's HTML results page, which needs no key.
    DuckDuckGo,
}

impl Engine {
    /// Every service, in the order they are tried with their key, or for
    /// SearXNG its address, when the user names none.
    pub(super) const ALL: [Engine; 16] = [
        Engine::Brave,
        Engine::Tavily,
        Engine::Exa,
        Engine::Perplexity,
        Engine::Kagi,
        Engine::Parallel,
        Engine::Jina,
        Engine::Firecrawl,
        Engine::TinyFish,
        Engine::Kimi,
        Engine::DeepSeek,
        Engine::Synthetic,
        Engine::Ollama,
        Engine::Keenable,
        Engine::Searxng,
        Engine::DuckDuckGo,
    ];

    /// The services that answer without a key, in the order they are tried
    /// after the services that have one, when the user names none.
    pub(super) const OPEN: [Engine; 5] = [
        Engine::Parallel,
        Engine::Exa,
        Engine::Keenable,
        Engine::Firecrawl,
        Engine::DuckDuckGo,
    ];

    /// The description of the service, from its module.
    pub(super) fn spec(self) -> &'static Spec {
        match self {
            Engine::Brave => &engines::brave::SPEC,
            Engine::Tavily => &engines::tavily::SPEC,
            Engine::Exa => &engines::exa::SPEC,
            Engine::Perplexity => &engines::perplexity::SPEC,
            Engine::Kagi => &engines::kagi::SPEC,
            Engine::Parallel => &engines::parallel::SPEC,
            Engine::Jina => &engines::jina::SPEC,
            Engine::Firecrawl => &engines::firecrawl::SPEC,
            Engine::TinyFish => &engines::tinyfish::SPEC,
            Engine::Kimi => &engines::kimi::SPEC,
            Engine::DeepSeek => &engines::deepseek::SPEC,
            Engine::Synthetic => &engines::synthetic::SPEC,
            Engine::Ollama => &engines::ollama::SPEC,
            Engine::Keenable => &engines::keenable::SPEC,
            Engine::Searxng => &engines::searxng::SPEC,
            Engine::DuckDuckGo => &engines::duckduckgo::SPEC,
        }
    }

    /// The name the service goes by in `OMC_SEARCH` and in results.
    pub(super) fn name(self) -> &'static str {
        self.spec().name
    }

    /// The variables the service's key, or for SearXNG its address, is read
    /// from, in order; none for a service that never takes a key.
    pub(super) fn variables(self) -> &'static [&'static str] {
        self.spec().variables
    }

    /// The service called `name`, or by one of its other names, ignoring case
    /// and surrounding spaces.
    pub(super) fn named(name: &str) -> Option<Engine> {
        let name = name.trim().to_ascii_lowercase();
        Engine::ALL.into_iter().find(|engine| {
            let spec = engine.spec();
            spec.name == name || spec.aliases.contains(&name.as_str())
        })
    }
}

/// Builds the request asking a backend, at the parsed address of its base,
/// for a number of results for a query, limited to a recency when the service
/// can be.
pub(super) type Builder =
    fn(&Backend, reqwest::Url, &str, usize, Option<Recency>) -> Result<Call, String>;

/// What a service is and how it is asked, as its module describes it.
pub(super) struct Spec {
    /// The name the service goes by in `OMC_SEARCH` and in results.
    pub(super) name: &'static str,
    /// Other names `OMC_SEARCH` accepts for it, in lowercase.
    pub(super) aliases: &'static [&'static str],
    /// The variables its key, or for SearXNG its address, is read from, in
    /// order; empty for a service that never takes a key.
    pub(super) variables: &'static [&'static str],
    /// Where requests signed with a key go; empty for SearXNG, whose address
    /// the user gives.
    pub(super) base: &'static str,
    /// Where requests without a key go, for a service that answers without
    /// one; `None` for a service that needs a key.
    pub(super) open_base: Option<&'static str>,
    /// Builds the request asking the backend, at the parsed address of its
    /// base, for a number of results for the query, limited to the recency
    /// when the service can be; the backend's key is empty when it is asked
    /// without one.
    pub(super) call: Builder,
    /// Reads the answer the backend gave into results.
    pub(super) parse: fn(&Backend, &[u8]) -> Result<Found, String>,
}

/// How recent results must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Recency {
    /// From the last day.
    Day,
    /// From the last week.
    Week,
    /// From the last month.
    Month,
    /// From the last year.
    Year,
}

impl Recency {
    /// The recency called `name`: `day`, `week`, `month` or `year`.
    pub(super) fn named(name: &str) -> Option<Recency> {
        match name.trim().to_ascii_lowercase().as_str() {
            "day" => Some(Recency::Day),
            "week" => Some(Recency::Week),
            "month" => Some(Recency::Month),
            "year" => Some(Recency::Year),
            _ => None,
        }
    }

    /// The word for the recency: `day`, `week`, `month` or `year`.
    pub(super) fn word(self) -> &'static str {
        match self {
            Recency::Day => "day",
            Recency::Week => "week",
            Recency::Month => "month",
            Recency::Year => "year",
        }
    }

    /// The date the recency starts at, as `YYYY-MM-DD`.
    pub(super) fn since(self) -> String {
        let days = match self {
            Recency::Day => 1,
            Recency::Week => 7,
            Recency::Month => 31,
            Recency::Year => 365,
        };
        days_ago(days)
    }
}

/// A service ready to be asked: which one, where, and with what key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Backend {
    /// The service.
    pub(super) engine: Engine,
    /// Where requests go.
    pub(super) base: String,
    /// The key requests are signed with; empty when the service is asked
    /// without one.
    pub(super) key: String,
}

/// The services a search may ask, in order, and the ones the user named that
/// cannot be asked, with why.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Plan {
    /// The services to ask, in order, until one answers.
    pub(super) backends: Vec<Backend>,
    /// Services named in `OMC_SEARCH` that are not set up, or names it holds
    /// that are no service, each with the reason.
    pub(super) skipped: Vec<String>,
}

impl Plan {
    /// The plan the environment, read through `get`, sets up.
    ///
    /// `OMC_SEARCH` names the services to try, in order; a named service
    /// without its key is asked without one when it can be. Without it every
    /// service that has its key, or for SearXNG its address, is tried in the
    /// order of [`Engine::ALL`], then the services of [`Engine::OPEN`] not
    /// already asked with a key.
    pub(super) fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| get(name).map(|value| value.trim().to_owned());
        let found = |engine: Engine| {
            engine
                .variables()
                .iter()
                .find_map(|variable| value(variable).filter(|value| !value.is_empty()))
        };
        let mut plan = Plan::default();
        let Some(order) = value(ORDER_VARIABLE).filter(|order| !order.is_empty()) else {
            for engine in Engine::ALL {
                if let Some(backend) = found(engine).and_then(|key| Backend::keyed(engine, key)) {
                    plan.add(backend);
                }
            }
            for engine in Engine::OPEN {
                let asked = plan.backends.iter().any(|backend| backend.engine == engine);
                if let Some(backend) = Backend::open(engine).filter(|_| !asked) {
                    plan.add(backend);
                }
            }
            return plan;
        };
        for name in order
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
        {
            let Some(engine) = Engine::named(name) else {
                plan.skipped.push(format!(
                    "{name} is not a search service {ORDER_VARIABLE} knows"
                ));
                continue;
            };
            let backend = found(engine)
                .and_then(|key| Backend::keyed(engine, key))
                .or_else(|| Backend::open(engine));
            match backend {
                Some(backend) => plan.add(backend),
                None => plan.skipped.push(format!(
                    "{} needs {}",
                    engine.name(),
                    engine.variables().join(" or ")
                )),
            }
        }
        plan
    }

    /// Adds `backend` unless the plan already asks it.
    fn add(&mut self, backend: Backend) {
        if !self.backends.contains(&backend) {
            self.backends.push(backend);
        }
    }
}

/// One result of a search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Hit {
    /// The page's title.
    pub(super) title: String,
    /// The page's address.
    pub(super) url: String,
    /// What the page says about the query, as plain text; may be empty.
    pub(super) snippet: String,
    /// When the page was published or last changed, as the service gives it.
    pub(super) date: Option<String>,
}

/// What a service answered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Found {
    /// A short written answer, from services that write one.
    pub(super) answer: Option<String>,
    /// The results, best first.
    pub(super) hits: Vec<Hit>,
}

/// The HTTP request that asks a service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Call {
    /// Whether the request is a POST rather than a GET.
    pub(super) post: bool,
    /// Where it goes, with its query.
    pub(super) url: reqwest::Url,
    /// Its headers.
    pub(super) headers: Vec<(&'static str, String)>,
    /// Its body and that body's type, for a POST.
    pub(super) body: Option<(&'static str, String)>,
}

impl Call {
    /// A GET of `url` with `headers`.
    pub(super) fn get(url: reqwest::Url, headers: Vec<(&'static str, String)>) -> Call {
        Call {
            post: false,
            url,
            headers,
            body: None,
        }
    }

    /// A POST of the JSON `body` to `url` with `headers`.
    pub(super) fn post_json(
        url: reqwest::Url,
        headers: Vec<(&'static str, String)>,
        body: &Value,
    ) -> Call {
        Call {
            post: true,
            url,
            headers,
            body: Some(("application/json", body.to_string())),
        }
    }
}

impl Backend {
    /// The backend asking `engine` signed with `key`, or for SearXNG at the
    /// address `key`; `None` for a service that never takes a key.
    fn keyed(engine: Engine, key: String) -> Option<Backend> {
        if engine.variables().is_empty() {
            return None;
        }
        if engine == Engine::Searxng {
            return Some(Backend {
                engine,
                base: format!("{}/search", key.trim_end_matches('/')),
                key: String::new(),
            });
        }
        Some(Backend {
            engine,
            base: engine.spec().base.to_owned(),
            key,
        })
    }

    /// The backend asking `engine` without a key; `None` for a service that
    /// needs one.
    fn open(engine: Engine) -> Option<Backend> {
        engine.spec().open_base.map(|base| Backend {
            engine,
            base: base.to_owned(),
            key: String::new(),
        })
    }

    /// The `Authorization` header signing a request with the key as a bearer
    /// token.
    pub(super) fn bearer(&self) -> (&'static str, String) {
        ("Authorization", format!("Bearer {}", self.key))
    }

    /// The request that asks this service for `limit` results for `query`,
    /// limited to `recency` when the service can be.
    ///
    /// # Errors
    ///
    /// Returns the reason when the service's address is not a URL, or its
    /// module cannot build the request.
    pub(super) fn call(
        &self,
        query: &str,
        limit: usize,
        recency: Option<Recency>,
    ) -> Result<Call, String> {
        let url = reqwest::Url::parse(&self.base)
            .map_err(|error| format!("{} is not a URL: {error}", self.base))?;
        (self.engine.spec().call)(self, url, query, limit, recency)
    }

    /// Reads the service's answer `body` into results, at most `limit`, and
    /// drops results without an address.
    ///
    /// # Errors
    ///
    /// Returns the reason when the answer is not what the service sends, or
    /// says that the search was refused.
    pub(super) fn parse(&self, body: &[u8], limit: usize) -> Result<Found, String> {
        let mut found = (self.engine.spec().parse)(self, body)?;
        found.hits.retain(|hit| !hit.url.is_empty());
        found.hits.truncate(limit);
        Ok(found)
    }
}

/// The date `days` days before now, as `YYYY-MM-DD`.
fn days_ago(days: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    civil_date(now / 86_400 - days.min(now / 86_400))
}

/// The calendar date `days` days after 1970-01-01, as `YYYY-MM-DD`.
fn civil_date(days: u64) -> String {
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The answer `body` read as JSON.
///
/// # Errors
///
/// Returns the reason when it is not JSON.
pub(super) fn json(body: &[u8]) -> Result<Value, String> {
    serde_json::from_slice(body).map_err(|error| format!("the answer is not JSON: {error}"))
}

/// The items of the JSON array `value`; none when it is not an array.
pub(super) fn list(value: &Value) -> Vec<Value> {
    value.as_array().cloned().unwrap_or_default()
}

/// The results in the JSON array `items`, each read with [`hit`] from its
/// `title` and `url` and the snippet and date keys given.
pub(super) fn hits(items: &Value, snippets: &[&str], dates: &[&str]) -> Vec<Hit> {
    list(items)
        .iter()
        .map(|item| hit(item, "title", "url", snippets, dates))
        .collect()
}

/// The non-empty text of the string `value`, trimmed.
pub(super) fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The result in the JSON object `item`, with its title, address, snippet and
/// date under the keys given; the first snippet and date key with text wins.
pub(super) fn hit(item: &Value, title: &str, url: &str, snippets: &[&str], dates: &[&str]) -> Hit {
    Hit {
        title: plain(item[title].as_str().unwrap_or_default()),
        url: item[url].as_str().unwrap_or_default().trim().to_owned(),
        snippet: snippets
            .iter()
            .find_map(|key| text(&item[*key]))
            .map(|snippet| plain(&snippet))
            .unwrap_or_default(),
        date: dates.iter().find_map(|key| text(&item[*key])),
    }
}

/// `text` as plain text: tags such as the `<strong>` services mark matches
/// with taken out, entities decoded and whitespace collapsed.
pub(super) fn plain(text: &str) -> String {
    if !text.contains(['<', '&']) {
        return html::collapse(text);
    }
    html::parse(text)
        .map(|tree| html::collapse(&html::text_of(&tree)))
        .unwrap_or_else(|| html::collapse(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The environment holding `pairs`.
    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect();
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        }
    }

    /// The names of the services a plan asks, in order.
    fn names(plan: &Plan) -> Vec<&'static str> {
        plan.backends
            .iter()
            .map(|backend| backend.engine.name())
            .collect()
    }

    /// Without `OMC_SEARCH`, every service with a key is asked in the default
    /// order, then the services that answer without one.
    #[test]
    fn plans_from_keys() {
        let plan = Plan::from_env(env(&[
            ("EXA_API_KEY", "e"),
            ("BRAVE_SEARCH_API_KEY", "b"),
            ("SEARXNG_URL", "http://localhost:8888/"),
        ]));
        assert_eq!(
            names(&plan),
            [
                "brave",
                "exa",
                "searxng",
                "parallel",
                "keenable",
                "firecrawl",
                "duckduckgo"
            ]
        );
        assert_eq!(plan.backends[0].key, "b");
        assert_eq!(plan.backends[2].base, "http://localhost:8888/search");
        assert!(plan.skipped.is_empty());
        assert_eq!(
            names(&Plan::from_env(env(&[]))),
            ["parallel", "exa", "keenable", "firecrawl", "duckduckgo"]
        );
    }

    /// `OMC_SEARCH` picks and orders the services and says which it cannot use.
    #[test]
    fn plans_from_the_order() {
        let plan = Plan::from_env(env(&[
            ("OMC_SEARCH", "tavily, ddg, brave, bing"),
            ("TAVILY_API_KEY", "t"),
        ]));
        assert_eq!(names(&plan), ["tavily", "duckduckgo"]);
        assert_eq!(
            plan.skipped,
            [
                "brave needs BRAVE_API_KEY or BRAVE_SEARCH_API_KEY",
                "bing is not a search service OMC_SEARCH knows"
            ]
        );
        let none = Plan::from_env(env(&[("OMC_SEARCH", "kagi")]));
        assert!(none.backends.is_empty());
    }

    /// The backend for `engine` with the key `k` at its usual address.
    fn backend(engine: Engine) -> Backend {
        Backend {
            engine,
            base: match engine {
                Engine::Searxng => "https://searx.example/search".to_owned(),
                other => other.spec().base.to_owned(),
            },
            key: "k".to_owned(),
        }
    }

    /// Each service is asked the way its documentation says.
    #[test]
    fn builds_requests() {
        let brave = backend(Engine::Brave)
            .call("rust async", 30, Some(Recency::Week))
            .unwrap();
        assert!(!brave.post);
        assert_eq!(
            brave.url.as_str(),
            "https://api.search.brave.com/res/v1/web/search?q=rust+async&count=20&freshness=pw"
        );
        assert!(
            brave
                .headers
                .contains(&("X-Subscription-Token", "k".to_owned()))
        );
        let tavily = backend(Engine::Tavily)
            .call("q", 5, Some(Recency::Day))
            .unwrap();
        let body: Value = serde_json::from_str(&tavily.body.unwrap().1).unwrap();
        assert_eq!(body["include_answer"], "basic");
        assert_eq!(body["time_range"], "day");
        assert_eq!(body["max_results"], 5);
        assert!(
            tavily
                .headers
                .contains(&("Authorization", "Bearer k".to_owned()))
        );
        let exa = backend(Engine::Exa)
            .call("q", 5, Some(Recency::Year))
            .unwrap();
        let body: Value = serde_json::from_str(&exa.body.unwrap().1).unwrap();
        assert_eq!(body["numResults"], 5);
        assert!(
            body["startPublishedDate"]
                .as_str()
                .unwrap()
                .ends_with("T00:00:00.000Z")
        );
        assert!(exa.headers.contains(&("x-api-key", "k".to_owned())));
        let perplexity = backend(Engine::Perplexity)
            .call("q", 50, Some(Recency::Month))
            .unwrap();
        let body: Value = serde_json::from_str(&perplexity.body.unwrap().1).unwrap();
        assert_eq!(body["max_results"], 20);
        assert_eq!(body["search_recency_filter"], "month");
        let kagi = backend(Engine::Kagi)
            .call("q", 5, Some(Recency::Week))
            .unwrap();
        assert_eq!(kagi.url.as_str(), "https://kagi.com/api/v1/search");
        let body: Value = serde_json::from_str(&kagi.body.unwrap().1).unwrap();
        assert_eq!(body["lens"]["time_relative"], "week");
        let searx = backend(Engine::Searxng)
            .call("a b", 5, Some(Recency::Week))
            .unwrap();
        assert_eq!(
            searx.url.as_str(),
            "https://searx.example/search?q=a+b&format=json&time_range=month"
        );
        let ddg = backend(Engine::DuckDuckGo)
            .call("a&b", 5, Some(Recency::Year))
            .unwrap();
        assert!(ddg.post);
        assert_eq!(
            ddg.body,
            Some((
                "application/x-www-form-urlencoded",
                "q=a%26b&df=y".to_owned()
            ))
        );
    }

    /// Each service's recorded answer reads into results.
    #[test]
    fn parses_answers() {
        let parse = |engine, body: &str| backend(engine).parse(body.as_bytes(), 10).unwrap();
        let brave = parse(Engine::Brave, include_str!("fixtures/brave.json"));
        assert_eq!(brave.hits.len(), 2);
        assert_eq!(brave.hits[0].title, "Tokio - An asynchronous Rust runtime");
        assert_eq!(
            brave.hits[0].snippet,
            "Tokio is an asynchronous runtime for the Rust programming language & more."
        );
        assert_eq!(brave.hits[1].date.as_deref(), Some("2026-01-04T00:00:00"));
        let tavily = parse(Engine::Tavily, include_str!("fixtures/tavily.json"));
        assert_eq!(
            tavily.answer.as_deref(),
            Some("Tokio is the most widely used async runtime for Rust.")
        );
        assert_eq!(tavily.hits[0].url, "https://tokio.rs/");
        let exa = parse(Engine::Exa, include_str!("fixtures/exa.json"));
        assert_eq!(exa.hits[0].snippet, "Tokio is a runtime … It provides I/O");
        assert_eq!(exa.hits[1].snippet, "A summary of smol.");
        let perplexity = parse(Engine::Perplexity, include_str!("fixtures/perplexity.json"));
        assert_eq!(perplexity.hits[0].date.as_deref(), Some("2025-03-02"));
        let kagi = parse(Engine::Kagi, include_str!("fixtures/kagi.json"));
        assert_eq!(kagi.hits.len(), 2);
        assert_eq!(kagi.hits[1].snippet, "");
        let searx = parse(Engine::Searxng, include_str!("fixtures/searxng.json"));
        assert_eq!(
            searx.answer.as_deref(),
            Some("Tokio 1.48\nan answer object")
        );
        assert_eq!(searx.hits[0].title, "tokio - crates.io");
        let refused = backend(Engine::Kagi)
            .parse(
                br#"{"meta":{},"data":null,"error":[{"code":"auth","message":"bad key"}]}"#,
                5,
            )
            .unwrap_err();
        assert!(refused.contains("bad key"), "{refused}");
        assert!(backend(Engine::Brave).parse(b"<html>", 5).is_err());
    }

    /// Dates are counted back from the epoch correctly.
    #[test]
    fn counts_dates() {
        assert_eq!(civil_date(0), "1970-01-01");
        assert_eq!(civil_date(19_782), "2024-02-29");
        assert_eq!(civil_date(20_398), "2025-11-06");
    }
}
