//! Kimi Code: the windows of the Kimi For Coding subscription, read from its usage endpoint.

use omc_auth::Credentials;
use omc_wire::{Limit, LimitUnit};
use serde_json::Value;

use super::{
    DAY_MS, HOUR_MS, MINUTE_MS, Parsed, counted, epoch_ms, get_json, now_ms, number, parse_time,
    share, text, window_of,
};
use crate::error::ProviderError;
use crate::provider::Provider;

/// Reads the usage of the Kimi Code subscription `credentials` belong to, at the address they were made for: that of the account login, else the provider's.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(super) async fn fetch(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Option<Parsed>, ProviderError> {
    if credentials.access.is_empty() {
        return Ok(None);
    }
    let base = credentials
        .base_url
        .clone()
        .unwrap_or_else(|| provider.def().base_url());
    let mut headers: Vec<(String, String)> = provider
        .def()
        .headers
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
    headers.extend(credentials.headers.iter().cloned());
    headers.push((
        "Authorization".to_owned(),
        format!("Bearer {}", credentials.access),
    ));
    let payload = get_json(&format!("{}/usages", base.trim_end_matches('/')), headers).await?;
    Ok(Some(Parsed {
        plan: None,
        limits: parse(&payload, now_ms()),
    }))
}

/// When a record says its window resets: a time under one of the `reset` keys, or a wait in seconds under one of the `reset in` keys counted from `now`.
fn reset_time(record: &Value, now: u64) -> Option<u64> {
    let times = ["reset_at", "resetAt", "reset_time", "resetTime"];
    let at = times.iter().find_map(|key| {
        let value = record.get(*key)?;
        value
            .as_str()
            .and_then(parse_time)
            .or_else(|| epoch_ms(value))
    });
    at.or_else(|| {
        ["reset_in", "resetIn", "ttl", "window"]
            .iter()
            .find_map(|key| number(record.get(*key)?))
            .map(|seconds| now + (seconds * 1000.0) as u64)
    })
}

/// The length of a window given as a count and the name of a unit, such as `300` and `TIME_UNIT_MINUTE`, in milliseconds.
fn window_length(window: &Value) -> Option<u64> {
    let count = number(window.get("duration")?)?;
    let unit = window.get("timeUnit")?.as_str()?.to_uppercase();
    let each = if unit.contains("MINUTE") {
        MINUTE_MS
    } else if unit.contains("HOUR") {
        HOUR_MS
    } else if unit.contains("DAY") {
        DAY_MS
    } else if unit.contains("WEEK") {
        7 * DAY_MS
    } else if unit.contains("SECOND") {
        return Some((count * 1000.0) as u64);
    } else {
        return None;
    };
    Some((count * each as f64) as u64)
}

/// The limit a record with `limit`, `used` and `remaining` counts describes, labelled `label` unless the record names itself; nothing when it holds no count.
fn row(record: &Value, label: &str, window: Option<&str>, now: u64) -> Option<Limit> {
    let limit = record.get("limit").and_then(number);
    let remaining = record.get("remaining").and_then(number);
    let used = record
        .get("used")
        .and_then(number)
        .or_else(|| Some(limit? - remaining?));
    if used.is_none() && limit.is_none() {
        return None;
    }
    let name = ["name", "title"]
        .iter()
        .find_map(|key| record.get(*key).and_then(text))
        .unwrap_or_else(|| label.to_owned());
    Some(counted(
        &name,
        window,
        LimitUnit::Unknown,
        (used, limit),
        None,
        reset_time(record, now),
    ))
}

/// The limits of a usage payload: the weekly summary, the total quota, each entry of `limits` with its window, and the monthly shares of `usages`; `now` turns a wait into a time.
pub(super) fn parse(payload: &Value, now: u64) -> Vec<Limit> {
    let mut limits = Vec::new();
    if let Some(summary) = payload.get("usage") {
        limits.extend(row(summary, "Weekly limit", Some("7d"), now));
    }
    if let Some(total) = payload.get("totalQuota") {
        let length = total.get("window").and_then(window_length);
        let window = length.map(|length| window_of(length).0);
        limits.extend(row(total, "Total quota", window.as_deref(), now));
    }
    let entries = payload.get("limits").and_then(Value::as_array);
    for (index, entry) in entries.into_iter().flatten().enumerate() {
        let detail = entry.get("detail").unwrap_or(entry);
        let length = entry.get("window").and_then(window_length);
        let (window, span) = match length {
            Some(length) => {
                let (name, label) = window_of(length);
                (Some(name), Some(format!("{label} limit")))
            }
            None => (None, None),
        };
        let label = ["name", "title", "scope"]
            .iter()
            .find_map(|key| entry.get(*key).or_else(|| detail.get(*key)).and_then(text))
            .or(span)
            .unwrap_or_else(|| format!("Limit {}", index + 1));
        let reset = reset_time(detail, now)
            .or_else(|| entry.get("window").and_then(|w| reset_time(w, now)));
        if let Some(mut limit) = row(detail, &label, window.as_deref(), now) {
            limit.resets_at_ms = limit.resets_at_ms.or(reset);
            limits.push(limit);
        }
    }
    if let Some(shares) = payload.get("usages").and_then(Value::as_object) {
        for (key, aggregate) in shares.iter().filter(|(key, _)| *key != "limit_5h") {
            let Some(ratio) = aggregate.get("used_ratio").and_then(number) else {
                continue;
            };
            let label = match key.as_str() {
                "limit_month_total" => "Monthly total",
                "limit_month_code" => "Monthly code",
                other => other,
            };
            limits.push(share(
                label,
                Some(key),
                ratio.clamp(0.0, 1.0) * 100.0,
                reset_time(aggregate, now),
            ));
        }
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The weekly summary and the five-hour window of `limits` are listed, with the window named by its length.
    #[test]
    fn the_summary_and_the_windows_are_listed() {
        let payload = json!({
            "usage": { "limit": "100", "used": "40", "resetTime": "2026-11-14T00:00:00Z" },
            "limits": [{
                "window": { "duration": 300, "timeUnit": "TIME_UNIT_MINUTE" },
                "detail": { "limit": "50", "remaining": "45", "resetTime": "2026-11-10T15:00:00Z" }
            }]
        });
        let limits = parse(&payload, 0);
        assert_eq!(limits[0].label, "Weekly limit");
        assert_eq!(limits[0].used_fraction, Some(0.4));
        assert_eq!(limits[1].label, "5 hour limit");
        assert_eq!(limits[1].window.as_deref(), Some("5h"));
        assert_eq!(limits[1].used, Some(5.0));
        assert_eq!(limits[1].resets_at_ms, parse_time("2026-11-10T15:00:00Z"));
    }

    /// A monthly share is read from its ratio, and the five-hour entry of `usages` is left to `limits`.
    #[test]
    fn monthly_shares_come_from_ratios() {
        let payload = json!({ "usages": {
            "limit_5h": { "used_ratio": 0.5 },
            "limit_month_total": { "used_ratio": 0.2, "reset_in": 60 }
        }});
        let limits = parse(&payload, 1_000);
        assert_eq!(limits.len(), 1);
        assert_eq!(limits[0].label, "Monthly total");
        assert_eq!(limits[0].resets_at_ms, Some(61_000));
    }
}
