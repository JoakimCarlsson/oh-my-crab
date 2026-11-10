//! MiniMax: the interval and weekly allowance of a token plan, read from the plan's remaining-quota endpoint.

use omc_auth::Credentials;
use omc_wire::Limit;
use serde_json::Value;

use super::{Parsed, epoch_ms, get_json, number, share, window_of};
use crate::error::ProviderError;
use crate::provider::Provider;

/// Where, below the API's address, MiniMax reports what a token plan has left.
const REMAINS_PATH: &str = "/v1/token_plan/remains";

/// The status MiniMax gives a window whose allowance is spent.
const STATUS_EXHAUSTED: u64 = 2;

/// The status MiniMax gives a window that has no cap.
const STATUS_UNLIMITED: u64 = 3;

/// Reads what the token plan of the key in `credentials` has left, at the host the provider is set to use.
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
    let base = provider.def().base_url();
    let base = base.trim_end_matches("/v1").trim_end_matches("/anthropic");
    let headers = vec![
        ("Accept".to_owned(), "application/json".to_owned()),
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credentials.access),
        ),
    ];
    let payload = get_json(&format!("{base}{REMAINS_PATH}"), headers).await?;
    Ok(parse(&payload))
}

/// The status of a window as MiniMax numbered it, when it gave one.
fn status(bucket: &Value, key: &str) -> Option<u64> {
    bucket.get(key).and_then(number).map(|status| status as u64)
}

/// The limit one window of a model's bucket describes, labelled `label`; nothing when it reports no share left.
///
/// `prefix` is the window's keys, `current_interval` or `current_weekly`, and `name` its short name when the bucket does not give the span.
fn window_limit(bucket: &Value, model: &str, prefix: &str, name: Option<&str>) -> Option<Limit> {
    let (start, end) = if prefix == "current_interval" {
        ("start_time", "end_time")
    } else {
        ("weekly_start_time", "weekly_end_time")
    };
    let start = bucket.get(start).and_then(epoch_ms);
    let end = bucket.get(end).and_then(epoch_ms);
    let (window, length) = match (name, start.zip(end)) {
        (Some(name), _) => (name.to_owned(), "7 day".to_owned()),
        (None, Some((start, end))) if end > start => window_of(end - start),
        (None, _) => ("interval".to_owned(), "interval".to_owned()),
    };
    let flag = status(bucket, &format!("{prefix}_status"));
    let remaining = bucket
        .get(format!("{prefix}_remaining_percent"))
        .and_then(number);
    let percent = if flag == Some(STATUS_EXHAUSTED) {
        100.0
    } else {
        100.0 - remaining?
    };
    let mut label = model.to_owned();
    if let Some(first) = label.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    let mut limit = share(&format!("{label} {length}"), Some(&window), percent, end);
    limit.unlimited = flag == Some(STATUS_UNLIMITED);
    Some(limit)
}

/// What a remaining-quota payload holds: an interval window and a weekly one for each model; nothing when the request failed or the account has no plan, which MiniMax reports as windows that are all unlimited and uncounted.
pub(super) fn parse(payload: &Value) -> Option<Parsed> {
    if payload.pointer("/base_resp/status_code").and_then(number) != Some(0.0) {
        return None;
    }
    let mut limits = Vec::new();
    for bucket in payload.get("model_remains")?.as_array()? {
        let Some(model) = bucket
            .get("model_name")
            .and_then(Value::as_str)
            .map(str::trim)
        else {
            continue;
        };
        let no_plan = ["current_interval", "current_weekly"].iter().all(|prefix| {
            status(bucket, &format!("{prefix}_status")) == Some(STATUS_UNLIMITED)
                && bucket.get(format!("{prefix}_total_count")).and_then(number) == Some(0.0)
        });
        if model.is_empty() || no_plan {
            continue;
        }
        limits.extend(window_limit(bucket, model, "current_interval", None));
        limits.extend(window_limit(bucket, model, "current_weekly", Some("7d")));
    }
    Some(Parsed { plan: None, limits })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Each model has an interval window, named by its span, and a weekly one, with the share used worked out from the share left.
    #[test]
    fn windows_come_from_the_share_left() {
        let payload = json!({
            "base_resp": { "status_code": 0 },
            "model_remains": [{
                "model_name": "general",
                "start_time": 1_794_294_000_000_u64, "end_time": 1_794_312_000_000_u64,
                "current_interval_remaining_percent": 60,
                "weekly_end_time": 1_794_900_000_000_u64,
                "current_weekly_remaining_percent": 90, "current_weekly_status": 2
            }]
        });
        let limits = parse(&payload).unwrap().limits;
        assert_eq!(limits[0].label, "General 5 hour");
        assert_eq!(limits[0].used_fraction, Some(0.4));
        assert_eq!(limits[0].resets_at_ms, Some(1_794_312_000_000));
        assert!(limits[1].exhausted());
    }

    /// A model the plan does not cover is left out, and a rejected request reports nothing.
    #[test]
    fn uncovered_models_and_failures_are_left_out() {
        let payload = json!({ "base_resp": { "status_code": 0 }, "model_remains": [{
            "model_name": "speech",
            "current_interval_total_count": 0, "current_interval_status": 3,
            "current_weekly_total_count": 0, "current_weekly_status": 3
        }]});
        assert!(parse(&payload).unwrap().limits.is_empty());
        assert!(parse(&json!({ "base_resp": { "status_code": 1004 } })).is_none());
    }
}
