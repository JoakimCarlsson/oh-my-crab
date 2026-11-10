//! How much of a subscription's allowance an account has used: the windows and quotas a provider meters.

/// What a limit is counted in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitUnit {
    /// A share of the allowance, from 0 to 100, when the provider reports no absolute count.
    Percent,
    /// Requests, messages or other metered interactions.
    Requests,
    /// Tokens.
    Tokens,
    /// Credits the provider sells its own way.
    Credits,
    /// United States dollars, as for spend on top of a plan.
    Usd,
    /// The provider reports a share or a count without saying what is counted.
    #[default]
    Unknown,
}

/// One window or quota of an account's allowance, such as Claude's five hours or Copilot's month.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limit {
    /// The name a client shows, such as `5 hour` or `Premium requests`.
    pub label: String,
    /// A short stable name for the length of the window, such as `5h`, `7d` or `monthly`, which clients match on; nothing when the quota has no window.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<String>,
    /// What is counted, in [`Limit::used`] and [`Limit::limit`].
    #[serde(default)]
    pub unit: LimitUnit,
    /// How much of the allowance is used, from 0 to 1; nothing when the provider reports neither a share nor a count to work it out from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used_fraction: Option<f64>,
    /// How much of the allowance is used, in [`Limit::unit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub used: Option<f64>,
    /// How much the allowance takes in all, in [`Limit::unit`]; nothing when it is not capped or not reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<f64>,
    /// When the window resets, as Unix time in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at_ms: Option<u64>,
    /// Whether the allowance has no cap, so that a share used means nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unlimited: bool,
}

impl Limit {
    /// A limit with the label `label` and nothing else known.
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            window: None,
            unit: LimitUnit::Unknown,
            used_fraction: None,
            used: None,
            limit: None,
            resets_at_ms: None,
            unlimited: false,
        }
    }

    /// Whether the allowance is spent: the whole of it is used, and it is capped.
    pub fn exhausted(&self) -> bool {
        !self.unlimited && self.used_fraction.is_some_and(|fraction| fraction >= 1.0)
    }
}

/// What one account of one provider has used of its allowance, as the provider reported it when asked.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Limits {
    /// The id of the provider, such as `claude`.
    pub provider: String,
    /// The plan the account is on, such as `pro` or `max`, when the provider said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// When the provider was asked, as Unix time in milliseconds.
    pub fetched_at_ms: u64,
    /// The account's windows and quotas, in the order a client should show them.
    pub limits: Vec<Limit>,
}
