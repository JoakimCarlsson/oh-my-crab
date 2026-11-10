//! An account's subscription allowance as text a reader can take in at a glance: a bar, a share and a wait for each window.

use omc_wire::{Limit, LimitUnit, Limits};

/// How many cells the bar of a limit has.
const BAR_CELLS: usize = 20;

/// The bar of `fraction` of an allowance used: filled cells then empty ones, held between nothing and everything.
fn bar(fraction: f64) -> String {
    let filled = (fraction.clamp(0.0, 1.0) * BAR_CELLS as f64).round() as usize;
    format!("[{}{}]", "#".repeat(filled), "-".repeat(BAR_CELLS - filled))
}

/// How long until `resets_at_ms`, from `now_ms`, as `2d 3h`, `2h 14m` or `9m`; `now` when it has passed.
fn wait(resets_at_ms: u64, now_ms: u64) -> String {
    let minutes = resets_at_ms.saturating_sub(now_ms) / 60_000;
    let (days, hours, minutes) = (minutes / 1440, minutes % 1440 / 60, minutes % 60);
    match (days, hours, minutes) {
        (0, 0, 0) => "now".to_owned(),
        (0, 0, minutes) => format!("{minutes}m"),
        (0, hours, minutes) => format!("{hours}h {minutes}m"),
        (days, hours, _) => format!("{days}d {hours}h"),
    }
}

/// `value` counted in `unit`, as a figure with no needless decimals: `225`, `12.50 USD` is written `$12.50`.
fn amount(value: f64, unit: LimitUnit) -> String {
    match unit {
        LimitUnit::Usd => format!("${value:.2}"),
        _ if value.fract() == 0.0 => format!("{value:.0}"),
        _ => format!("{value:.1}"),
    }
}

/// The text of one limit, its label padded to `width`: the bar and share when the share is known, the counts, and when the window resets.
fn line(limit: &Limit, width: usize, now_ms: u64) -> String {
    let mut parts = vec![format!("  {:<width$}", limit.label)];
    if limit.unlimited {
        parts.push("unlimited".to_owned());
    } else if let Some(fraction) = limit.used_fraction {
        parts.push(format!("{}  {:>3.0}%", bar(fraction), fraction * 100.0));
    }
    if limit.unit != LimitUnit::Percent && !limit.unlimited {
        match (limit.used, limit.limit) {
            (Some(used), Some(cap)) => parts.push(format!(
                "{} / {}",
                amount(used, limit.unit),
                amount(cap, limit.unit)
            )),
            (Some(used), None) => parts.push(format!("{} used", amount(used, limit.unit))),
            _ => {}
        }
    }
    if let Some(at) = limit.resets_at_ms {
        parts.push(format!("resets in {}", wait(at, now_ms)));
    }
    parts.join("  ")
}

/// The text of what an account has used, headed with the provider's `name` and the plan when it is known; `now_ms` is the time the waits count from.
pub fn render(name: &str, limits: &Limits, now_ms: u64) -> String {
    let mut text = match &limits.plan {
        Some(plan) => format!("{name} ({plan})\n"),
        None => format!("{name}\n"),
    };
    let width = limits
        .limits
        .iter()
        .map(|limit| limit.label.chars().count())
        .max()
        .unwrap_or(0);
    for limit in &limits.limits {
        text.push_str(&line(limit, width, now_ms));
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window shows its bar, its share and its wait; a capped count shows both figures; an unlimited quota says so.
    #[test]
    fn limits_read_at_a_glance() {
        let mut five = Limit::new("5 hour");
        five.unit = LimitUnit::Percent;
        five.used_fraction = Some(0.25);
        five.resets_at_ms = Some(8_040_000);
        let mut premium = Limit::new("Premium requests");
        premium.unit = LimitUnit::Requests;
        premium.used = Some(225.0);
        premium.limit = Some(300.0);
        premium.used_fraction = Some(0.75);
        let mut chat = Limit::new("Chat");
        chat.unlimited = true;
        let limits = Limits {
            provider: "claude".into(),
            plan: Some("max".into()),
            fetched_at_ms: 0,
            limits: vec![five, premium, chat],
        };
        let text = render("Claude", &limits, 0);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "Claude (max)");
        assert_eq!(
            lines[1],
            "  5 hour            [#####---------------]   25%  resets in 2h 14m"
        );
        assert_eq!(
            lines[2],
            "  Premium requests  [###############-----]   75%  225 / 300"
        );
        assert_eq!(lines[3], "  Chat              unlimited");
    }

    /// A wait is written in its two largest units, and a past reset is now.
    #[test]
    fn waits_are_short() {
        assert_eq!(wait(90_000_000, 0), "1d 1h");
        assert_eq!(wait(540_000, 0), "9m");
        assert_eq!(wait(0, 5), "now");
    }
}
