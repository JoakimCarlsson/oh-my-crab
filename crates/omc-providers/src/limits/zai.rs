//! Z.ai: the token, request and credit quotas of a coding plan, read from the quota monitor.

use omc_auth::Credentials;
use omc_wire::LimitUnit;
use serde_json::Value;

use super::{DAY_MS, HOUR_MS, Parsed, counted, epoch_ms, get_json, number, text};
use crate::error::ProviderError;
use crate::provider::Provider;

/// Where, below the API's address, Z.ai reports what a plan has used.
const QUOTA_PATH: &str = "/api/monitor/usage/quota/limit";

/// The scheme and host of `url`, without a path.
fn origin(url: &str) -> &str {
    let after_scheme = url.find("://").map_or(0, |at| at + 3);
    match url[after_scheme..].find('/') {
        Some(at) => &url[..after_scheme + at],
        None => url,
    }
}

/// Reads the quotas of the coding plan the key in `credentials` belongs to, at the host the provider is set to use.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure or a non-success status.
pub(super) async fn fetch(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Option<Parsed>, ProviderError> {
    if credentials.access.trim().is_empty() {
        return Ok(None);
    }
    let url = format!("{}{QUOTA_PATH}", origin(&provider.def().base_url()));
    let headers = vec![
        ("Authorization".to_owned(), credentials.access.clone()),
        ("Content-Type".to_owned(), "application/json".to_owned()),
    ];
    let payload = get_json(&url, headers).await?;
    Ok(parse(&payload))
}

/// The window an entry names with a unit and a count, as its short name, its label and its length in milliseconds.
fn window_of_entry(entry: &Value) -> Option<(String, String, u64)> {
    let count = entry
        .get("number")
        .and_then(number)
        .filter(|count| *count > 0.0)
        .unwrap_or(1.0) as u64;
    let plural = if count == 1 { "" } else { "s" };
    match entry.get("unit").and_then(number)? as u64 {
        3 => Some((
            format!("{count}h"),
            format!("{count} hour{plural}"),
            count * HOUR_MS,
        )),
        4 => Some((
            format!("{count}d"),
            format!("{count} day{plural}"),
            count * DAY_MS,
        )),
        5 => Some((
            format!("{count}mo"),
            if count == 1 {
                "monthly".to_owned()
            } else {
                format!("{count} months")
            },
            count * 30 * DAY_MS,
        )),
        6 => Some(("1w".to_owned(), "weekly".to_owned(), 7 * DAY_MS)),
        _ => None,
    }
}

/// What a quota payload holds; nothing when the request failed, as it does for an account with no coding plan.
pub(super) fn parse(payload: &Value) -> Option<Parsed> {
    if payload.get("success").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let data = payload.get("data")?;
    let mut limits = Vec::new();
    for entry in data
        .get("limits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let (kind, unit) = match entry.get("type").and_then(Value::as_str) {
            Some("TOKENS_LIMIT") => ("Tokens", LimitUnit::Tokens),
            Some("TIME_LIMIT") => ("Requests", LimitUnit::Requests),
            Some("CREDIT_LIMIT") => ("Credits", LimitUnit::Credits),
            _ => continue,
        };
        let window = window_of_entry(entry);
        let label = match &window {
            Some((_, length, _)) => format!("{kind} ({length})"),
            None => kind.to_owned(),
        };
        let used = entry.get("currentValue").and_then(number);
        let cap = entry.get("usage").and_then(number).filter(|cap| *cap > 0.0);
        let percentage = entry
            .get("percentage")
            .and_then(number)
            .map(|percent| percent / 100.0);
        let fraction = if used.is_some() && cap.is_some() {
            None
        } else {
            percentage
        };
        limits.push(counted(
            &label,
            window.as_ref().map(|(name, _, _)| name.as_str()),
            unit,
            (used, cap),
            fraction,
            entry.get("nextResetTime").and_then(epoch_ms),
        ));
    }
    Some(Parsed {
        plan: data.get("level").and_then(text),
        limits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A plan's quotas are named by their unit and window, with the counts when Z.ai gives them and the percentage when it does not.
    #[test]
    fn quotas_are_named_by_unit_and_window() {
        let payload = json!({ "success": true, "data": { "level": "pro", "limits": [
            { "type": "TOKENS_LIMIT", "unit": 3, "number": 5, "percentage": 30, "nextResetTime": 1_794_312_000_000_u64 },
            { "type": "TIME_LIMIT", "unit": 5, "number": 1, "usage": 1000, "currentValue": 250, "percentage": 99 }
        ]}});
        let parsed = parse(&payload).unwrap();
        assert_eq!(parsed.plan.as_deref(), Some("pro"));
        assert_eq!(parsed.limits[0].label, "Tokens (5 hours)");
        assert_eq!(parsed.limits[0].window.as_deref(), Some("5h"));
        assert_eq!(parsed.limits[0].used_fraction, Some(0.3));
        assert_eq!(parsed.limits[1].label, "Requests (monthly)");
        assert_eq!(parsed.limits[1].used_fraction, Some(0.25));
    }

    /// A refused request, such as for an account with no coding plan, reports nothing.
    #[test]
    fn a_failure_reports_nothing() {
        assert!(parse(&json!({ "success": false, "msg": "no coding plan" })).is_none());
    }

    /// The host of an address is the part before its path.
    #[test]
    fn the_origin_drops_the_path() {
        assert_eq!(
            origin("https://api.z.ai/api/coding/paas/v4"),
            "https://api.z.ai"
        );
        assert_eq!(origin("https://api.z.ai"), "https://api.z.ai");
    }
}
