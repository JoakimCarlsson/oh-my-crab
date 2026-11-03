//! The checkpoint the agent marks in a conversation, and the report that replaces everything after it.

use std::sync::{Mutex, MutexGuard};

/// Where the conversation stands with its checkpoint.
#[derive(Default)]
enum Phase {
    /// No checkpoint is active.
    #[default]
    Idle,
    /// The `checkpoint` tool ran, and the batch of tool calls it was in has not finished yet.
    Pending,
    /// The checkpoint is at this many messages of history, the results of the call that made it included.
    Marked(usize),
}

/// What the checkpoint keeps as the turn goes on.
#[derive(Default)]
struct State {
    /// Whether a checkpoint is active and where.
    phase: Phase,
    /// The report of a `rewind` call whose batch has not finished yet.
    report: Option<String>,
}

/// The single checkpoint of a conversation and the pending request to rewind to it.
///
/// The tools only record what the model asked for. The agent carries it out when the batch of tool calls has finished, so a request never sees a half-pruned history.
#[derive(Default)]
pub struct Checkpoint {
    /// What changes as the agent works.
    state: Mutex<State>,
}

impl Checkpoint {
    /// A conversation without a checkpoint.
    pub fn new() -> Self {
        Self::default()
    }

    /// Takes the state, whatever a panicking writer left.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Starts a checkpoint at the current position, which the agent fixes when the batch of tool calls has finished.
    ///
    /// # Errors
    ///
    /// Returns a message for the model when a checkpoint is already active; nothing is changed then.
    pub fn begin(&self) -> Result<(), String> {
        let mut state = self.lock();
        if !matches!(state.phase, Phase::Idle) {
            return Err(
                "a checkpoint is already active; finish with rewind and your report before marking another. Nothing was changed.".to_owned(),
            );
        }
        state.phase = Phase::Pending;
        Ok(())
    }

    /// Asks to replace everything since the checkpoint with `report` once the current batch of tool calls has finished.
    ///
    /// # Errors
    ///
    /// Returns a message for the model when there is no checkpoint to rewind to, when the checkpoint was made in this very batch, when a rewind was already asked for, or when `report` is empty or only whitespace; nothing is changed then.
    pub fn request_rewind(&self, report: &str) -> Result<(), String> {
        let mut state = self.lock();
        match state.phase {
            Phase::Idle => {
                return Err(
                    "there is no active checkpoint; call checkpoint first, explore, then call rewind. Nothing was changed.".to_owned(),
                );
            }
            Phase::Pending => {
                return Err(
                    "the checkpoint was made in this same batch of calls, so there is nothing to rewind yet; explore first, then call rewind in a later step. Nothing was changed.".to_owned(),
                );
            }
            Phase::Marked(_) => {}
        }
        if state.report.is_some() {
            return Err(
                "a rewind was already requested in this batch. Nothing was changed.".to_owned(),
            );
        }
        let report = report.trim();
        if report.is_empty() {
            return Err(
                "the report is empty; say what you found so it can replace the exploration, then call rewind again. Nothing was changed.".to_owned(),
            );
        }
        state.report = Some(report.to_owned());
        Ok(())
    }

    /// Fixes a checkpoint made in the batch that has just finished at `length` messages of history.
    pub fn settle(&self, length: usize) {
        let mut state = self.lock();
        if matches!(state.phase, Phase::Pending) {
            state.phase = Phase::Marked(length);
        }
    }

    /// Carries out a requested rewind: returns how many messages of history to keep and the report that goes in their place, and ends the checkpoint. Nothing when none was asked for.
    pub fn take_rewind(&self) -> Option<(usize, String)> {
        let mut state = self.lock();
        let Phase::Marked(length) = state.phase else {
            return None;
        };
        let report = state.report.take()?;
        state.phase = Phase::Idle;
        Some((length, report))
    }

    /// Whether a checkpoint is active.
    pub fn active(&self) -> bool {
        !matches!(self.lock().phase, Phase::Idle)
    }

    /// Forgets the checkpoint and any pending rewind, for a turn that ended or whose history was discarded.
    pub fn clear(&self) {
        *self.lock() = State::default();
    }
}

#[cfg(test)]
/// Tests for the checkpoint.
mod tests {
    use super::*;

    /// One checkpoint at a time, and it is only fixed once its batch has finished.
    #[test]
    fn allows_one_checkpoint() {
        let checkpoint = Checkpoint::new();
        checkpoint.begin().unwrap();
        assert!(checkpoint.begin().unwrap_err().contains("already active"));
        checkpoint.settle(4);
        assert!(checkpoint.begin().is_err());
        assert!(checkpoint.active());
    }

    /// A rewind needs a settled checkpoint and a report with words in it.
    #[test]
    fn rewind_needs_a_checkpoint_and_a_report() {
        let checkpoint = Checkpoint::new();
        assert!(
            checkpoint
                .request_rewind("x")
                .unwrap_err()
                .contains("no active checkpoint")
        );
        checkpoint.begin().unwrap();
        assert!(
            checkpoint
                .request_rewind("x")
                .unwrap_err()
                .contains("same batch")
        );
        checkpoint.settle(2);
        assert!(
            checkpoint
                .request_rewind("  \n ")
                .unwrap_err()
                .contains("empty")
        );
        assert!(checkpoint.take_rewind().is_none());
        assert!(checkpoint.active());
    }

    /// A rewind is carried out once, with the trimmed report and the length that was marked, and ends the checkpoint.
    #[test]
    fn rewind_happens_once() {
        let checkpoint = Checkpoint::new();
        checkpoint.begin().unwrap();
        checkpoint.settle(6);
        checkpoint.request_rewind("  found it ").unwrap();
        assert!(checkpoint.request_rewind("again").is_err());
        assert_eq!(checkpoint.take_rewind(), Some((6, "found it".to_owned())));
        assert!(checkpoint.take_rewind().is_none());
        assert!(!checkpoint.active());
        checkpoint.begin().unwrap();
    }

    /// Clearing forgets the checkpoint and a pending report.
    #[test]
    fn clear_forgets_everything() {
        let checkpoint = Checkpoint::new();
        checkpoint.begin().unwrap();
        checkpoint.settle(1);
        checkpoint.request_rewind("r").unwrap();
        checkpoint.clear();
        assert!(!checkpoint.active());
        assert!(checkpoint.take_rewind().is_none());
    }
}
