//! Background runs: work that goes on beyond the tool call that started it, owned by the conversation.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};

use omc_wire::Background;
use tokio::sync::Notify;
use tokio::task::AbortHandle;

use crate::ToolError;

/// How many finished runs are kept for `wait`; when more end unreported, the oldest are dropped.
const MAX_UNREPORTED: usize = 32;

/// What a background run came to, as `wait` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The handle the run was started under.
    pub id: String,
    /// The command or task that was run.
    pub label: String,
    /// Whether it ended without failing.
    pub ok: bool,
    /// What it printed or answered, or why it failed.
    pub text: String,
}

impl Finished {
    /// The report the model reads: the handle, the status and the output.
    pub fn report(&self) -> String {
        let status = if self.ok { "completed" } else { "failed" };
        format!(
            "{} {status}: {}\n{}",
            self.id,
            self.label.lines().next().unwrap_or_default(),
            self.text
        )
    }
}

/// A run that has not ended.
struct Running {
    /// The command or task.
    label: String,
    /// The call that started it, when known.
    call: Option<String>,
    /// How to stop it.
    handle: AbortHandle,
}

/// What the runs are doing.
#[derive(Default)]
struct State {
    /// The number the next handle gets.
    next: u64,
    /// The runs that have not ended, by handle.
    running: HashMap<String, Running>,
    /// The runs that ended and have not been waited on, oldest first.
    done: VecDeque<Finished>,
}

/// What the runs and their owner share.
struct Inner {
    /// What the runs are doing.
    state: Mutex<State>,
    /// Woken when a run ends.
    wake: Notify,
    /// Told when a run starts and when it ends.
    observer: Box<dyn Fn(Background) + Send + Sync>,
}

impl Inner {
    /// Locks the state, going on with it when a panic poisoned it.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Records that the run `id` ended with `outcome`, tells the observer and wakes a waiter; a run that was already taken out is ignored.
    fn finish(&self, id: &str, outcome: Result<String, String>) {
        let mut state = self.lock();
        let Some(run) = state.running.remove(id) else {
            return;
        };
        let (ok, text) = match outcome {
            Ok(text) => (true, text),
            Err(text) => (false, text),
        };
        state.done.push_back(Finished {
            id: id.to_owned(),
            label: run.label.clone(),
            ok,
            text,
        });
        if state.done.len() > MAX_UNREPORTED {
            state.done.pop_front();
        }
        (self.observer)(Background {
            id: id.to_owned(),
            label: run.label,
            running: false,
            call: run.call,
        });
        drop(state);
        self.wake.notify_one();
    }
}

/// Makes sure a run is recorded as ended even when its future panics.
struct Settle {
    /// What the run reports to.
    inner: Arc<Inner>,
    /// The run's handle.
    id: String,
    /// Whether the run reported its outcome.
    settled: bool,
}

impl Drop for Settle {
    /// Reports a failure when the run ended without an outcome.
    fn drop(&mut self) {
        if !self.settled {
            self.inner
                .finish(&self.id, Err("the run ended unexpectedly".to_owned()));
        }
    }
}

/// The background runs of a conversation.
///
/// Runs belong to the conversation and not to the turn that started them: they go on when the turn is cancelled, and every one still running is stopped when the conversation ends or the value is dropped.
pub struct Jobs {
    /// What the runs share.
    inner: Arc<Inner>,
}

impl Jobs {
    /// Creates a conversation's runs; `observer` is told when a run starts and when it ends.
    pub fn new(observer: impl Fn(Background) + Send + Sync + 'static) -> Self {
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::default()),
                wake: Notify::new(),
                observer: Box::new(observer),
            }),
        }
    }

    /// Starts `work` in the background under a new handle, which is returned at once.
    ///
    /// `label` says what the run is and `call` is the call that started it. The work ends with its output, or with the reason it failed.
    pub fn spawn(
        &self,
        label: String,
        call: Option<String>,
        work: impl Future<Output = Result<String, String>> + Send + 'static,
    ) -> String {
        let mut state = self.inner.lock();
        state.next += 1;
        let id = format!("bg-{}", state.next);
        let inner = self.inner.clone();
        let own = id.clone();
        let task = tokio::spawn(async move {
            let mut settle = Settle {
                inner,
                id: own.clone(),
                settled: false,
            };
            let outcome = work.await;
            settle.settled = true;
            settle.inner.finish(&own, outcome);
        });
        (self.inner.observer)(Background {
            id: id.clone(),
            label: label.clone(),
            running: true,
            call: call.clone(),
        });
        state.running.insert(
            id.clone(),
            Running {
                label,
                call,
                handle: task.abort_handle(),
            },
        );
        id
    }

    /// Waits until a run ends and returns the one that ended first of those not yet reported; a run that ended earlier is returned at once.
    ///
    /// Dropping the future, as a cancelled turn does, takes nothing away.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] at once when nothing is running and nothing finished is waiting to be reported.
    pub async fn wait(&self) -> Result<Finished, ToolError> {
        loop {
            {
                let mut state = self.inner.lock();
                if let Some(finished) = state.done.pop_front() {
                    return Ok(finished);
                }
                if state.running.is_empty() {
                    return Err(ToolError::Failed(
                        "nothing is running in the background".to_owned(),
                    ));
                }
            }
            self.inner.wake.notified().await;
        }
    }

    /// Stops every run that is still going and forgets the runs that ended.
    pub fn stop_all(&self) {
        let stopped: Vec<(String, Running)> = {
            let mut state = self.inner.lock();
            state.done.clear();
            state.running.drain().collect()
        };
        for (id, run) in stopped {
            run.handle.abort();
            (self.inner.observer)(Background {
                id,
                label: run.label,
                running: false,
                call: run.call,
            });
        }
    }
}

impl Drop for Jobs {
    /// Stops what is still running.
    fn drop(&mut self) {
        self.stop_all();
    }
}

#[cfg(test)]
/// Tests for background runs.
mod tests {
    use std::time::Duration;

    use super::*;

    /// A set of runs and the notices its observer received.
    fn jobs() -> (Jobs, Arc<Mutex<Vec<Background>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (Jobs::new(move |b| sink.lock().unwrap().push(b)), seen)
    }

    /// A run that ends after `millis` with `text`.
    async fn after(millis: u64, text: &'static str) -> Result<String, String> {
        tokio::time::sleep(Duration::from_millis(millis)).await;
        Ok(text.to_owned())
    }

    /// Two overlapping runs are reported in the order they end, and each is reported once.
    #[tokio::test]
    async fn overlapping_runs_finish_in_completion_order() {
        let (jobs, _) = jobs();
        let slow = jobs.spawn("slow".into(), None, after(300, "one"));
        let fast = jobs.spawn("fast".into(), None, after(50, "two"));
        let first = jobs.wait().await.unwrap();
        assert_eq!(
            (first.id.as_str(), first.text.as_str()),
            (fast.as_str(), "two")
        );
        let second = jobs.wait().await.unwrap();
        assert_eq!(
            (second.id.as_str(), second.text.as_str()),
            (slow.as_str(), "one")
        );
        assert!(jobs.wait().await.is_err());
    }

    /// With nothing running and nothing finished, waiting fails at once.
    #[tokio::test]
    async fn waiting_with_nothing_running_is_an_error() {
        let (jobs, _) = jobs();
        let result = tokio::time::timeout(Duration::from_secs(1), jobs.wait()).await;
        assert!(matches!(result, Ok(Err(ToolError::Failed(_)))));
    }

    /// A run that ended before anyone waited is reported by the next wait, with its failure status.
    #[tokio::test]
    async fn a_run_that_ended_earlier_is_reported_later() {
        let (jobs, _) = jobs();
        jobs.spawn("bad".into(), None, async { Err("boom".to_owned()) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let finished = jobs.wait().await.unwrap();
        assert!(!finished.ok);
        assert!(finished.report().contains("failed: bad\nboom"));
    }

    /// The observer hears of each run when it starts and when it ends, with the call that started it.
    #[tokio::test]
    async fn the_observer_hears_of_starts_and_ends() {
        let (jobs, seen) = jobs();
        let id = jobs.spawn("work".into(), Some("c1".into()), after(10, "x"));
        jobs.wait().await.unwrap();
        let seen = seen.lock().unwrap();
        let expected = |running| Background {
            id: id.clone(),
            label: "work".into(),
            running,
            call: Some("c1".into()),
        };
        assert_eq!(*seen, vec![expected(true), expected(false)]);
    }

    /// A wait that is dropped, as a cancelled turn drops it, leaves the runs going and reportable.
    #[tokio::test]
    async fn a_cancelled_wait_leaves_the_runs_going() {
        let (jobs, _) = jobs();
        jobs.spawn("work".into(), None, after(100, "late"));
        let cancelled = tokio::time::timeout(Duration::from_millis(10), jobs.wait()).await;
        assert!(cancelled.is_err());
        assert_eq!(jobs.wait().await.unwrap().text, "late");
    }

    /// Dropping the runs stops what is still going.
    #[tokio::test]
    async fn dropping_stops_the_runs() {
        /// Records in a flag that it was dropped.
        struct Flag(Arc<Mutex<bool>>);
        impl Drop for Flag {
            /// Records that the future holding the flag was dropped.
            fn drop(&mut self) {
                *self.0.lock().unwrap() = true;
            }
        }
        let (jobs, seen) = jobs();
        let dropped = Arc::new(Mutex::new(false));
        let flag = Flag(dropped.clone());
        jobs.spawn("forever".into(), None, async move {
            let _flag = flag;
            std::future::pending::<Result<String, String>>().await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        drop(jobs);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(*dropped.lock().unwrap());
        assert!(!seen.lock().unwrap().last().unwrap().running);
    }
}
