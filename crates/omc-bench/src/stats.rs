//! How far the numbers of repeated runs can be trusted: intervals around pass rates, how steady a task is across its runs, the spread of time and tokens, and what the runs cost.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::run::Outcome;

/// The normal quantile of a two-sided 95% interval.
const Z95: f64 = 1.959_964;

/// The prices of a contender's tokens, in US dollars per million.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Price {
    /// What a million tokens sent to the model cost.
    pub(crate) sent: f64,
    /// What a million tokens the model wrote cost.
    pub(crate) written: f64,
}

impl Price {
    /// The price `text` gives, written `sent,written` in dollars per million tokens.
    ///
    /// # Errors
    ///
    /// Fails when there are not two numbers, or one is negative or not finite.
    pub(crate) fn parse(text: &str) -> Result<Self, String> {
        let wrong =
            || format!("a price is sent,written in dollars per million tokens, not {text:?}");
        let (sent, written) = text.split_once(',').ok_or_else(wrong)?;
        let number = |part: &str| {
            part.trim()
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .ok_or_else(wrong)
        };
        Ok(Self {
            sent: number(sent)?,
            written: number(written)?,
        })
    }

    /// What `sent` tokens sent and `written` tokens written cost, in dollars.
    pub(crate) fn cost(&self, sent: u64, written: u64) -> f64 {
        (sent as f64 * self.sent + written as f64 * self.written) / 1_000_000.0
    }
}

/// The Wilson score interval, at 95%, around `passed` successes of `runs`, as a lower and an upper share from 0 to 1; nothing is known of no runs, which gives 0 to 1.
pub(crate) fn wilson(passed: u32, runs: u32) -> (f64, f64) {
    if runs == 0 {
        return (0.0, 1.0);
    }
    let n = f64::from(runs);
    let p = f64::from(passed) / n;
    let z2 = Z95 * Z95;
    let centre = (p + z2 / (2.0 * n)) / (1.0 + z2 / n);
    let half = Z95 * (p * (1.0 - p) / n + z2 / (4.0 * n * n)).sqrt() / (1.0 + z2 / n);
    ((centre - half).max(0.0), (centre + half).min(1.0))
}

/// The chance that at least one of `k` runs drawn without replacement from `runs` runs, `passed` of which passed, passed: the unbiased pass@k estimate.
pub(crate) fn pass_at(runs: u32, passed: u32, k: u32) -> f64 {
    if runs == 0 || k == 0 || k > runs {
        return 0.0;
    }
    if runs - passed < k {
        return 1.0;
    }
    let none = (0..k).fold(1.0, |product, drawn| {
        product * f64::from(runs - passed - drawn) / f64::from(runs - drawn)
    });
    1.0 - none
}

/// The chance that all of `k` runs drawn without replacement from `runs` runs, `passed` of which passed, passed: how dependably a task is done, not just whether it can be.
pub(crate) fn pass_all(runs: u32, passed: u32, k: u32) -> f64 {
    if runs == 0 || k == 0 || k > runs || passed < k {
        return 0.0;
    }
    (0..k).fold(1.0, |product, drawn| {
        product * f64::from(passed - drawn) / f64::from(runs - drawn)
    })
}

/// The mean and the sample standard deviation of `values`; the deviation of fewer than two values is 0.
pub(crate) fn spread(values: &[f64]) -> (f64, f64) {
    if values.is_empty() {
        return (0.0, 0.0);
    }
    let n = values.len() as f64;
    let mean = values.iter().sum::<f64>() / n;
    if values.len() < 2 {
        return (mean, 0.0);
    }
    let variance = values
        .iter()
        .map(|value| (value - mean).powi(2))
        .sum::<f64>()
        / (n - 1.0);
    (mean, variance.sqrt())
}

/// The value below which a `share` from 0 to 1 of `values` lie, by the nearest rank; 0 for no values.
pub(crate) fn quantile(values: &[f64], share: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    let rank = (share * sorted.len() as f64).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

/// The runs of `outcomes` by contender, in the order contenders first appear, then by task.
fn grouped(outcomes: &[Outcome]) -> Vec<(&str, BTreeMap<&str, Vec<&Outcome>>)> {
    let mut groups: Vec<(&str, BTreeMap<&str, Vec<&Outcome>>)> = Vec::new();
    for outcome in outcomes {
        let place = match groups.iter().position(|(model, _)| *model == outcome.model) {
            Some(place) => place,
            None => {
                groups.push((&outcome.model, BTreeMap::new()));
                groups.len() - 1
            }
        };
        groups[place]
            .1
            .entry(&outcome.task)
            .or_default()
            .push(outcome);
    }
    groups
}

/// How many of `runs` passed.
fn passes(runs: &[&Outcome]) -> u32 {
    runs.iter().filter(|outcome| outcome.passed).count() as u32
}

/// The table of how far each contender's numbers can be trusted: the pass rate with its 95% interval, pass@k and pass^k averaged over tasks for the fewest runs any task had, how many tasks went both ways across runs, and the spread of seconds and tokens a run took.
pub(crate) fn render(outcomes: &[Outcome]) -> String {
    let mut table = String::from(
        "| contender | pass rate (95% CI) | k | pass@k | pass^k | flaky tasks | seconds mean ± sd | seconds p50 / p90 | tokens sent mean ± sd |\n|---|---|---|---|---|---|---|---|---|\n",
    );
    for (contender, tasks) in grouped(outcomes) {
        let runs: Vec<&Outcome> = tasks.values().flatten().copied().collect();
        let passed = passes(&runs);
        let total = runs.len() as u32;
        let (low, high) = wilson(passed, total);
        let k = tasks
            .values()
            .map(|runs| runs.len() as u32)
            .min()
            .unwrap_or(0);
        let count = tasks.len().max(1) as f64;
        let at = tasks
            .values()
            .map(|runs| pass_at(runs.len() as u32, passes(runs), k))
            .sum::<f64>()
            / count;
        let all = tasks
            .values()
            .map(|runs| pass_all(runs.len() as u32, passes(runs), k))
            .sum::<f64>()
            / count;
        let flaky = tasks
            .values()
            .filter(|runs| {
                let passed = passes(runs);
                passed > 0 && (passed as usize) < runs.len()
            })
            .count();
        let seconds: Vec<f64> = runs.iter().map(|outcome| outcome.seconds).collect();
        let tokens: Vec<f64> = runs.iter().map(|outcome| outcome.tokens as f64).collect();
        let (seconds_mean, seconds_sd) = spread(&seconds);
        let (tokens_mean, tokens_sd) = spread(&tokens);
        let _ = writeln!(
            table,
            "| {contender} | {:.0}% ({:.0}–{:.0}%) | {k} | {:.0}% | {:.0}% | {flaky}/{} | {seconds_mean:.0} ± {seconds_sd:.0} | {:.0} / {:.0} | {:.1}k ± {:.1}k |",
            f64::from(passed) * 100.0 / f64::from(total.max(1)),
            low * 100.0,
            high * 100.0,
            at * 100.0,
            all * 100.0,
            tasks.len(),
            quantile(&seconds, 0.5),
            quantile(&seconds, 0.9),
            tokens_mean / 1000.0,
            tokens_sd / 1000.0,
        );
    }
    table
}

/// The table of what each contender with a price in `prices` spent: in all, per run, and per run that passed.
pub(crate) fn costs(outcomes: &[Outcome], prices: &[(String, Price)]) -> String {
    let mut table =
        String::from("| contender | cost | cost / run | cost / pass |\n|---|---|---|---|\n");
    for (contender, tasks) in grouped(outcomes) {
        let Some((_, price)) = prices.iter().find(|(name, _)| name == contender) else {
            continue;
        };
        let runs: Vec<&Outcome> = tasks.values().flatten().copied().collect();
        let spent: f64 = runs
            .iter()
            .map(|outcome| price.cost(outcome.tokens, outcome.written))
            .sum();
        let passed = passes(&runs);
        let per_pass = if passed == 0 {
            "-".to_owned()
        } else {
            format!("${:.4}", spent / f64::from(passed))
        };
        let _ = writeln!(
            table,
            "| {contender} | ${spent:.4} | ${:.4} | {per_pass} |",
            spent / runs.len().max(1) as f64,
        );
    }
    table
}

#[cfg(test)]
/// Tests for the statistics of repeated runs.
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A run of `task` by `contender` that passed or not, took `seconds` and sent `tokens`.
    fn outcome(contender: &str, task: &str, passed: bool, seconds: f64, tokens: u64) -> Outcome {
        Outcome {
            model: contender.to_owned(),
            task: task.to_owned(),
            run: 1,
            passed,
            clean: passed,
            exact: passed,
            differing: Vec::new(),
            error: None,
            tools: BTreeMap::new(),
            context: 0,
            requests: 1,
            tokens,
            written: 1000,
            seconds,
            reply_lines: 1,
            refused_shell: 0,
        }
    }

    /// The Wilson interval holds the observed share, stays within 0 and 1, and narrows with more runs.
    #[test]
    fn wilson_intervals_are_sound() {
        assert_eq!(wilson(0, 0), (0.0, 1.0));
        let (low, high) = wilson(5, 10);
        assert!(low < 0.5 && high > 0.5);
        assert!((low - 0.2366).abs() < 0.001 && (high - 0.7634).abs() < 0.001);
        let (low, high) = wilson(10, 10);
        assert!(high <= 1.0 && low > 0.65);
        let (wide_low, wide_high) = wilson(3, 6);
        let (narrow_low, narrow_high) = wilson(30, 60);
        assert!(narrow_high - narrow_low < wide_high - wide_low);
    }

    /// pass@k and pass^k agree with counting by hand.
    #[test]
    fn pass_at_and_pass_all_are_estimated() {
        assert!((pass_at(3, 1, 1) - 1.0 / 3.0).abs() < 1e-9);
        assert!((pass_at(3, 1, 2) - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(pass_at(3, 0, 3), 0.0);
        assert_eq!(pass_at(3, 2, 2), 1.0);
        assert!((pass_all(3, 2, 2) - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(pass_all(3, 3, 3), 1.0);
        assert_eq!(pass_all(3, 2, 3), 0.0);
        assert_eq!(pass_all(0, 0, 1), 0.0);
    }

    /// The spread and quantiles of a few values are what they should be.
    #[test]
    fn spread_and_quantiles() {
        assert_eq!(spread(&[]), (0.0, 0.0));
        assert_eq!(spread(&[4.0]), (4.0, 0.0));
        let (mean, sd) = spread(&[2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0]);
        assert_eq!(mean, 5.0);
        assert!((sd - 2.138).abs() < 0.001);
        assert_eq!(quantile(&[3.0, 1.0, 2.0, 4.0], 0.5), 2.0);
        assert_eq!(quantile(&[3.0, 1.0, 2.0, 4.0], 0.9), 4.0);
        assert_eq!(quantile(&[], 0.5), 0.0);
    }

    /// The table counts a task that went both ways as flaky and gives each contender its row.
    #[test]
    fn the_table_shows_variance_by_contender() {
        let outcomes = [
            outcome("a", "one", true, 10.0, 1000),
            outcome("a", "one", false, 20.0, 3000),
            outcome("a", "two", true, 30.0, 2000),
            outcome("a", "two", true, 40.0, 2000),
            outcome("b", "one", false, 5.0, 500),
        ];
        let table = render(&outcomes);
        let rows: Vec<&str> = table.lines().collect();
        assert!(rows[2].starts_with("| a | 75% ("), "{}", rows[2]);
        assert!(rows[2].contains("| 2 | 100% | 50% | 1/2 |"), "{}", rows[2]);
        assert!(rows[2].contains("| 25 ± 13 | 20 / 40 |"), "{}", rows[2]);
        assert!(rows[3].starts_with("| b | 0% (0–"), "{}", rows[3]);
    }

    /// Prices are read and turned into costs, and contenders without one are left out.
    #[test]
    fn costs_follow_prices() {
        let price = Price::parse("3, 15").unwrap();
        assert_eq!(
            price,
            Price {
                sent: 3.0,
                written: 15.0
            }
        );
        assert!(Price::parse("3").is_err());
        assert!(Price::parse("-1,2").is_err());
        assert!((price.cost(1_000_000, 100_000) - 4.5).abs() < 1e-9);
        let outcomes = [
            outcome("a", "one", true, 1.0, 1_000_000),
            outcome("a", "one", false, 1.0, 1_000_000),
            outcome("b", "one", true, 1.0, 1_000_000),
        ];
        let table = costs(&outcomes, &[("a".to_owned(), price)]);
        assert!(
            table.contains("| a | $6.0300 | $3.0150 | $6.0300 |"),
            "{table}"
        );
        assert!(!table.contains("| b |"));
    }
}
