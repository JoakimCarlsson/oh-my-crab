//! What each known model can do: context window, output limit and thinking efforts.

use crate::{Provider, Wire};

use crate::Effort;

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
