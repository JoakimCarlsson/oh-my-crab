//! How much of a subscription's allowance an account has used, asked of the provider that meters it.
//!
//! Claude, Codex, GitHub Copilot, Kimi Code, Z.ai and MiniMax each answer with their own shape; every one is
//! turned into the same [`Limits`]. The endpoints are the ones those providers' own tools read and are not
//! documented, so every parser takes what is there and skips what is not.

mod claude;
mod codex;
mod copilot;
mod kimi;
mod minimax;
mod zai;

use std::time::{SystemTime, UNIX_EPOCH};

use omc_auth::Credentials;
use omc_wire::{Limit, LimitUnit, Limits};
use serde_json::Value;

use crate::error::ProviderError;
use crate::provider::Provider;

/// One minute, in milliseconds.
const MINUTE_MS: u64 = 60_000;
/// One hour, in milliseconds.
const HOUR_MS: u64 = 60 * MINUTE_MS;
/// One day, in milliseconds.
const DAY_MS: u64 = 24 * HOUR_MS;

/// What a provider's usage answer holds: the plan, when it names one, and the limits.
struct Parsed {
    /// The plan the account is on.
    plan: Option<String>,
    /// The windows and quotas, in the order to show them.
    limits: Vec<Limit>,
}

/// Whether `provider` meters an allowance that [`fetch`] can read.
pub fn supported(provider: Provider) -> bool {
    matches!(
        provider.def().id,
        "claude" | "codex" | "copilot" | "kimi-code" | "zai" | "zai-coding" | "minimax"
    )
}

/// Asks the provider what `credentials` have used of their allowance.
///
/// Nothing is returned when the provider meters no allowance that can be read, the credentials are an API key
/// of a provider whose allowance is only metered for logins, or the answer holds no limit: an account without
/// a plan has nothing to show.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, or when the provider refuses the request, as for a login it no
/// longer accepts. A usage endpoint that is rate limited answers with a status, too.
pub async fn fetch(
    provider: Provider,
    credentials: &Credentials,
) -> Result<Option<Limits>, ProviderError> {
    let id = provider.def().id;
    let parsed = match id {
        "claude" => claude::fetch(credentials).await?,
        "codex" => codex::fetch(credentials).await?,
        "copilot" => copilot::fetch(credentials).await?,
        "kimi-code" => kimi::fetch(provider, credentials).await?,
        "zai" | "zai-coding" => zai::fetch(provider, credentials).await?,
        "minimax" => minimax::fetch(provider, credentials).await?,
        _ => None,
    };
    Ok(parsed
        .filter(|parsed| !parsed.limits.is_empty())
        .map(|parsed| Limits {
            provider: id.to_owned(),
            plan: parsed.plan,
            fetched_at_ms: now_ms(),
            limits: parsed.limits,
        }))
}

/// Sends a `GET` to `url` with `headers` and returns the JSON the provider answers with.
///
/// # Errors
///
/// Returns [`ProviderError`] on network failure, a non-success status, or a body that is not JSON.
async fn get_json(url: &str, headers: Vec<(String, String)>) -> Result<Value, ProviderError> {
    let mut request = reqwest::Client::new().get(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(ProviderError::from_response(response).await);
    }
    Ok(response.json().await?)
}

/// The current Unix time in milliseconds.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads `value` as a number: a JSON number, or a string holding one, which some providers send; nothing for anything else or a number that is not finite.
fn number(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    parsed.filter(|number| number.is_finite())
}

/// Reads `value` as a non-empty string.
fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Reads a Unix time given in seconds or in milliseconds as milliseconds; a time is taken to be in milliseconds when it is too large to be a date in seconds.
fn epoch_ms(value: &Value) -> Option<u64> {
    let time = number(value).filter(|time| *time > 0.0)?;
    Some(if time < 1.0e12 { time * 1000.0 } else { time } as u64)
}

/// Reads `text` as a date and time in RFC 3339 form, or as a date alone, which counts as midnight at its start in UTC, as Unix time in milliseconds.
fn parse_time(text: &str) -> Option<u64> {
    let text = text.trim();
    let (date, rest) = match text.split_once(['T', ' ']) {
        Some((date, rest)) => (date, Some(rest)),
        None => (text, None),
    };
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let mut seconds = days_from_civil(year, month, day) * 86_400;
    let mut millis = 0;
    if let Some(rest) = rest {
        let (clock, offset) = split_offset(rest)?;
        let mut fields = clock.split(':');
        let hour: i64 = fields.next()?.parse().ok()?;
        let minute: i64 = fields.next().unwrap_or("0").parse().ok()?;
        let second = fields.next().unwrap_or("0");
        let (whole, fraction) = second.split_once('.').unwrap_or((second, ""));
        let whole: i64 = whole.parse().ok()?;
        let digits: String = fraction.chars().take(3).collect();
        if !digits.is_empty() {
            millis = format!("{digits:0<3}").parse::<i64>().ok()?;
        }
        seconds += hour * 3600 + minute * 60 + whole - offset;
    }
    u64::try_from(seconds * 1000 + millis).ok()
}

/// Splits the time of day of an RFC 3339 time from its offset from UTC, which is returned in seconds; `Z` or no offset is none.
fn split_offset(rest: &str) -> Option<(&str, i64)> {
    if let Some(clock) = rest.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
    let Some(at) = rest.rfind(['+', '-']) else {
        return Some((rest, 0));
    };
    let (clock, offset) = rest.split_at(at);
    let sign = if offset.starts_with('-') { -1 } else { 1 };
    let mut fields = offset[1..].split(':');
    let hours: i64 = fields.next()?.parse().ok()?;
    let minutes: i64 = fields.next().unwrap_or("0").parse().ok()?;
    Some((clock, sign * (hours * 3600 + minutes * 60)))
}

/// The number of days from 1970-01-01 to the given day of the proleptic Gregorian calendar.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year.rem_euclid(400);
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// A window of `length_ms` as its short name and the label a client shows: `5h` and `5 hour`, `7d` and `7 day`.
fn window_of(length_ms: u64) -> (String, String) {
    if length_ms >= DAY_MS {
        let days = (length_ms as f64 / DAY_MS as f64).round() as u64;
        return (format!("{days}d"), format!("{days} day"));
    }
    if length_ms >= HOUR_MS || length_ms == 0 {
        let hours = ((length_ms as f64 / HOUR_MS as f64).round() as u64).max(1);
        return (format!("{hours}h"), format!("{hours} hour"));
    }
    let minutes = (length_ms as f64 / MINUTE_MS as f64).round() as u64;
    (format!("{minutes}m"), format!("{minutes} minute"))
}

/// A limit that is a share of the allowance: `percent` used, from 0 to 100, which is clamped to that range.
fn share(label: &str, window: Option<&str>, percent: f64, resets_at_ms: Option<u64>) -> Limit {
    let percent = percent.clamp(0.0, 100.0);
    Limit {
        window: window.map(str::to_owned),
        unit: LimitUnit::Percent,
        used_fraction: Some(percent / 100.0),
        used: Some(percent),
        limit: Some(100.0),
        resets_at_ms,
        ..Limit::new(label)
    }
}

/// A limit counted in `unit`: `used` of `limit`. The share used is `fraction` when the provider gave one, else worked out from the counts, and in either case held between 0 and 1.
fn counted(
    label: &str,
    window: Option<&str>,
    unit: LimitUnit,
    counts: (Option<f64>, Option<f64>),
    fraction: Option<f64>,
    resets_at_ms: Option<u64>,
) -> Limit {
    let (used, limit) = counts;
    let worked = match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => Some(used / limit),
        _ => None,
    };
    Limit {
        window: window.map(str::to_owned),
        unit,
        used_fraction: fraction.or(worked).map(|fraction| fraction.clamp(0.0, 1.0)),
        used,
        limit,
        resets_at_ms,
        ..Limit::new(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An RFC 3339 time and a bare date both read as Unix milliseconds, with the offset applied.
    #[test]
    fn times_are_read_as_unix_milliseconds() {
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_time("2026-11-10T12:00:00.250Z"),
            Some(1_794_312_000_250)
        );
        assert_eq!(
            parse_time("2026-11-10T14:00:00+02:00"),
            parse_time("2026-11-10T12:00:00Z")
        );
        assert_eq!(parse_time("2026-11-10"), Some(1_794_268_800_000));
        assert_eq!(parse_time("soon"), None);
    }

    /// Seconds and milliseconds are told apart by size, and strings holding numbers count.
    #[test]
    fn epochs_and_numbers_are_read_loosely() {
        assert_eq!(
            epoch_ms(&serde_json::json!(1_794_312_000)),
            Some(1_794_312_000_000)
        );
        assert_eq!(
            epoch_ms(&serde_json::json!(1_794_312_000_000_u64)),
            Some(1_794_312_000_000)
        );
        assert_eq!(number(&serde_json::json!("12.5")), Some(12.5));
        assert_eq!(number(&serde_json::json!(null)), None);
    }

    /// A window's length is named in the largest whole unit.
    #[test]
    fn windows_are_named_by_length() {
        assert_eq!(window_of(5 * HOUR_MS).0, "5h");
        assert_eq!(window_of(7 * DAY_MS).1, "7 day");
        assert_eq!(window_of(30 * MINUTE_MS).0, "30m");
    }

    /// A share over the top is held at the whole allowance, and counts give a share when none is given.
    #[test]
    fn shares_are_held_between_nothing_and_everything() {
        assert_eq!(share("x", None, 140.0, None).used_fraction, Some(1.0));
        let limit = counted(
            "x",
            None,
            LimitUnit::Requests,
            (Some(25.0), Some(100.0)),
            None,
            None,
        );
        assert_eq!(limit.used_fraction, Some(0.25));
    }
}
