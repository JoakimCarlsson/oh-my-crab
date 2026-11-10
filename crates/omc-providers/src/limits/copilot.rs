//! GitHub Copilot: the premium requests of a plan for the month, read from GitHub's Copilot user endpoint.

use omc_auth::Credentials;
use omc_wire::{Limit, LimitUnit};
use serde_json::Value;

use super::{Parsed, counted, get_json, number, parse_time, text};
use crate::error::ProviderError;

/// Where GitHub reports a Copilot account's plan and quotas.
const USER_URL: &str = "https://api.github.com/copilot_internal/user";

/// The quotas a Copilot plan can meter, as the key in the payload and the label shown.
const QUOTAS: [(&str, &str); 3] = [
    ("premium_interactions", "Premium requests"),
    ("chat", "Chat"),
    ("completions", "Completions"),
];

/// Reads the quotas of the Copilot account `credentials` are a login for.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(super) async fn fetch(credentials: &Credentials) -> Result<Option<Parsed>, ProviderError> {
    if credentials.access.is_empty() {
        return Ok(None);
    }
    let headers = vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credentials.access),
        ),
        ("User-Agent".to_owned(), "copilot/1.0.82".to_owned()),
    ];
    let payload = get_json(USER_URL, headers).await?;
    Ok(Some(parse(&payload)))
}

/// The limit one quota snapshot describes, which resets at `resets_at`; nothing when the snapshot holds no count.
fn quota_limit(label: &str, snapshot: &Value, resets_at: Option<u64>) -> Option<Limit> {
    let unlimited = snapshot.get("unlimited").and_then(Value::as_bool) == Some(true);
    let entitlement = snapshot.get("entitlement").and_then(number);
    let remaining = snapshot.get("remaining").and_then(number);
    let percent_remaining = snapshot.get("percent_remaining").and_then(number);
    if !unlimited && entitlement.is_none() && percent_remaining.is_none() {
        return None;
    }
    let used = match (entitlement, remaining) {
        (Some(entitlement), Some(remaining)) => Some((entitlement - remaining).max(0.0)),
        _ => None,
    };
    let fraction = percent_remaining.map(|percent| 1.0 - percent / 100.0);
    let cap = entitlement.filter(|_| !unlimited);
    let mut limit = counted(
        label,
        Some("monthly"),
        LimitUnit::Requests,
        (used, cap),
        fraction.filter(|_| !unlimited),
        resets_at,
    );
    limit.unlimited = unlimited;
    Some(limit)
}

/// What a Copilot user payload holds: the plan, and each quota the plan meters.
pub(super) fn parse(payload: &Value) -> Parsed {
    let resets_at = payload
        .get("quota_reset_date")
        .and_then(Value::as_str)
        .and_then(parse_time);
    let snapshots = payload.get("quota_snapshots");
    let limits = QUOTAS
        .into_iter()
        .filter_map(|(key, label)| quota_limit(label, snapshots?.get(key)?, resets_at))
        .collect();
    Parsed {
        plan: payload.get("copilot_plan").and_then(text),
        limits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A capped quota counts what is used of it; an unlimited one says so and has no cap.
    #[test]
    fn quotas_are_counted_or_unlimited() {
        let payload = json!({
            "copilot_plan": "individual_pro",
            "quota_reset_date": "2026-12-01",
            "quota_snapshots": {
                "premium_interactions": { "entitlement": 300, "remaining": 75, "percent_remaining": 25, "unlimited": false },
                "chat": { "entitlement": 0, "remaining": 0, "percent_remaining": 100, "unlimited": true }
            }
        });
        let parsed = parse(&payload);
        assert_eq!(parsed.plan.as_deref(), Some("individual_pro"));
        let premium = &parsed.limits[0];
        assert_eq!(
            (premium.used, premium.limit, premium.used_fraction),
            (Some(225.0), Some(300.0), Some(0.75))
        );
        assert_eq!(premium.resets_at_ms, parse_time("2026-12-01"));
        let chat = &parsed.limits[1];
        assert!(chat.unlimited && !chat.exhausted() && chat.limit.is_none());
    }
}
