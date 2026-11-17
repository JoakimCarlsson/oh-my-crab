//! What each subagent costs while it runs: every answered model request made on behalf of a subagent run is added to that run's [`Meter`] and to the meters of the subagents that started it.
//!
//! The conversation's model wrappers price each request as they add it to what the whole conversation cost, and hand the same tokens and dollars to [`charge`]. That adds them to the meter of the subagent whose work the request was made for, found through a task-local value [`metered`] sets around the subagent's run, so the conversation's own totals are kept apart and counted once. A request made outside any subagent run, as the main agent's, is charged to no meter.

use std::future::Future;
use std::sync::{Arc, Mutex};

use omc_providers::Tokens;
use omc_wire::{SubagentSpend, micros};

tokio::task_local! {
    /// The meter of the subagent run the current task works for.
    static CURRENT: Arc<Meter>;
}

/// Something told what a subagent has cost so far each time that grows.
pub(crate) type Watcher = Arc<dyn Fn(SubagentSpend) + Send + Sync>;

/// What one subagent has cost so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(crate) struct Tally {
    /// How many requests were answered.
    pub(crate) requests: u64,
    /// The tokens they were billed for.
    pub(crate) tokens: Tokens,
    /// What the priced ones cost, in US dollars.
    pub(crate) dollars: f64,
    /// How many of them had known prices; `dollars` says something only when some had.
    pub(crate) priced: u64,
}

impl Tally {
    /// Whether any request counted had known prices.
    pub(crate) fn is_priced(&self) -> bool {
        self.priced > 0
    }

    /// The tally as a client is told it.
    pub(crate) fn told(&self) -> SubagentSpend {
        SubagentSpend {
            requests: self.requests,
            input: self.tokens.input,
            output: self.tokens.output,
            cache_read: self.tokens.cache_read,
            cache_write: self.tokens.cache_write,
            cost: self.is_priced().then(|| micros(self.dollars)),
        }
    }
}

/// What one subagent costs, across every follow-up it runs, with the meter of the subagent that started it, which every charge reaches as well.
#[derive(Default)]
pub(crate) struct Meter {
    /// The meter of the subagent that started this one; none for one the main agent started.
    parent: Option<Arc<Meter>>,
    /// What it cost so far.
    tally: Mutex<Tally>,
    /// Who is told when the tally grows, while the subagent runs.
    watcher: Mutex<Option<Watcher>>,
}

impl Meter {
    /// A meter for a subagent started by the subagent metered by `parent`, or by the main agent when that is none.
    pub(crate) fn below(parent: Option<Arc<Meter>>) -> Arc<Self> {
        Arc::new(Self {
            parent,
            ..Self::default()
        })
    }

    /// What the subagent cost so far.
    pub(crate) fn tally(&self) -> Tally {
        *self
            .tally
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Has `watcher` told what the subagent cost each time that grows, or nobody when it is none.
    pub(crate) fn watch(&self, watcher: Option<Watcher>) {
        *self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = watcher;
    }

    /// Adds a request billed `tokens`, which cost `dollars` when its prices are known, to this meter and every meter above it, telling each one's watcher.
    pub(crate) fn add(&self, tokens: &Tokens, dollars: Option<f64>) {
        let tally = {
            let mut tally = self
                .tally
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            tally.requests += 1;
            tally.tokens.input += tokens.input;
            tally.tokens.output += tokens.output;
            tally.tokens.cache_read += tokens.cache_read;
            tally.tokens.cache_write += tokens.cache_write;
            if let Some(dollars) = dollars {
                tally.priced += 1;
                tally.dollars += dollars;
            }
            *tally
        };
        let watcher = self
            .watcher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        if let Some(watcher) = watcher {
            watcher(tally.told());
        }
        if let Some(parent) = &self.parent {
            parent.add(tokens, dollars);
        }
    }
}

/// Adds a model request billed `tokens`, which cost `dollars` when the model's prices are known, to the meter of the subagent run the current task works for, and to those of the subagents above it; nothing happens outside a subagent run.
pub fn charge(tokens: &Tokens, dollars: Option<f64>) {
    let _ = CURRENT.try_with(|meter| meter.add(tokens, dollars));
}

/// Runs `work` with the model requests it makes charged to `meter`.
pub(crate) async fn metered<F: Future>(meter: Arc<Meter>, work: F) -> F::Output {
    CURRENT.scope(meter, work).await
}

/// The line a subagent's result ends with to say what it cost: this run's `run` dollars and, when it ran before, the `total` across its runs; none when no model it asked has known prices.
pub(crate) fn cost_line(run: &Tally, total: &Tally) -> Option<String> {
    if !run.is_priced() {
        return None;
    }
    if total.requests > run.requests {
        Some(format!(
            "(Subagent cost: ${:.2} this run, ${:.2} in all.)",
            run.dollars, total.dollars
        ))
    } else {
        Some(format!("(Subagent cost: ${:.2}.)", run.dollars))
    }
}

/// The tally of what was charged between `before` and `after`, two tallies of one meter.
pub(crate) fn since(before: &Tally, after: &Tally) -> Tally {
    Tally {
        requests: after.requests - before.requests,
        tokens: Tokens {
            input: after.tokens.input - before.tokens.input,
            output: after.tokens.output - before.tokens.output,
            cache_read: after.tokens.cache_read - before.tokens.cache_read,
            cache_write: after.tokens.cache_write - before.tokens.cache_write,
        },
        dollars: after.dollars - before.dollars,
        priced: after.priced - before.priced,
    }
}

#[cfg(test)]
/// Tests for metering subagents.
mod tests {
    use super::*;

    /// Tokens of `input` and `output`.
    fn tokens(input: u64, output: u64) -> Tokens {
        Tokens {
            input,
            output,
            ..Tokens::default()
        }
    }

    /// A charge inside a run reaches its meter and those above it, one outside reaches none, and an unpriced one counts tokens only.
    #[tokio::test]
    async fn charges_reach_the_run_and_its_parents() {
        let parent = Meter::below(None);
        let child = Meter::below(Some(parent.clone()));
        charge(&tokens(1, 1), Some(1.0));
        metered(parent.clone(), async { charge(&tokens(10, 1), Some(0.5)) }).await;
        metered(child.clone(), async { charge(&tokens(5, 2), Some(0.25)) }).await;
        metered(child.clone(), async { charge(&tokens(3, 0), None) }).await;
        let (parent, child) = (parent.tally(), child.tally());
        assert_eq!(child.requests, 2);
        assert_eq!(child.tokens.input, 8);
        assert!((child.dollars - 0.25).abs() < 1e-9);
        assert_eq!(parent.requests, 3);
        assert!((parent.dollars - 0.75).abs() < 1e-9);
        assert_eq!(parent.told().cost, Some(750_000));
    }

    /// An unpriced meter tells no cost and earns no cost line; a priced one says what the run cost, and the total once it ran before.
    #[test]
    fn cost_lines_follow_the_prices() {
        let meter = Meter::below(None);
        meter.add(&tokens(1, 1), None);
        assert_eq!(meter.tally().told().cost, None);
        assert_eq!(cost_line(&meter.tally(), &meter.tally()), None);
        let first = meter.tally();
        meter.add(&tokens(1, 1), Some(0.5));
        let run = since(&first, &meter.tally());
        assert_eq!(
            cost_line(&run, &meter.tally()).unwrap(),
            "(Subagent cost: $0.50 this run, $0.50 in all.)"
        );
        let fresh = Meter::below(None);
        fresh.add(&tokens(1, 1), Some(1.25));
        assert_eq!(
            cost_line(&fresh.tally(), &fresh.tally()).unwrap(),
            "(Subagent cost: $1.25.)"
        );
    }
}
