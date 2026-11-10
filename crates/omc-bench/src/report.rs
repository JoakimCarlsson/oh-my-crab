//! What the runs of a benchmark add up to, as tables a reader compares harnesses and models by.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::run::Outcome;
use crate::tally::edits;

/// Some runs, added up.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sum {
    /// How many runs there were.
    runs: u32,
    /// How many of them left the files the task asks for.
    passed: u32,
    /// How many of them passed without an edit call that did not work.
    clean: u32,
    /// How many tool calls the model made.
    calls: u32,
    /// How many edit calls the model made.
    edits: u32,
    /// How many of the edit calls did not work.
    failed: u32,
    /// How many runs ended badly: the turn failed, the time ran out or the harness stopped.
    errors: u32,
    /// How many requests the model was sent.
    requests: u32,
    /// The tokens the model was sent, added up.
    tokens: u64,
    /// How long the runs took together, in seconds.
    seconds: f64,
}

impl Sum {
    /// The sum of `outcomes`.
    fn of<'a>(outcomes: impl IntoIterator<Item = &'a Outcome>) -> Self {
        let mut sum = Self::default();
        for outcome in outcomes {
            let edits = edits(&outcome.tools);
            sum.runs += 1;
            sum.passed += u32::from(outcome.passed);
            sum.clean += u32::from(outcome.clean);
            sum.calls += outcome.tools.values().map(|uses| uses.calls).sum::<u32>();
            sum.edits += edits.calls;
            sum.failed += edits.failed;
            sum.errors += u32::from(outcome.error.is_some());
            sum.requests += outcome.requests;
            sum.tokens += outcome.tokens;
            sum.seconds += outcome.seconds;
        }
        sum
    }

    /// `total` for one run, on average.
    fn each(&self, total: f64) -> f64 {
        total / f64::from(self.runs.max(1))
    }
}

/// `part` of `whole` as a percentage, or a dash when there is nothing to take a part of.
fn share(part: u32, whole: u32) -> String {
    if whole == 0 {
        "-".to_owned()
    } else {
        format!("{:.0}%", f64::from(part) * 100.0 / f64::from(whole))
    }
}

/// `tokens` in thousands, with one decimal.
fn thousands(tokens: f64) -> String {
    format!("{:.1}k", tokens / 1000.0)
}

/// What was measured in `outcomes`, each harness and model once, in the order they first appear.
fn contenders(outcomes: &[Outcome]) -> Vec<&str> {
    let mut contenders: Vec<&str> = Vec::new();
    for outcome in outcomes {
        if !contenders.contains(&outcome.model.as_str()) {
            contenders.push(&outcome.model);
        }
    }
    contenders
}

/// The table of `outcomes` by contender: how many runs passed and passed cleanly, what a run took in requests, tool calls, tokens sent and time, how many edit calls failed, and how many runs ended badly.
fn by_contender(outcomes: &[Outcome]) -> String {
    let mut table = String::from(
        "| contender | runs | passed | clean | requests / run | tool calls / run | failed edit calls | tokens sent / run | seconds / run | errors |\n|---|---|---|---|---|---|---|---|---|---|\n",
    );
    for contender in contenders(outcomes) {
        let sum = Sum::of(outcomes.iter().filter(|outcome| outcome.model == contender));
        let _ = writeln!(
            table,
            "| {contender} | {} | {} | {} | {:.1} | {:.1} | {} | {} | {:.0} | {} |",
            sum.runs,
            share(sum.passed, sum.runs),
            share(sum.clean, sum.runs),
            sum.each(f64::from(sum.requests)),
            sum.each(f64::from(sum.calls)),
            share(sum.failed, sum.edits),
            thousands(sum.each(sum.tokens as f64)),
            sum.each(sum.seconds),
            sum.errors,
        );
    }
    table
}

/// The table of `outcomes` by task: for each contender, how many of the task's runs passed, and the requests, tokens sent and seconds a run of it took.
fn by_task(outcomes: &[Outcome]) -> String {
    let contenders = contenders(outcomes);
    let mut runs: BTreeMap<&str, BTreeMap<&str, Vec<&Outcome>>> = BTreeMap::new();
    for outcome in outcomes {
        runs.entry(&outcome.task)
            .or_default()
            .entry(&outcome.model)
            .or_default()
            .push(outcome);
    }
    let mut table = format!(
        "| task | {} |\n|---|{}\n",
        contenders.join(" | "),
        "---|".repeat(contenders.len())
    );
    for (task, cells) in runs {
        let row: Vec<String> = contenders
            .iter()
            .map(|contender| match cells.get(contender) {
                None => "-".to_owned(),
                Some(runs) => {
                    let sum = Sum::of(runs.iter().copied());
                    format!(
                        "{}/{} · {:.1} req · {} · {:.0}s",
                        sum.passed,
                        sum.runs,
                        sum.each(f64::from(sum.requests)),
                        thousands(sum.each(sum.tokens as f64)),
                        sum.each(sum.seconds),
                    )
                }
            })
            .collect();
        let _ = writeln!(table, "| {task} | {} |", row.join(" | "));
    }
    table
}

/// The report of `outcomes`: the table by contender, then the table by task.
pub(crate) fn render(outcomes: &[Outcome]) -> String {
    format!("{}\n{}", by_contender(outcomes), by_task(outcomes))
}

/// One line that says how the run `outcome` went, for showing as the runs finish.
pub(crate) fn line(outcome: &Outcome) -> String {
    let verdict = match (&outcome.error, outcome.passed, outcome.clean) {
        (Some(reason), _, _) => format!("error: {}", reason.lines().next().unwrap_or_default()),
        (None, true, true) => "passed".to_owned(),
        (None, true, false) => "passed after a failed edit".to_owned(),
        (None, false, _) => format!("failed: {}", outcome.differing.join(", ")),
    };
    format!(
        "{} {} #{}: {verdict} ({:.0}s)",
        outcome.model, outcome.task, outcome.run, outcome.seconds
    )
}

#[cfg(test)]
/// Tests for the report.
mod tests {
    use super::*;
    use crate::tally::Uses;

    /// A run of `task` by `contender` that passed or not, with `calls` edit calls of which `failed` did not work.
    fn outcome(contender: &str, task: &str, passed: bool, calls: u32, failed: u32) -> Outcome {
        Outcome {
            model: contender.to_owned(),
            task: task.to_owned(),
            run: 1,
            passed,
            clean: passed && failed == 0,
            differing: if passed {
                Vec::new()
            } else {
                vec!["a.txt".to_owned()]
            },
            error: None,
            tools: BTreeMap::from([
                ("edit".to_owned(), Uses { calls, failed }),
                (
                    "read".to_owned(),
                    Uses {
                        calls: 1,
                        failed: 0,
                    },
                ),
            ]),
            context: 1000,
            requests: calls + 2,
            tokens: 2500,
            seconds: 10.0,
        }
    }

    /// Contenders keep the order they were given in, shares are of the runs and of the edit calls, and each task has a cell for each contender.
    #[test]
    fn runs_are_added_up_by_contender_and_by_task() {
        let outcomes = [
            outcome("b", "one", true, 1, 0),
            outcome("b", "two", true, 3, 1),
            outcome("a", "one", false, 2, 2),
        ];
        let report = render(&outcomes);
        let rows: Vec<&str> = report.lines().collect();
        assert_eq!(
            rows[2],
            "| b | 2 | 100% | 50% | 4.0 | 3.0 | 25% | 2.5k | 10 | 0 |"
        );
        assert_eq!(
            rows[3],
            "| a | 1 | 0% | 0% | 4.0 | 3.0 | 100% | 2.5k | 10 | 0 |"
        );
        assert!(report.contains("| task | b | a |"));
        assert!(
            report.contains("| one | 1/1 · 3.0 req · 2.5k · 10s | 0/1 · 4.0 req · 2.5k · 10s |")
        );
        assert!(report.contains("| two | 1/1 · 5.0 req · 2.5k · 10s | - |"));
    }

    /// The line of a run says whether it passed, passed after a failed edit, left the wrong files or ended badly.
    #[test]
    fn a_run_is_told_in_a_line() {
        assert_eq!(
            line(&outcome("m", "one", true, 1, 0)),
            "m one #1: passed (10s)"
        );
        assert!(line(&outcome("m", "one", true, 2, 1)).contains("passed after a failed edit"));
        assert!(line(&outcome("m", "one", false, 1, 0)).contains("failed: a.txt"));
        let mut broken = outcome("m", "one", false, 0, 0);
        broken.error = Some("timed out after 300s\nmore".to_owned());
        assert!(line(&broken).contains("error: timed out after 300s ("));
    }
}
