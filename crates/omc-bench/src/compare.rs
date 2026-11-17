//! What changed since an earlier benchmark: its outcomes read back from its results file and set against this one's, contender by contender and task by task, with a test of whether a change in passes is more than chance.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use crate::run::Outcome;

/// How unlikely a change in passes must be by chance, one-sided, before it counts as a regression or an improvement.
const SIGNIFICANCE: f64 = 0.05;

/// What a change in passes between two sets of runs amounts to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    /// The share that passed fell by more than chance explains.
    Regressed,
    /// The share that passed fell, but chance could explain it.
    Worse,
    /// The share that passed did not change.
    Same,
    /// The share that passed rose, but chance could explain it.
    Better,
    /// The share that passed rose by more than chance explains.
    Improved,
    /// Only the earlier benchmark ran it.
    Gone,
    /// Only this benchmark ran it.
    New,
}

impl Verdict {
    /// The word the report shows for the verdict.
    fn word(self) -> &'static str {
        match self {
            Self::Regressed => "REGRESSED",
            Self::Worse => "worse",
            Self::Same => "same",
            Self::Better => "better",
            Self::Improved => "IMPROVED",
            Self::Gone => "gone",
            Self::New => "new",
        }
    }
}

/// Passes and runs of one contender on one task, before and now.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Pair {
    /// Passes and runs in the earlier benchmark, when it ran this.
    before: Option<(u32, u32)>,
    /// Passes and runs in this benchmark, when it ran this.
    now: Option<(u32, u32)>,
}

/// Reads the outcomes an earlier benchmark wrote to `path`, its results file.
///
/// # Errors
///
/// Fails when the file cannot be read or is not a list of outcomes.
pub(crate) fn load(path: &Path) -> Result<Vec<Outcome>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("could not read {}: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not a results file: {error}", path.display()))
}

/// The natural logarithm of `n!`.
fn ln_factorial(n: u32) -> f64 {
    (2..=n).map(|i| f64::from(i).ln()).sum()
}

/// The chance of `k` passes among `drawn` runs drawn from `total` runs of which `passes` passed.
fn hypergeometric(k: u32, drawn: u32, passes: u32, total: u32) -> f64 {
    if k > passes || k > drawn || drawn - k > total - passes {
        return 0.0;
    }
    let ln_choose = |n: u32, r: u32| ln_factorial(n) - ln_factorial(r) - ln_factorial(n - r);
    (ln_choose(passes, k) + ln_choose(total - passes, drawn - k) - ln_choose(total, drawn)).exp()
}

/// The one-sided Fisher exact chance that the runs now pass at most `now.0` of `now.1` times, were they no worse than the `before.0` of `before.1` before: small when the drop is more than chance.
pub(crate) fn chance_of_drop(before: (u32, u32), now: (u32, u32)) -> f64 {
    let passes = before.0 + now.0;
    let total = before.1 + now.1;
    (0..=now.0)
        .map(|k| hypergeometric(k, now.1, passes, total))
        .sum::<f64>()
        .min(1.0)
}

/// What going from `before` to `now`, each passes and runs, amounts to.
pub(crate) fn judge(before: Option<(u32, u32)>, now: Option<(u32, u32)>) -> Verdict {
    let (before, now) = match (before, now) {
        (Some(before), Some(now)) if before.1 > 0 && now.1 > 0 => (before, now),
        (Some(_), _) => return Verdict::Gone,
        _ => return Verdict::New,
    };
    let rate_before = f64::from(before.0) / f64::from(before.1);
    let rate_now = f64::from(now.0) / f64::from(now.1);
    if rate_now < rate_before {
        if chance_of_drop(before, now) < SIGNIFICANCE {
            Verdict::Regressed
        } else {
            Verdict::Worse
        }
    } else if rate_now > rate_before {
        if chance_of_drop(now, before) < SIGNIFICANCE {
            Verdict::Improved
        } else {
            Verdict::Better
        }
    } else {
        Verdict::Same
    }
}

/// The passes and runs of `outcomes` by contender and task.
fn counted(outcomes: &[Outcome]) -> BTreeMap<(&str, &str), (u32, u32)> {
    let mut counts: BTreeMap<(&str, &str), (u32, u32)> = BTreeMap::new();
    for outcome in outcomes {
        let count = counts.entry((&outcome.model, &outcome.task)).or_default();
        count.0 += u32::from(outcome.passed);
        count.1 += 1;
    }
    counts
}

/// The comparison of `now` with `before`: a table of what changed for each contender and task, and whether any of it regressed by more than chance.
pub(crate) fn compare(before: &[Outcome], now: &[Outcome]) -> (String, bool) {
    let mut pairs: BTreeMap<(&str, &str), Pair> = BTreeMap::new();
    for (key, count) in counted(before) {
        pairs.entry(key).or_default().before = Some(count);
    }
    for (key, count) in counted(now) {
        pairs.entry(key).or_default().now = Some(count);
    }
    let mut table =
        String::from("| contender | task | before | now | change |\n|---|---|---|---|---|\n");
    let mut regressed = false;
    let mut totals: BTreeMap<&str, Pair> = BTreeMap::new();
    for ((contender, task), pair) in &pairs {
        let verdict = judge(pair.before, pair.now);
        regressed |= verdict == Verdict::Regressed;
        let total = totals.entry(contender).or_default();
        total.before = add(total.before, pair.before);
        total.now = add(total.now, pair.now);
        if verdict != Verdict::Same {
            let _ = writeln!(
                table,
                "| {contender} | {task} | {} | {} | {} |",
                fraction(pair.before),
                fraction(pair.now),
                verdict.word()
            );
        }
    }
    for (contender, total) in &totals {
        let verdict = judge(total.before, total.now);
        regressed |= verdict == Verdict::Regressed;
        let _ = writeln!(
            table,
            "| {contender} | (all tasks) | {} | {} | {} |",
            fraction(total.before),
            fraction(total.now),
            verdict.word()
        );
    }
    (table, regressed)
}

/// `count` and `more`, passes and runs, added together; nothing when neither has anything.
fn add(count: Option<(u32, u32)>, more: Option<(u32, u32)>) -> Option<(u32, u32)> {
    match (count, more) {
        (None, None) => None,
        (count, more) => {
            let (a, b) = (count.unwrap_or_default(), more.unwrap_or_default());
            Some((a.0 + b.0, a.1 + b.1))
        }
    }
}

/// Passes of runs written `passes/runs`, or a dash for none.
fn fraction(count: Option<(u32, u32)>) -> String {
    count.map_or_else(
        || "-".to_owned(),
        |(passes, runs)| format!("{passes}/{runs}"),
    )
}

#[cfg(test)]
/// Tests for setting a benchmark against an earlier one.
mod tests {
    use super::*;

    /// A run of `task` by `contender` that passed or not.
    fn outcome(contender: &str, task: &str, passed: bool) -> Outcome {
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
            requests: 0,
            tokens: 0,
            written: 0,
            seconds: 0.0,
        }
    }

    /// `passes` passing and `fails` failing runs of `task` by `contender`.
    fn runs(contender: &str, task: &str, passes: usize, fails: usize) -> Vec<Outcome> {
        let mut runs = vec![outcome(contender, task, true); passes];
        runs.extend(vec![outcome(contender, task, false); fails]);
        runs
    }

    /// Fisher's test gives the textbook chances.
    #[test]
    fn fisher_chances() {
        assert!((chance_of_drop((5, 5), (0, 5)) - 1.0 / 252.0).abs() < 1e-9);
        assert!((chance_of_drop((3, 3), (2, 3)) - 0.5).abs() < 1e-9);
        assert!((chance_of_drop((1, 2), (1, 2)) - 5.0 / 6.0).abs() < 1e-9);
    }

    /// Big drops regress, small ones are only worse, and missing sides are new or gone.
    #[test]
    fn verdicts() {
        assert_eq!(judge(Some((10, 10)), Some((2, 10))), Verdict::Regressed);
        assert_eq!(judge(Some((3, 3)), Some((2, 3))), Verdict::Worse);
        assert_eq!(judge(Some((2, 3)), Some((2, 3))), Verdict::Same);
        assert_eq!(judge(Some((2, 3)), Some((3, 3))), Verdict::Better);
        assert_eq!(judge(Some((0, 10)), Some((9, 10))), Verdict::Improved);
        assert_eq!(judge(Some((1, 1)), None), Verdict::Gone);
        assert_eq!(judge(None, Some((1, 1))), Verdict::New);
    }

    /// The table lists what changed, leaves out what did not, totals each contender, and says when anything regressed.
    #[test]
    fn benchmarks_are_compared() {
        let mut before = runs("a", "one", 10, 0);
        before.extend(runs("a", "two", 2, 1));
        before.extend(runs("a", "old", 1, 0));
        let mut now = runs("a", "one", 1, 9);
        now.extend(runs("a", "two", 2, 1));
        now.extend(runs("b", "one", 1, 0));
        let (table, regressed) = compare(&before, &now);
        assert!(regressed);
        assert!(
            table.contains("| a | one | 10/10 | 1/10 | REGRESSED |"),
            "{table}"
        );
        assert!(!table.contains("| a | two |"));
        assert!(table.contains("| a | old | 1/1 | - | gone |"));
        assert!(table.contains("| b | one | - | 1/1 | new |"));
        assert!(table.contains("| a | (all tasks) | 13/14 | 3/13 | REGRESSED |"));
        let (_, steady) = compare(&now, &now);
        assert!(!steady);
    }

    /// A results file is read back as it was written, and a bad one is refused.
    #[test]
    fn results_are_read_back() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("build")
            .join("bench-compare-test");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("results.json");
        let outcomes = runs("a", "one", 1, 1);
        std::fs::write(&path, serde_json::to_string(&outcomes).unwrap()).unwrap();
        assert_eq!(load(&path).unwrap(), outcomes);
        std::fs::write(&path, "{}").unwrap();
        assert!(load(&path).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
