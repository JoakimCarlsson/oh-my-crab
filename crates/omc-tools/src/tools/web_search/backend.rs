//! The search services `web_search` can ask, how each is chosen from the
//! environment, and how each one's request is made and answer read.
//!
//! Every service is asked over HTTPS with its documented API, except
//! DuckDuckGo, which has no search API and is read from its HTML results page.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::duckduckgo;
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
    /// Exa's search API.
    Exa,
    /// Perplexity's Search API.
    Perplexity,
    /// Kagi's Search API.
    Kagi,
    /// A SearXNG instance the user runs, with JSON output turned on.
    Searxng,
    /// DuckDuckGo's HTML results page, which needs no key.
    DuckDuckGo,
}

impl Engine {
    /// Every service, in the order they are tried when the user names none.
    pub(super) const ALL: [Engine; 7] = [
        Engine::Brave,
        Engine::Tavily,
        Engine::Exa,
        Engine::Perplexity,
        Engine::Kagi,
        Engine::Searxng,
        Engine::DuckDuckGo,
    ];

    /// The name the service goes by in `OMC_SEARCH` and in results.
    pub(super) fn name(self) -> &'static str {
        match self {
            Engine::Brave => "brave",
            Engine::Tavily => "tavily",
            Engine::Exa => "exa",
            Engine::Perplexity => "perplexity",
            Engine::Kagi => "kagi",
            Engine::Searxng => "searxng",
            Engine::DuckDuckGo => "duckduckgo",
        }
    }

    /// The variables the service's key, or for SearXNG its address, is read
    /// from, in order; none for a service that needs nothing.
    pub(super) fn variables(self) -> &'static [&'static str] {
        match self {
            Engine::Brave => &["BRAVE_API_KEY", "BRAVE_SEARCH_API_KEY"],
            Engine::Tavily => &["TAVILY_API_KEY"],
            Engine::Exa => &["EXA_API_KEY"],
            Engine::Perplexity => &["PERPLEXITY_API_KEY", "PPLX_API_KEY"],
            Engine::Kagi => &["KAGI_API_KEY"],
            Engine::Searxng => &["SEARXNG_URL", "OMC_SEARXNG_URL"],
            Engine::DuckDuckGo => &[],
        }
    }

    /// The address requests go to, when the user did not give one.
    fn default_base(self) -> &'static str {
        match self {
            Engine::Brave => "https://api.search.brave.com/res/v1/web/search",
            Engine::Tavily => "https://api.tavily.com/search",
            Engine::Exa => "https://api.exa.ai/search",
            Engine::Perplexity => "https://api.perplexity.ai/search",
            Engine::Kagi => "https://kagi.com/api/v1/search",
            Engine::Searxng => "",
            Engine::DuckDuckGo => "https://html.duckduckgo.com/html/",
        }
    }

    /// The service called `name`, ignoring case and surrounding spaces.
    pub(super) fn named(name: &str) -> Option<Engine> {
        let name = name.trim().to_ascii_lowercase();
        let name = match name.as_str() {
            "ddg" => "duckduckgo",
            "searx" => "searxng",
            other => other,
        };
        Engine::ALL.into_iter().find(|engine| engine.name() == name)
    }
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
    fn word(self) -> &'static str {
        match self {
            Recency::Day => "day",
            Recency::Week => "week",
            Recency::Month => "month",
            Recency::Year => "year",
        }
    }

    /// The relative time Kagi filters by for the recency; Kagi has none for a
    /// year.
    fn kagi(self) -> Option<&'static str> {
        match self {
            Recency::Year => None,
            other => Some(other.word()),
        }
    }

    /// How many days back the recency reaches.
    fn days(self) -> u64 {
        match self {
            Recency::Day => 1,
            Recency::Week => 7,
            Recency::Month => 31,
            Recency::Year => 365,
        }
    }
}

/// A service ready to be asked: which one, where, and with what key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Backend {
    /// The service.
    pub(super) engine: Engine,
    /// Where requests go.
    pub(super) base: String,
    /// The key requests are signed with; empty for services that need none.
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
    /// `OMC_SEARCH` names the services to try, in order; without it every
    /// service that has its key, or for SearXNG its address, is tried in the
    /// order of [`Engine::ALL`], DuckDuckGo last.
    pub(super) fn from_env(get: impl Fn(&str) -> Option<String>) -> Self {
        let value = |name: &str| get(name).map(|value| value.trim().to_owned());
        let order = value(ORDER_VARIABLE).filter(|order| !order.is_empty());
        let mut plan = Plan::default();
        let named: Vec<(String, Option<Engine>)> = match &order {
            Some(order) => order
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map(|name| (name.to_owned(), Engine::named(name)))
                .collect(),
            None => Engine::ALL
                .into_iter()
                .map(|engine| (engine.name().to_owned(), Some(engine)))
                .collect(),
        };
        for (name, engine) in named {
            let Some(engine) = engine else {
                plan.skipped.push(format!(
                    "{name} is not a search service {ORDER_VARIABLE} knows"
                ));
                continue;
            };
            let found = engine
                .variables()
                .iter()
                .find_map(|variable| value(variable).filter(|value| !value.is_empty()));
            let backend = match (engine, found) {
                (Engine::DuckDuckGo, _) => Some(Backend {
                    engine,
                    base: engine.default_base().to_owned(),
                    key: String::new(),
                }),
                (Engine::Searxng, Some(address)) => Some(Backend {
                    engine,
                    base: format!("{}/search", address.trim_end_matches('/')),
                    key: String::new(),
                }),
                (_, Some(key)) => Some(Backend {
                    engine,
                    base: engine.default_base().to_owned(),
                    key,
                }),
                (_, None) => None,
            };
            match backend {
                Some(backend) if !plan.backends.contains(&backend) => plan.backends.push(backend),
                Some(_) => {}
                None if order.is_some() => plan.skipped.push(format!(
                    "{} needs {}",
                    engine.name(),
                    engine.variables().join(" or ")
                )),
                None => {}
            }
        }
        plan
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

/// The date `days` days before now, as `YYYY-MM-DD`.
pub(super) fn days_ago(days: u64) -> String {
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

impl Backend {
    /// The request that asks this service for `limit` results for `query`,
    /// limited to `recency` when the service can be.
    ///
    /// # Errors
    ///
    /// Returns the reason when the service's address is not a URL.
    pub(super) fn call(
        &self,
        query: &str,
        limit: usize,
        recency: Option<Recency>,
    ) -> Result<Call, String> {
        let mut url = reqwest::Url::parse(&self.base)
            .map_err(|error| format!("{} is not a URL: {error}", self.base))?;
        let bearer = || vec![("Authorization", format!("Bearer {}", self.key))];
        let json_body = |body: Value| Some(("application/json", body.to_string()));
        let call = match self.engine {
            Engine::Brave => {
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
                drop(pairs);
                Call {
                    post: false,
                    url,
                    headers: vec![
                        ("X-Subscription-Token", self.key.clone()),
                        ("Accept", "application/json".to_owned()),
                    ],
                    body: None,
                }
            }
            Engine::Tavily => {
                let mut body = json!({
                    "query": query,
                    "max_results": limit.min(20),
                    "search_depth": "basic",
                    "include_answer": "basic",
                });
                if let Some(recency) = recency {
                    body["time_range"] = json!(recency.word());
                }
                Call {
                    post: true,
                    url,
                    headers: bearer(),
                    body: json_body(body),
                }
            }
            Engine::Exa => {
                let mut body = json!({
                    "query": query,
                    "numResults": limit.min(100),
                    "type": "auto",
                    "contents": { "highlights": { "maxCharacters": 600 } },
                });
                if let Some(recency) = recency {
                    body["startPublishedDate"] =
                        json!(format!("{}T00:00:00.000Z", days_ago(recency.days())));
                }
                Call {
                    post: true,
                    url,
                    headers: vec![("x-api-key", self.key.clone())],
                    body: json_body(body),
                }
            }
            Engine::Perplexity => {
                let mut body = json!({ "query": query, "max_results": limit.min(20) });
                if let Some(recency) = recency {
                    body["search_recency_filter"] = json!(recency.word());
                }
                Call {
                    post: true,
                    url,
                    headers: bearer(),
                    body: json_body(body),
                }
            }
            Engine::Kagi => {
                let mut body = json!({ "query": query, "limit": limit });
                if let Some(relative) = recency.and_then(Recency::kagi) {
                    body["lens"] = json!({ "time_relative": relative });
                }
                Call {
                    post: true,
                    url,
                    headers: bearer(),
                    body: json_body(body),
                }
            }
            Engine::Searxng => {
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
                drop(pairs);
                Call {
                    post: false,
                    url,
                    headers: vec![("Accept", "application/json".to_owned())],
                    body: None,
                }
            }
            Engine::DuckDuckGo => {
                let mut form = reqwest::Url::parse("http://form.invalid/")
                    .map_err(|error| error.to_string())?;
                let mut pairs = form.query_pairs_mut();
                pairs.append_pair("q", query);
                if let Some(recency) = recency {
                    pairs.append_pair("df", &recency.word()[..1]);
                }
                drop(pairs);
                Call {
                    post: true,
                    url,
                    headers: vec![("Accept", "text/html".to_owned())],
                    body: Some((
                        "application/x-www-form-urlencoded",
                        form.query().unwrap_or_default().to_owned(),
                    )),
                }
            }
        };
        Ok(call)
    }

    /// Reads the service's answer `body` into results, at most `limit`.
    ///
    /// # Errors
    ///
    /// Returns the reason when the answer is not what the service sends, or
    /// says that the search was refused.
    pub(super) fn parse(&self, body: &[u8], limit: usize) -> Result<Found, String> {
        if self.engine == Engine::DuckDuckGo {
            let mut found = duckduckgo::parse(&String::from_utf8_lossy(body))?;
            found.hits.truncate(limit);
            return Ok(found);
        }
        let value: Value = serde_json::from_slice(body)
            .map_err(|error| format!("the answer is not JSON: {error}"))?;
        let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
        let (answer, hits) = match self.engine {
            Engine::Brave => (
                None,
                list(&value["web"]["results"])
                    .iter()
                    .map(|item| hit(item, "title", "url", &["description"], &["page_age", "age"]))
                    .collect(),
            ),
            Engine::Tavily => (
                text(&value["answer"]),
                list(&value["results"])
                    .iter()
                    .map(|item| hit(item, "title", "url", &["content"], &["published_date"]))
                    .collect(),
            ),
            Engine::Exa => (
                None,
                list(&value["results"])
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
            ),
            Engine::Perplexity => (
                None,
                list(&value["results"])
                    .iter()
                    .map(|item| {
                        hit(
                            item,
                            "title",
                            "url",
                            &["snippet"],
                            &["date", "last_updated"],
                        )
                    })
                    .collect(),
            ),
            Engine::Kagi => {
                if let Some(error) = value["error"].as_array().and_then(|errors| errors.first()) {
                    return Err(format!(
                        "Kagi refused the search: {}",
                        text(&error["message"])
                            .or_else(|| text(&error["code"]))
                            .unwrap_or_default()
                    ));
                }
                (
                    None,
                    list(&value["data"]["search"])
                        .iter()
                        .map(|item| hit(item, "title", "url", &["snippet"], &["time"]))
                        .collect(),
                )
            }
            Engine::Searxng => (
                list(&value["answers"])
                    .iter()
                    .filter_map(|answer| text(answer).or_else(|| text(&answer["answer"])))
                    .reduce(|all, next| format!("{all}\n{next}")),
                list(&value["results"])
                    .iter()
                    .map(|item| hit(item, "title", "url", &["content"], &["publishedDate"]))
                    .collect(),
            ),
            Engine::DuckDuckGo => (None, Vec::new()),
        };
        let mut hits: Vec<Hit> = hits
            .into_iter()
            .filter(|hit: &Hit| !hit.url.is_empty())
            .collect();
        hits.truncate(limit);
        Ok(Found { answer, hits })
    }
}

/// The non-empty text of the string `value`, trimmed.
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// The result in the JSON object `item`, with its title, address, snippet and
/// date under the keys given; the first snippet and date key with text wins.
fn hit(item: &Value, title: &str, url: &str, snippets: &[&str], dates: &[&str]) -> Hit {
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
    /// order, DuckDuckGo last.
    #[test]
    fn plans_from_keys() {
        let plan = Plan::from_env(env(&[
            ("EXA_API_KEY", "e"),
            ("BRAVE_SEARCH_API_KEY", "b"),
            ("SEARXNG_URL", "http://localhost:8888/"),
        ]));
        assert_eq!(names(&plan), ["brave", "exa", "searxng", "duckduckgo"]);
        assert_eq!(plan.backends[0].key, "b");
        assert_eq!(plan.backends[2].base, "http://localhost:8888/search");
        assert!(plan.skipped.is_empty());
        assert_eq!(names(&Plan::from_env(env(&[]))), ["duckduckgo"]);
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
                other => other.default_base().to_owned(),
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
