//! Claude: the five-hour and weekly windows of a subscription, read from the account's usage endpoint.

use omc_auth::Credentials;
use omc_wire::{Limit, LimitUnit};
use serde_json::Value;

use super::{Parsed, counted, get_json, number, parse_time, share, text};
use crate::error::ProviderError;
use crate::identity;

/// Where Anthropic reports what a subscription has used.
const USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";

/// Reads the usage of the Claude subscription `credentials` are a login for; nothing for an API key, which has no windows.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(super) async fn fetch(credentials: &Credentials) -> Result<Option<Parsed>, ProviderError> {
    if !identity::is_oauth_token(&credentials.access) {
        return Ok(None);
    }
    let betas = identity::oauth_betas(false);
    let headers = identity::headers(&credentials.access, &betas);
    let payload = get_json(USAGE_URL, headers).await?;
    Ok(Some(Parsed {
        plan: None,
        limits: parse(&payload),
    }))
}

/// The window `bucket` describes, as a limit labelled `label` and named `window`; nothing when the bucket reports no share used.
fn bucket_limit(bucket: &Value, label: &str, window: &str) -> Option<Limit> {
    let percent = number(bucket.get("utilization")?)?;
    let resets_at = bucket
        .get("resets_at")
        .and_then(Value::as_str)
        .and_then(parse_time);
    Some(share(label, Some(window), percent, resets_at))
}

/// The weekly limits that name a model, from the generic `limits` list: those with the kind `weekly_scoped`, each model once, as `(model, bucket)` pairs.
fn scoped_weekly(payload: &Value) -> Vec<(String, Limit)> {
    let mut found: Vec<(String, Limit)> = Vec::new();
    let entries = payload.get("limits").and_then(Value::as_array);
    for entry in entries.into_iter().flatten() {
        if entry.get("kind").and_then(Value::as_str) != Some("weekly_scoped") {
            continue;
        }
        let Some(model) = entry.pointer("/scope/model/display_name").and_then(text) else {
            continue;
        };
        if found
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case(&model))
        {
            continue;
        }
        let bucket = serde_json::json!({
            "utilization": entry.get("percent"),
            "resets_at": entry.get("resets_at"),
        });
        let label = format!("7 day ({model})");
        if let Some(limit) = bucket_limit(&bucket, &label, "7d") {
            found.push((model, limit));
        }
    }
    found
}

/// A dollar amount of the form `{ amount_minor, exponent, currency }`, in dollars; nothing for another currency or a malformed amount.
fn dollars(amount: &Value) -> Option<f64> {
    let minor = amount.get("amount_minor")?.as_u64()?;
    let exponent = u32::try_from(amount.get("exponent")?.as_u64()?).ok()?;
    if !amount
        .get("currency")?
        .as_str()?
        .eq_ignore_ascii_case("usd")
    {
        return None;
    }
    Some(minor as f64 / 10f64.powi(i32::try_from(exponent).ok()?))
}

/// The spend on top of the plan, from the payload's `spend` entry, or from its older `extra_usage` entry when it has none; nothing when extra usage is off.
fn extra_usage(payload: &Value) -> Option<Limit> {
    let (used, cap) = match payload.get("spend").filter(|spend| !spend.is_null()) {
        Some(spend) => {
            if spend.get("enabled")?.as_bool() != Some(true) {
                return None;
            }
            let used = dollars(spend.get("used")?)?;
            let cap = spend
                .get("limit")
                .filter(|cap| !cap.is_null())
                .and_then(dollars);
            (used, cap)
        }
        None => {
            let extra = payload.get("extra_usage")?;
            if extra.get("is_enabled")?.as_bool() != Some(true) {
                return None;
            }
            let places = extra
                .get("decimal_places")
                .and_then(Value::as_i64)
                .unwrap_or(2) as i32;
            let divisor = 10f64.powi(places);
            let used = number(extra.get("used_credits")?)? / divisor;
            let cap = extra
                .get("monthly_limit")
                .and_then(number)
                .map(|cap| cap / divisor);
            (used, cap)
        }
    };
    let cap = cap.filter(|cap| *cap > 0.0);
    Some(counted(
        "Extra usage",
        Some("monthly"),
        LimitUnit::Usd,
        (Some(used), cap),
        None,
        None,
    ))
}

/// The limits an account's usage payload holds: the five-hour and weekly windows, the weekly window of each model that has its own, and the spend on top of the plan.
pub(super) fn parse(payload: &Value) -> Vec<Limit> {
    let mut limits = Vec::new();
    let windows = [
        ("five_hour", "5 hour", "5h"),
        ("seven_day", "7 day", "7d"),
        ("seven_day_opus", "7 day (Opus)", "7d"),
        ("seven_day_sonnet", "7 day (Sonnet)", "7d"),
    ];
    for (key, label, window) in windows {
        if let Some(limit) = payload
            .get(key)
            .and_then(|bucket| bucket_limit(bucket, label, window))
        {
            limits.push(limit);
        }
    }
    for (model, limit) in scoped_weekly(payload) {
        let taken = limits
            .iter()
            .any(|have| have.label.to_lowercase().contains(&model.to_lowercase()));
        if !taken {
            limits.push(limit);
        }
    }
    limits.extend(extra_usage(payload));
    limits
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The two account-wide windows come first, then a model's own weekly cap, and a null bucket is left out.
    #[test]
    fn windows_and_model_caps_are_listed() {
        let payload = json!({
            "five_hour": { "utilization": 37.0, "resets_at": "2026-11-10T15:00:00Z" },
            "seven_day": { "utilization": 12.5, "resets_at": "2026-11-14T00:00:00Z" },
            "seven_day_opus": null,
            "limits": [
                { "kind": "weekly", "percent": 12.5 },
                { "kind": "weekly_scoped", "percent": 80, "resets_at": "2026-11-14T00:00:00Z",
                  "scope": { "model": { "display_name": "Fable" } } },
                { "kind": "weekly_scoped", "percent": 81,
                  "scope": { "model": { "display_name": "fable" } } }
            ]
        });
        let limits = parse(&payload);
        let labels: Vec<&str> = limits.iter().map(|limit| limit.label.as_str()).collect();
        assert_eq!(labels, ["5 hour", "7 day", "7 day (Fable)"]);
        assert_eq!(limits[0].used_fraction, Some(0.37));
        assert_eq!(limits[0].resets_at_ms, parse_time("2026-11-10T15:00:00Z"));
        assert_eq!(limits[2].used_fraction, Some(0.8));
    }

    /// Spend on top of the plan is read in dollars, from either shape the payload has.
    #[test]
    fn extra_usage_is_read_in_dollars() {
        let current = json!({ "spend": {
            "enabled": true,
            "used": { "amount_minor": 1250, "exponent": 2, "currency": "USD" },
            "limit": { "amount_minor": 5000, "exponent": 2, "currency": "USD" }
        }});
        let limit = &parse(&current)[0];
        assert_eq!(
            (limit.used, limit.limit, limit.used_fraction),
            (Some(12.5), Some(50.0), Some(0.25))
        );
        let older = json!({ "extra_usage": {
            "is_enabled": true, "used_credits": 300, "monthly_limit": null
        }});
        let limit = &parse(&older)[0];
        assert_eq!((limit.used, limit.limit), (Some(3.0), None));
        assert!(parse(&json!({ "spend": { "enabled": false } })).is_empty());
    }
}
