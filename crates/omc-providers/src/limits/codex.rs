//! Codex: the windows of a ChatGPT plan, read from the usage endpoint of the ChatGPT backend.

use omc_auth::Credentials;
use omc_wire::{Limit, LimitUnit};
use serde_json::Value;

use super::{Parsed, counted, epoch_ms, get_json, now_ms, number, share, text, window_of};
use crate::error::ProviderError;

/// Where the ChatGPT backend reports what a plan has used.
const USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";

/// Reads the usage of the ChatGPT plan `credentials` are a login for.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(super) async fn fetch(credentials: &Credentials) -> Result<Option<Parsed>, ProviderError> {
    if credentials.access.is_empty() {
        return Ok(None);
    }
    let mut headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credentials.access),
        ),
        (
            "User-Agent".to_owned(),
            format!("omc/{}", env!("CARGO_PKG_VERSION")),
        ),
    ];
    if let Some(account) = &credentials.account_id {
        headers.push(("ChatGPT-Account-Id".to_owned(), account.clone()));
    }
    let payload = get_json(USAGE_URL, headers).await?;
    Ok(Some(parse(&payload, now_ms())))
}

/// The limit a rate-limit window describes, labelled `prefix` and the window's length; nothing when it reports no share used.
fn window_limit(window: &Value, prefix: &str, now: u64) -> Option<Limit> {
    let percent = number(window.get("used_percent")?)?;
    let length_ms = window
        .get("limit_window_seconds")
        .and_then(number)
        .map_or(0, |seconds| (seconds * 1000.0) as u64);
    let resets_at = window.get("reset_at").and_then(epoch_ms).or_else(|| {
        let after = number(window.get("reset_after_seconds")?)?;
        Some(now + (after * 1000.0) as u64)
    });
    let (name, length) = if length_ms > 0 {
        let (name, length) = window_of(length_ms);
        (Some(name), length)
    } else {
        (None, "window".to_owned())
    };
    let label = if prefix.is_empty() {
        length
    } else {
        format!("{prefix} {length}")
    };
    Some(share(&label, name.as_deref(), percent, resets_at))
}

/// The limits of one `rate_limit` entry: its primary window, then its secondary.
fn rate_limit(entry: &Value, prefix: &str, now: u64) -> Vec<Limit> {
    ["primary_window", "secondary_window"]
        .into_iter()
        .filter_map(|key| window_limit(entry.get(key)?, prefix, now))
        .collect()
}

/// The per-user credit cap of a seat billed by credits, from `spend_control`; nothing when there is none.
fn spend_cap(payload: &Value) -> Option<Limit> {
    let cap = payload.pointer("/spend_control/individual_limit")?;
    let used = cap.get("used").and_then(number);
    let limit = cap.get("limit").and_then(number);
    let fraction = cap
        .get("used_percent")
        .and_then(number)
        .map(|percent| percent / 100.0);
    if used.is_none() && limit.is_none() && fraction.is_none() {
        return None;
    }
    let resets_at = cap.get("reset_at").and_then(epoch_ms);
    Some(counted(
        "Credit cap",
        None,
        LimitUnit::Credits,
        (used, limit),
        fraction,
        resets_at,
    ))
}

/// What a usage payload holds: the plan's windows, those of each extra metered feature, and a credit cap; `now` turns "resets in" into a time.
pub(super) fn parse(payload: &Value, now: u64) -> Parsed {
    let mut limits = Vec::new();
    if let Some(entry) = payload.get("rate_limit") {
        limits.extend(rate_limit(entry, "", now));
    }
    let extras = payload
        .get("additional_rate_limits")
        .and_then(Value::as_array);
    for extra in extras.into_iter().flatten() {
        let name = extra
            .get("limit_name")
            .and_then(text)
            .or_else(|| extra.get("metered_feature").and_then(text));
        if let (Some(name), Some(entry)) = (name, extra.get("rate_limit")) {
            limits.extend(rate_limit(entry, &name, now));
        }
    }
    limits.extend(spend_cap(payload));
    Parsed {
        plan: payload.get("plan_type").and_then(text),
        limits,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The plan's windows are named by their length, and a reset given as a wait is counted from now.
    #[test]
    fn plan_windows_are_named_by_length() {
        let payload = json!({
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": { "used_percent": 41, "limit_window_seconds": 18000, "reset_after_seconds": 600 },
                "secondary_window": { "used_percent": 9, "limit_window_seconds": 604800, "reset_at": 1_794_312_000 }
            },
            "additional_rate_limits": [{
                "limit_name": "Spark",
                "rate_limit": { "primary_window": { "used_percent": 100, "limit_window_seconds": 18000 } }
            }]
        });
        let parsed = parse(&payload, 1_000);
        assert_eq!(parsed.plan.as_deref(), Some("pro"));
        let labels: Vec<&str> = parsed
            .limits
            .iter()
            .map(|limit| limit.label.as_str())
            .collect();
        assert_eq!(labels, ["5 hour", "7 day", "Spark 5 hour"]);
        assert_eq!(parsed.limits[0].resets_at_ms, Some(601_000));
        assert_eq!(parsed.limits[1].resets_at_ms, Some(1_794_312_000_000));
        assert!(parsed.limits[2].exhausted());
    }

    /// A seat billed by credits has no windows, only a cap.
    #[test]
    fn a_credit_cap_is_a_limit() {
        let payload =
            json!({ "spend_control": { "individual_limit": { "used": 30, "limit": 120 } } });
        let limit = &parse(&payload, 0).limits[0];
        assert_eq!(
            (limit.unit, limit.used_fraction),
            (LimitUnit::Credits, Some(0.25))
        );
    }
}
