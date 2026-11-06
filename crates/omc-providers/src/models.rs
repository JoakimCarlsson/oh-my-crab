//! What each known model can do and costs: context window, output limit, thinking efforts, prices and cache lifetimes.

use std::time::Duration;

use crate::wires::anthropic_messages;
use crate::{Effort, Provider, Tokens, Wire};

/// A model's generation as `major.minor`, parsed from its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Revision {
    /// The leading version number.
    pub major: u32,
    /// The number after the point, zero when the id names none.
    pub minor: u32,
}

impl Revision {
    /// Builds a revision from its two parts.
    const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

/// The families of Claude models that differ in what they can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    /// Opus models.
    Opus,
    /// Sonnet models.
    Sonnet,
    /// Haiku models.
    Haiku,
    /// Any other family, such as Fable or Mythos.
    Other,
}

/// A Claude model id broken into family and revision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ClaudeModel {
    /// The model's family.
    pub family: Family,
    /// The model's generation, when the id carries one.
    pub revision: Option<Revision>,
}

/// Parses `id` into family and revision.
///
/// Two-digit-or-shorter number segments make up the revision, so a trailing
/// date such as `20250514` is ignored and `claude-3-7-sonnet` reads as 3.7.
pub(crate) fn parse_claude(id: &str) -> ClaudeModel {
    let mut family = Family::Other;
    let mut numbers = Vec::new();
    for part in id.split('-').skip_while(|part| *part == "claude") {
        match part {
            "opus" => family = Family::Opus,
            "sonnet" => family = Family::Sonnet,
            "haiku" => family = Family::Haiku,
            _ => {
                if let Ok(number) = part.parse::<u32>()
                    && part.len() <= 2
                    && numbers.len() < 2
                {
                    numbers.push(number);
                }
            }
        }
    }
    let revision = numbers
        .first()
        .map(|major| Revision::new(*major, numbers.get(1).copied().unwrap_or(0)));
    ClaudeModel { family, revision }
}

/// Parses the `gpt-5.4`, `gpt-6.1-sol` or `gpt-5.1-codex-mini` generation out of `id`.
pub(crate) fn parse_gpt(id: &str) -> Option<Revision> {
    let rest = id.strip_prefix("gpt-").or_else(|| id.strip_prefix("gpt"))?;
    let digits = |text: &str| -> Option<(u32, usize)> {
        let end = text
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(text.len());
        text[..end].parse().ok().map(|number| (number, end))
    };
    let (major, used) = digits(rest)?;
    let minor = rest[used..]
        .strip_prefix('.')
        .and_then(digits)
        .map_or(0, |(minor, _)| minor);
    Some(Revision::new(major, minor))
}

/// How a Claude model is asked to think.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThinkingMode {
    /// The model cannot think.
    None,
    /// A token budget, with no effort setting.
    Budget,
    /// A token budget plus `output_config.effort` (Opus 4.5).
    BudgetEffort,
    /// Adaptive thinking steered by `output_config.effort`.
    Adaptive,
}

/// Picks the thinking mode of a Claude model.
pub(crate) fn claude_thinking_mode(model: &ClaudeModel) -> ThinkingMode {
    let Some(revision) = model.revision else {
        return ThinkingMode::Budget;
    };
    if revision >= Revision::new(4, 6) {
        return ThinkingMode::Adaptive;
    }
    if revision < Revision::new(3, 7) {
        return ThinkingMode::None;
    }
    if model.family == Family::Opus && revision == Revision::new(4, 5) {
        return ThinkingMode::BudgetEffort;
    }
    ThinkingMode::Budget
}

/// Whether the model accepts `thinking.display`, which asks for summarized thinking.
pub(crate) fn claude_supports_display(model: &ClaudeModel) -> bool {
    model
        .revision
        .is_some_and(|revision| revision >= Revision::new(4, 7))
}

/// The most output tokens a request to a Claude model may ask for.
pub(crate) fn claude_output_limit(model: &ClaudeModel) -> u64 {
    let Some(revision) = model.revision else {
        return 64_000;
    };
    let opus = model.family == Family::Opus;
    if revision >= Revision::new(5, 0) || (opus && revision >= Revision::new(4, 6)) {
        128_000
    } else if opus && revision < Revision::new(4, 5) {
        32_000
    } else {
        64_000
    }
}

/// The effort levels a Claude model accepts, lightest first.
pub(crate) fn claude_efforts(id: &str) -> Vec<Effort> {
    let model = parse_claude(id);
    let revision = model.revision;
    match claude_thinking_mode(&model) {
        ThinkingMode::None => Vec::new(),
        ThinkingMode::BudgetEffort => vec![Effort::Low, Effort::Medium, Effort::High],
        ThinkingMode::Budget => vec![Effort::Low, Effort::Medium, Effort::High, Effort::XHigh],
        ThinkingMode::Adaptive => match revision {
            Some(r) if r >= Revision::new(4, 7) => Effort::ALL.to_vec(),
            _ if model.family == Family::Sonnet => vec![Effort::Low, Effort::Medium, Effort::High],
            _ => vec![Effort::Low, Effort::Medium, Effort::High, Effort::Max],
        },
    }
}

/// The context window of a Claude model, in tokens.
pub(crate) fn claude_context_window(id: &str) -> u64 {
    match parse_claude(id).revision {
        Some(revision) if revision >= Revision::new(5, 0) => 1_000_000,
        _ => 200_000,
    }
}

/// The effort levels a Codex model accepts, lightest first.
pub(crate) fn codex_efforts(id: &str) -> Vec<Effort> {
    let Some(revision) = parse_gpt(id) else {
        return Effort::ALL.to_vec();
    };
    if revision >= Revision::new(5, 6) {
        Effort::ALL.to_vec()
    } else if revision >= Revision::new(5, 2) {
        vec![Effort::Low, Effort::Medium, Effort::High, Effort::XHigh]
    } else if revision == Revision::new(5, 1) && id.contains("codex-mini") {
        vec![Effort::Medium, Effort::High]
    } else {
        vec![Effort::Low, Effort::Medium, Effort::High]
    }
}

/// The context window of a Codex model, in tokens.
pub(crate) fn codex_context_window(id: &str) -> u64 {
    let small = id.contains("-mini") || id.contains("-nano");
    match parse_gpt(id) {
        Some(revision) if revision == Revision::new(6, 1) && id.contains("sol") => 500_000,
        Some(revision) if revision >= Revision::new(5, 6) && revision < Revision::new(6, 0) => {
            1_000_000
        }
        Some(revision) if revision == Revision::new(5, 4) && !small => 1_000_000,
        _ => 272_000,
    }
}

/// The context window assumed for a model a server does not describe.
const UNKNOWN_CONTEXT_WINDOW: u64 = 128_000;

/// How many tokens of context `model` of `provider` takes in.
pub fn context_window(provider: Provider, model: &str) -> u64 {
    match provider.wire() {
        Wire::AnthropicMessages => claude_context_window(model),
        Wire::OpenAiResponses => codex_context_window(model),
        Wire::ChatCompletions => UNKNOWN_CONTEXT_WINDOW,
    }
}

/// The effort levels `model` of `provider` accepts, lightest first; none when it does not think.
pub fn efforts(provider: Provider, model: &str) -> Vec<Effort> {
    match provider.wire() {
        Wire::AnthropicMessages => claude_efforts(model),
        Wire::OpenAiResponses => codex_efforts(model),
        Wire::ChatCompletions => Vec::new(),
    }
}

/// How long the provider is asked to keep the prompt cache entries a request writes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Retention {
    /// The provider's default lifetime: five minutes for Claude.
    #[default]
    Short,
    /// An hour, which Claude charges twice the input price to write instead of a quarter more.
    Long,
}

/// What a model charges, in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pricing {
    /// Input that is neither read from nor written to the cache.
    pub input: f64,
    /// Output, thinking included.
    pub output: f64,
    /// Input read from the cache.
    pub cache_read: f64,
    /// Input written to a cache entry with the short lifetime.
    pub cache_write: f64,
    /// Input written to a cache entry with the long lifetime.
    pub cache_write_long: f64,
}

impl Pricing {
    /// The prices of a Claude model that charges `input`, `output` and `cache_read` per million tokens: writes cost a quarter more than input for the short lifetime and twice input for the long one.
    const fn claude(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write: input * 1.25,
            cache_write_long: input * 2.0,
        }
    }

    /// The prices of an OpenAI model that charges `input`, `output` and `cache_read` per million tokens; writing to the cache costs no more than input.
    const fn openai(input: f64, output: f64, cache_read: f64) -> Self {
        Self {
            input,
            output,
            cache_read,
            cache_write: input,
            cache_write_long: input,
        }
    }

    /// The price per million tokens of writing to a cache entry that lives as `retention` says.
    pub fn cache_write_for(&self, retention: Retention) -> f64 {
        match retention {
            Retention::Short => self.cache_write,
            Retention::Long => self.cache_write_long,
        }
    }

    /// What `tokens` cost in US dollars, when the cache entries they wrote live as `retention` says.
    pub fn cost(&self, tokens: &Tokens, retention: Retention) -> f64 {
        let dollars = |count: u64, price: f64| count as f64 * price / 1_000_000.0;
        dollars(tokens.input, self.input)
            + dollars(tokens.output, self.output)
            + dollars(tokens.cache_read, self.cache_read)
            + dollars(tokens.cache_write, self.cache_write_for(retention))
    }
}

/// The list prices of a Claude model, when its family and generation are known.
///
/// Claude Haiku 5.5 is priced for prompts of up to 100,000 tokens; longer ones cost five times as much.
fn claude_pricing(id: &str) -> Option<Pricing> {
    let model = parse_claude(id);
    let revision = model.revision?;
    let at_least = |major, minor| revision >= Revision::new(major, minor);
    Some(match model.family {
        Family::Opus if at_least(5, 5) => Pricing::claude(4.0, 20.0, 0.20),
        Family::Opus if at_least(4, 5) => Pricing::claude(5.0, 25.0, 0.50),
        Family::Opus => Pricing::claude(15.0, 75.0, 1.50),
        Family::Sonnet if at_least(5, 0) => Pricing::claude(2.0, 10.0, 0.20),
        Family::Sonnet => Pricing::claude(3.0, 15.0, 0.30),
        Family::Haiku if at_least(5, 5) => Pricing::claude(0.10, 0.50, 0.01),
        Family::Haiku if at_least(4, 5) => Pricing::claude(1.0, 5.0, 0.10),
        Family::Haiku if at_least(3, 5) => Pricing::claude(0.80, 4.0, 0.08),
        Family::Haiku => Pricing::claude(0.25, 1.25, 0.03),
        Family::Other if !id.contains("fable") && !id.contains("mythos") => return None,
        Family::Other if at_least(5, 1) => Pricing::claude(10.0, 50.0, 0.25),
        Family::Other => Pricing::claude(10.0, 50.0, 1.0),
    })
}

/// The list prices of a Codex model, for the generations whose prices are published.
fn codex_pricing(id: &str) -> Option<Pricing> {
    let revision = parse_gpt(id)?;
    let small = id.contains("-mini");
    if revision == Revision::new(5, 2) && !small {
        Some(Pricing::openai(1.75, 14.0, 0.175))
    } else if revision > Revision::new(5, 1) || id.contains("-nano") {
        None
    } else if small {
        Some(Pricing::openai(0.25, 2.0, 0.025))
    } else {
        Some(Pricing::openai(1.25, 10.0, 0.125))
    }
}

/// What `model` of `provider` charges, when it is known.
pub fn pricing(provider: Provider, model: &str) -> Option<Pricing> {
    match provider.wire() {
        Wire::AnthropicMessages => claude_pricing(model),
        Wire::OpenAiResponses => codex_pricing(model),
        Wire::ChatCompletions => None,
    }
}

/// How long `provider` keeps the prompt cache entries a request to `model` writes when it asks for `retention`; nothing when the provider has no such lifetime or does not say.
///
/// Codex keeps entries for at least five minutes of inactivity and offers no longer lifetime on the wire crab speaks.
pub fn cache_ttl(provider: Provider, _model: &str, retention: Retention) -> Option<Duration> {
    match (provider.wire(), retention) {
        (Wire::AnthropicMessages, Retention::Short) => Some(Duration::from_secs(5 * 60)),
        (Wire::AnthropicMessages, Retention::Long) => Some(Duration::from_secs(60 * 60)),
        (Wire::OpenAiResponses, Retention::Short) => Some(Duration::from_secs(5 * 60)),
        _ => None,
    }
}

/// Whether a request to `model` of `provider` at `effort` can be sent again without asking for output, to keep its cache entries alive, and still read exactly the entries it wrote.
///
/// Only Claude takes such requests. A Claude model that thinks within a token budget cannot: the budget has to stay below the output limit, so a request with no output changes the thinking settings, which the cache is keyed on.
pub fn warmable(provider: Provider, model: &str, effort: Option<Effort>) -> bool {
    provider.wire() == Wire::AnthropicMessages
        && anthropic_messages::thinking_params(model, effort)
            .thinking
            .is_none_or(|thinking| thinking["type"] != "enabled")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Claude ids parse to family and revision, ignoring dates.
    #[test]
    fn parses_claude_ids() {
        let cases = [
            ("claude-opus-5-5", Family::Opus, Revision::new(5, 5)),
            ("claude-sonnet-5-5", Family::Sonnet, Revision::new(5, 5)),
            ("claude-opus-4-20250514", Family::Opus, Revision::new(4, 0)),
            (
                "claude-opus-4-1-20250805",
                Family::Opus,
                Revision::new(4, 1),
            ),
            (
                "claude-3-7-sonnet-20250219",
                Family::Sonnet,
                Revision::new(3, 7),
            ),
            ("claude-fable-5-1", Family::Other, Revision::new(5, 1)),
        ];
        for (id, family, revision) in cases {
            let parsed = parse_claude(id);
            assert_eq!(parsed.family, family, "{id}");
            assert_eq!(parsed.revision, Some(revision), "{id}");
        }
    }

    /// Efforts follow the reference's per-revision ladders.
    #[test]
    fn claude_effort_ladders() {
        assert_eq!(efforts(Provider::Claude, "claude-opus-5-5"), Effort::ALL);
        assert_eq!(efforts(Provider::Claude, "claude-haiku-5-5"), Effort::ALL);
        assert_eq!(
            efforts(Provider::Claude, "claude-opus-4-6"),
            [Effort::Low, Effort::Medium, Effort::High, Effort::Max]
        );
        assert_eq!(
            efforts(Provider::Claude, "claude-sonnet-4-6"),
            [Effort::Low, Effort::Medium, Effort::High]
        );
        assert_eq!(efforts(Provider::Claude, "claude-3-5-haiku-20241022"), []);
        assert!(!efforts(Provider::Claude, "claude-sonnet-4-5-20250929").is_empty());
    }

    /// Thinking modes split at the 4.6 adaptive boundary and Opus 4.5.
    #[test]
    fn claude_modes() {
        let mode = |id| claude_thinking_mode(&parse_claude(id));
        assert_eq!(mode("claude-opus-4-8"), ThinkingMode::Adaptive);
        assert_eq!(mode("claude-opus-4-5-20251101"), ThinkingMode::BudgetEffort);
        assert_eq!(mode("claude-sonnet-4-5-20250929"), ThinkingMode::Budget);
        assert_eq!(mode("claude-3-haiku-20240307"), ThinkingMode::None);
    }

    /// Windows and output limits follow the generation.
    #[test]
    fn claude_limits() {
        assert_eq!(
            context_window(Provider::Claude, "claude-sonnet-5-5"),
            1_000_000
        );
        assert_eq!(context_window(Provider::Claude, "claude-opus-4-5"), 200_000);
        assert_eq!(
            claude_output_limit(&parse_claude("claude-fable-5-1")),
            128_000
        );
        assert_eq!(
            claude_output_limit(&parse_claude("claude-opus-4-1")),
            32_000
        );
    }

    /// Claude prices follow family and generation, and cache writes cost more for the longer lifetime.
    #[test]
    fn claude_prices() {
        let opus = pricing(Provider::Claude, "claude-opus-5-5").unwrap();
        assert_eq!(
            (opus.input, opus.output, opus.cache_read),
            (4.0, 20.0, 0.20)
        );
        assert_eq!(opus.cache_write_for(Retention::Short), 5.0);
        assert_eq!(opus.cache_write_for(Retention::Long), 8.0);
        let input = |id| pricing(Provider::Claude, id).map(|price| price.input);
        assert_eq!(input("claude-opus-4-8"), Some(5.0));
        assert_eq!(input("claude-opus-4-1-20250805"), Some(15.0));
        assert_eq!(input("claude-sonnet-5-5"), Some(2.0));
        assert_eq!(input("claude-sonnet-4-6"), Some(3.0));
        assert_eq!(input("claude-haiku-4-5"), Some(1.0));
        assert_eq!(input("claude-3-5-haiku-20241022"), Some(0.80));
        assert_eq!(input("claude-fable-5-1"), Some(10.0));
        assert_eq!(
            pricing(Provider::Claude, "claude-fable-5")
                .unwrap()
                .cache_read,
            1.0
        );
        assert_eq!(input("claude-unknown-5"), None);
        assert_eq!(input("claude-opus-latest"), None);
    }

    /// Cost adds each kind of token at its price.
    #[test]
    fn costs_add_up() {
        let price = pricing(Provider::Claude, "claude-opus-5-5").unwrap();
        let tokens = Tokens {
            input: 1_000_000,
            output: 100_000,
            cache_read: 1_000_000,
            cache_write: 1_000_000,
        };
        let cost = price.cost(&tokens, Retention::Short);
        assert!((cost - (4.0 + 2.0 + 0.2 + 5.0)).abs() < 1e-9);
        let long = price.cost(&tokens, Retention::Long);
        assert!((long - cost - 3.0).abs() < 1e-9);
    }

    /// Cache lifetimes are known for Claude in both lengths and for Codex in the short one only.
    #[test]
    fn cache_lifetimes() {
        let ttl = |provider, retention| cache_ttl(provider, "m", retention).map(|d| d.as_secs());
        assert_eq!(ttl(Provider::Claude, Retention::Short), Some(300));
        assert_eq!(ttl(Provider::Claude, Retention::Long), Some(3600));
        assert_eq!(ttl(Provider::Codex, Retention::Short), Some(300));
        assert_eq!(ttl(Provider::Codex, Retention::Long), None);
    }

    /// Only Claude requests without a thinking budget can be replayed to warm the cache.
    #[test]
    fn warming_needs_a_replayable_request() {
        assert!(warmable(
            Provider::Claude,
            "claude-opus-5-5",
            Some(Effort::High)
        ));
        assert!(warmable(Provider::Claude, "claude-sonnet-4-5", None));
        assert!(!warmable(
            Provider::Claude,
            "claude-sonnet-4-5",
            Some(Effort::High)
        ));
        assert!(!warmable(Provider::Codex, "gpt-5.5", None));
    }

    /// Codex prices are known for the published generations only.
    #[test]
    fn codex_prices() {
        let input = |id| pricing(Provider::Codex, id).map(|price| price.input);
        assert_eq!(input("gpt-5.1-codex"), Some(1.25));
        assert_eq!(input("gpt-5.1-codex-mini"), Some(0.25));
        assert_eq!(input("gpt-5.2"), Some(1.75));
        assert_eq!(input("gpt-5.5"), None);
        let price = pricing(Provider::Codex, "gpt-5").unwrap();
        assert_eq!(price.cache_write_for(Retention::Long), price.input);
    }

    /// Codex ids parse and map to the reference's ladders and windows.
    #[test]
    fn codex_tables() {
        assert_eq!(parse_gpt("gpt-5.6-sol"), Some(Revision::new(5, 6)));
        assert_eq!(parse_gpt("gpt-6.1-terra"), Some(Revision::new(6, 1)));
        assert_eq!(parse_gpt("gpt-daybreak-blue-latest"), None);
        assert_eq!(efforts(Provider::Codex, "gpt-6-sol"), Effort::ALL);
        assert_eq!(
            efforts(Provider::Codex, "gpt-5.5"),
            [Effort::Low, Effort::Medium, Effort::High, Effort::XHigh]
        );
        assert_eq!(
            efforts(Provider::Codex, "gpt-5.1-codex-mini"),
            [Effort::Medium, Effort::High]
        );
        assert_eq!(context_window(Provider::Codex, "gpt-6.1-sol"), 500_000);
        assert_eq!(context_window(Provider::Codex, "gpt-5.6-luna"), 1_000_000);
        assert_eq!(context_window(Provider::Codex, "gpt-5.4-mini"), 272_000);
        assert_eq!(context_window(Provider::Codex, "gpt-5.5"), 272_000);
    }
}
