//! The subagents of a conversation: the running ones, each under the id [`crate::Nesting`] gives it, so that the reader can stop one of them without stopping the turn, and the finished ones kept for a while, each under a handle, so that a follow-up reaches one with everything it learned.
//!
//! A subagent is announced with an [`AgentEvent::Subagent`] when it starts and again when it ends. Stopping one stops the subagents it started too, since their ids begin with its own.
//!
//! A finished subagent is kept with its conversation until a follow-up takes it, it grows older than the roster allows, or more are kept than the roster allows, when the one kept longest goes; the finished subagents it started itself go with it. The roster remembers why each handle it gave out is not kept, so that a follow-up to one that is gone is told why.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::AgentEvent;
use crate::task::{EventSink, Run};

/// How many finished subagents a conversation keeps for follow-ups by default.
pub const KEPT_MOST: usize = 64;

/// How long a finished subagent is kept for follow-ups by default, after it last finished.
pub const KEPT_FOR: Duration = Duration::from_secs(6 * 60 * 60);

/// Why a subagent that was given a handle is not kept for follow-ups under it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fate {
    /// More finished subagents were kept than the roster allows, and it was the one kept longest.
    Evicted,
    /// It was kept longer than the roster allows after it last finished.
    Expired,
    /// The subagent with this handle, which started it, went, and it went along.
    WentWith(String),
    /// It worked in a worktree of its own, which was removed when it ended.
    Isolated,
    /// It handed back a structured result.
    Structured,
    /// It ended with an error.
    Failed,
    /// It was stopped before it ended.
    Stopped,
    /// A follow-up to it runs now.
    Busy,
}

impl Fate {
    /// Says why the subagent is not kept, in words for the agent that asked for it; `most` and `age` are the roster's limits.
    fn reason(&self, most: usize, age: Duration) -> String {
        match self {
            Self::Evicted => format!(
                "it was evicted to make room: a conversation keeps at most {most} finished subagents, and it had been kept longest"
            ),
            Self::Expired => format!(
                "it expired: finished subagents are kept for {} minutes after they last finish",
                age.as_secs() / 60
            ),
            Self::WentWith(keeper) => {
                format!("the subagent {keeper} that started it is no longer kept, and it went with it")
            }
            Self::Isolated => {
                "it was isolated: it worked in a worktree of its own, which was removed when it ended, so it is never kept".to_owned()
            }
            Self::Structured => {
                "it returned structured output, so that its result stays the data it handed back; such subagents are never kept".to_owned()
            }
            Self::Failed => "it failed, which ends a subagent for good".to_owned(),
            Self::Stopped => "it was stopped before it finished, which ends a subagent for good".to_owned(),
            Self::Busy => "a follow-up to it runs now; wait for that one to end".to_owned(),
        }
    }
}

/// How a subagent stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubagentState {
    /// It runs, or waits for a seat to run.
    Running,
    /// It ended with a result.
    Finished,
    /// It ended with an error.
    Failed,
    /// The reader stopped it, or what it ran in was stopped.
    Stopped,
}

/// A subagent as it is announced when it starts and when it ends.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentNotice {
    /// Its id among the subagents of the conversation.
    pub id: String,
    /// The id of the `task` call that started it.
    pub call: String,
    /// The kind of subagent.
    pub agent: String,
    /// What it is doing, in a few words.
    pub label: String,
    /// What the model it asks is called, when that is known.
    pub model: Option<String>,
    /// The work category it was started in, when it was started in one.
    pub category: Option<String>,
    /// Whether it runs in the background.
    pub background: bool,
    /// How it stands.
    pub state: SubagentState,
    /// What it has cost so far, its own requests and those of the subagents it started, across its follow-ups; none before its first answered request.
    pub spent: Option<omc_wire::SubagentSpend>,
}

/// One running subagent: its id, and the switch that stops it.
struct Entry {
    /// Its id among the subagents of the conversation.
    id: String,
    /// Set to `true` to stop it.
    stop: watch::Sender<bool>,
}

/// One finished subagent kept for follow-ups.
struct Retired {
    /// The handle of the subagent that started it; empty when the main agent did.
    keeper: String,
    /// When it finished.
    since: Instant,
    /// The subagent, with its conversation.
    run: Run,
}

/// What the roster holds.
#[derive(Default)]
struct Inner {
    /// The key the next subagent enrolled gets.
    next: u64,
    /// The running subagents, by key.
    entries: HashMap<u64, Entry>,
    /// How many handles were given out.
    handles: u64,
    /// The finished subagents kept for follow-ups, by handle.
    retired: HashMap<String, Retired>,
    /// Why each handle given out that is not kept now is not.
    fates: HashMap<String, Fate>,
}

impl Inner {
    /// Takes out the finished subagent kept as `handle`, which goes for `fate`, and, at every depth, the finished subagents it started, and returns them all; each is remembered with why it went.
    fn discard(&mut self, handle: &str, fate: Fate) -> Vec<Retired> {
        let mut gone = Vec::new();
        let mut handles = vec![(handle.to_owned(), fate)];
        while let Some((handle, fate)) = handles.pop() {
            if let Some(retired) = self.retired.remove(&handle) {
                gone.push(retired);
                self.fates.insert(handle.clone(), fate);
            }
            handles.extend(
                self.retired
                    .iter()
                    .filter(|(_, retired)| retired.keeper == handle)
                    .map(|(below, _)| (below.clone(), Fate::WentWith(handle.clone()))),
            );
        }
        gone
    }

    /// Takes out the finished subagents kept for longer than `age` and, while more than `most` are kept, the one kept longest, each with the finished subagents it started, and returns them all.
    fn prune(&mut self, most: usize, age: Duration) -> Vec<Retired> {
        let now = Instant::now();
        let old: Vec<String> = self
            .retired
            .iter()
            .filter(|(_, retired)| now.duration_since(retired.since) >= age)
            .map(|(handle, _)| handle.clone())
            .collect();
        let mut gone = Vec::new();
        for handle in old {
            gone.extend(self.discard(&handle, Fate::Expired));
        }
        while self.retired.len() > most {
            let oldest = self
                .retired
                .iter()
                .min_by_key(|(_, retired)| retired.since)
                .map(|(handle, _)| handle.clone());
            match oldest {
                Some(handle) => gone.extend(self.discard(&handle, Fate::Evicted)),
                None => break,
            }
        }
        gone
    }
}

/// The subagents of a conversation, at every level, shared by every `task` tool of it: the running ones, and the finished ones kept for follow-ups.
pub struct Roster {
    /// The subagents.
    inner: Mutex<Inner>,
    /// The most finished subagents kept at once.
    most: usize,
    /// How long a finished subagent is kept.
    age: Duration,
    /// The most US dollars one subagent run may spend before it is stopped, in millionths of a dollar; none for no limit.
    budget: std::sync::atomic::AtomicU64,
}

/// The value of [`Roster::budget`]'s field that means no limit.
const NO_BUDGET: u64 = u64::MAX;

impl Default for Roster {
    /// A roster keeping [`KEPT_MOST`] finished subagents for [`KEPT_FOR`] each.
    fn default() -> Self {
        Self::keeping(KEPT_MOST, KEPT_FOR)
    }
}

impl Roster {
    /// A roster keeping at most `most` finished subagents for follow-ups, each for `age`; with `most` at zero none is kept.
    pub fn keeping(most: usize, age: Duration) -> Self {
        Self {
            inner: Mutex::new(Inner::default()),
            most,
            age,
            budget: std::sync::atomic::AtomicU64::new(NO_BUDGET),
        }
    }

    /// The most US dollars one subagent run may spend before it is stopped after the step it is in; none when there is no limit, the default.
    pub fn budget(&self) -> Option<f64> {
        match self.budget.load(std::sync::atomic::Ordering::Relaxed) {
            NO_BUDGET => None,
            micros => Some(omc_wire::dollars(micros)),
        }
    }

    /// Sets the most US dollars one subagent run may spend to `dollars`, for the runs that start from now on; none, or an amount that is not above zero, removes the limit.
    pub fn set_budget(&self, dollars: Option<f64>) {
        let micros = dollars
            .filter(|dollars| dollars.is_finite() && *dollars > 0.0)
            .map_or(NO_BUDGET, |dollars| {
                omc_wire::micros(dollars).min(NO_BUDGET - 1)
            });
        self.budget
            .store(micros, std::sync::atomic::Ordering::Relaxed);
    }

    /// How many finished subagents are kept for follow-ups now.
    pub fn kept(&self) -> usize {
        let mut inner = self.lock();
        let gone = inner.prune(self.most, self.age);
        let kept = inner.retired.len();
        drop(inner);
        drop(gone);
        kept
    }

    /// A handle no subagent of the conversation has yet, for a subagent to be kept under once it finished.
    pub(crate) fn handle(&self) -> String {
        let mut inner = self.lock();
        inner.handles += 1;
        format!("sub-{}", inner.handles)
    }

    /// Keeps the finished subagent `run`, started by the subagent with the handle `keeper` or by the main agent when that is empty, under its own handle, and returns whether it is kept; the subagents kept too long, and the ones kept longest when there are too many, go.
    pub(crate) fn retire(&self, keeper: &str, run: Run) -> bool {
        let handle = run.handle.clone();
        let mut inner = self.lock();
        inner.fates.remove(&handle);
        inner.retired.insert(
            handle.clone(),
            Retired {
                keeper: keeper.to_owned(),
                since: Instant::now(),
                run,
            },
        );
        let gone = inner.prune(self.most, self.age);
        let kept = inner.retired.contains_key(&handle);
        drop(inner);
        drop(gone);
        kept
    }

    /// Remembers that the subagent with the handle `handle` is not kept, and why, so that a follow-up to it is told.
    pub(crate) fn forget(&self, handle: &str, fate: Fate) {
        self.lock().fates.insert(handle.to_owned(), fate);
    }

    /// Takes the finished subagent kept as `handle` off the roster, for a follow-up, when the subagent with the handle `keeper`, or the main agent when that is empty, started it and it is not kept too long; the finished subagents it started stay.
    ///
    /// # Errors
    ///
    /// Returns why no such subagent is kept, in words for the agent that asked: it went and why, it is not one `keeper` started, or the handle was never given out in this conversation as this process holds it.
    pub(crate) fn recall(&self, keeper: &str, handle: &str) -> Result<Run, String> {
        let mut inner = self.lock();
        let gone = inner.prune(self.most, self.age);
        let found = match inner.retired.get(handle) {
            Some(retired) if retired.keeper == keeper => Ok(()),
            Some(_) => Err("it is not one you started".to_owned()),
            None => Err(match inner.fates.get(handle) {
                Some(fate) => fate.reason(self.most, self.age),
                None => "no subagent got that handle in this conversation since crab started it; handles do not survive a reopened conversation".to_owned(),
            }),
        };
        let run = found.and_then(|()| {
            inner
                .retired
                .remove(handle)
                .map(|retired| retired.run)
                .ok_or_else(|| "it is no longer kept".to_owned())
        });
        if run.is_ok() {
            inner.fates.insert(handle.to_owned(), Fate::Busy);
        }
        drop(inner);
        drop(gone);
        run
    }

    /// Whether the finished subagent kept as `handle`, started by `keeper`, only looks; nothing when no such subagent is kept.
    pub(crate) fn looks(&self, keeper: &str, handle: &str) -> Option<bool> {
        self.lock()
            .retired
            .get(handle)
            .filter(|retired| retired.keeper == keeper)
            .map(|retired| retired.run.looks())
    }

    /// Stops the running subagent `id` and every running subagent it started, at any depth.
    ///
    /// Returns whether any subagent was stopped: `false` for an empty id, or one that names no running subagent.
    pub fn stop(&self, id: &str) -> bool {
        if id.is_empty() {
            return false;
        }
        let below = format!("{id}:");
        let inner = self.lock();
        let mut stopped = false;
        for entry in inner.entries.values() {
            if entry.id == id || entry.id.starts_with(&below) {
                entry.stop.send_replace(true);
                stopped = true;
            }
        }
        stopped
    }

    /// Whether a subagent with the id `id` runs now.
    pub fn running(&self, id: &str) -> bool {
        self.lock().entries.values().any(|entry| entry.id == id)
    }

    /// Enrols the subagent `notice` describes, announcing its start through `events`; it leaves the roster, and its end is announced, when the returned enrolment ends or is dropped.
    pub(crate) fn enrol(
        self: &Arc<Self>,
        notice: SubagentNotice,
        events: Arc<EventSink>,
    ) -> Enrolment {
        let (stop, stopped) = watch::channel(false);
        let key = {
            let mut inner = self.lock();
            let key = inner.next;
            inner.next += 1;
            inner.entries.insert(
                key,
                Entry {
                    id: notice.id.clone(),
                    stop,
                },
            );
            key
        };
        let notice = SubagentNotice {
            state: SubagentState::Running,
            ..notice
        };
        events(AgentEvent::Subagent(notice.clone()));
        Enrolment {
            roster: self.clone(),
            key,
            stopped,
            notice,
            events,
            ended: false,
        }
    }

    /// Takes the subagent enrolled under `key` off the roster.
    fn leave(&self, key: u64) {
        self.lock().entries.remove(&key);
    }

    /// The roster's contents, locked; a poisoned lock is taken over, since every change to it is whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// A subagent's place on the [`Roster`] while it runs; dropping it before [`Enrolment::end`] announces that the subagent was stopped.
pub(crate) struct Enrolment {
    /// The roster it is on.
    roster: Arc<Roster>,
    /// Its key there.
    key: u64,
    /// Turns `true` once the subagent is to stop.
    stopped: watch::Receiver<bool>,
    /// How it was announced.
    notice: SubagentNotice,
    /// Where its end is announced.
    events: Arc<EventSink>,
    /// Whether its end was announced already.
    ended: bool,
}

impl Enrolment {
    /// How the subagent was announced, with what it cost so far once that was set.
    pub(crate) fn notice(&self) -> SubagentNotice {
        self.notice.clone()
    }

    /// Where its announcements go.
    pub(crate) fn events(&self) -> Arc<EventSink> {
        self.events.clone()
    }

    /// Records that the subagent has cost `spent` so far, which its end is announced with.
    pub(crate) fn spent(&mut self, spent: Option<omc_wire::SubagentSpend>) {
        self.notice.spent = spent;
    }

    /// Waits until the subagent is to stop; never returns when that does not happen.
    pub(crate) async fn stopped(&mut self) {
        if self.stopped.wait_for(|stop| *stop).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// Takes the subagent off the roster, so that it can no longer be stopped, while what it changed is still being handed back; its end is announced later.
    pub(crate) fn leave(&mut self) {
        self.roster.leave(self.key);
    }

    /// Takes the subagent off the roster and announces that it ended in `state`.
    pub(crate) fn end(mut self, state: SubagentState) {
        self.announce(state);
    }

    /// Takes the subagent off the roster and announces its end in `state`, once.
    fn announce(&mut self, state: SubagentState) {
        if self.ended {
            return;
        }
        self.ended = true;
        self.roster.leave(self.key);
        (self.events)(AgentEvent::Subagent(SubagentNotice {
            state,
            ..self.notice.clone()
        }));
    }
}

impl Drop for Enrolment {
    /// Announces that the subagent was stopped when it was dropped before it ended, as when the turn it ran in, the subagent that started it or its background run was stopped.
    fn drop(&mut self) {
        self.announce(SubagentState::Stopped);
    }
}

#[cfg(test)]
/// Tests for the roster.
mod tests {
    use super::*;

    /// A notice of a running subagent with the id `id`.
    fn notice(id: &str) -> SubagentNotice {
        SubagentNotice {
            id: id.to_owned(),
            call: "call".to_owned(),
            agent: "task".to_owned(),
            label: "work".to_owned(),
            model: None,
            category: None,
            background: false,
            state: SubagentState::Running,
            spent: None,
        }
    }

    /// A sink that keeps every event it is given.
    fn sink() -> (Arc<EventSink>, Arc<Mutex<Vec<AgentEvent>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let kept = seen.clone();
        let sink: Arc<EventSink> = Arc::new(move |event| kept.lock().unwrap().push(event));
        (sink, seen)
    }

    /// Stopping an id stops it and those under it, not a sibling sharing its start.
    #[test]
    fn stopping_reaches_the_subagents_below_only() {
        let roster = Arc::new(Roster::default());
        let (events, _) = sink();
        let parent = roster.enrol(notice("c/2"), events.clone());
        let child = roster.enrol(notice("c/2:d"), events.clone());
        let sibling = roster.enrol(notice("c/22"), events);
        assert!(roster.stop("c/2"));
        assert!(*parent.stopped.borrow());
        assert!(*child.stopped.borrow());
        assert!(!*sibling.stopped.borrow());
    }

    /// An empty, unknown or ended id stops nothing.
    #[test]
    fn unknown_or_ended_ids_are_refused() {
        let roster = Arc::new(Roster::default());
        let (events, seen) = sink();
        let run = roster.enrol(notice("c"), events);
        assert!(!roster.stop(""));
        assert!(!roster.stop("x"));
        run.end(SubagentState::Finished);
        assert!(!roster.stop("c"));
        let states: Vec<SubagentState> = seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|event| match event {
                AgentEvent::Subagent(notice) => Some(notice.state),
                _ => None,
            })
            .collect();
        assert_eq!(states, [SubagentState::Running, SubagentState::Finished]);
    }

    /// A subagent dropped before it ended is announced as stopped.
    #[test]
    fn dropping_announces_a_stop() {
        let roster = Arc::new(Roster::default());
        let (events, seen) = sink();
        drop(roster.enrol(notice("c"), events));
        assert!(!roster.running("c"));
        assert!(matches!(
            seen.lock().unwrap().last(),
            Some(AgentEvent::Subagent(SubagentNotice {
                state: SubagentState::Stopped,
                ..
            }))
        ));
    }

    /// A handle remembered as not kept is refused with why, each fate in its own words.
    #[test]
    fn forgotten_handles_say_why() {
        let roster = Roster::default();
        for (fate, words) in [
            (Fate::Failed, "it failed"),
            (Fate::Stopped, "stopped"),
            (Fate::Isolated, "isolated"),
            (Fate::Structured, "structured output"),
            (Fate::Evicted, "evicted"),
            (Fate::Expired, "expired"),
            (Fate::WentWith("sub-1".to_owned()), "sub-1"),
        ] {
            roster.forget("sub-2", fate);
            let why = roster.recall("", "sub-2").err().unwrap();
            assert!(why.contains(words), "{why}");
        }
    }

    /// The subagent budget is off by default, takes a positive amount, and is turned off again by none or an amount that is not above zero.
    #[test]
    fn the_budget_is_off_until_set() {
        let roster = Roster::default();
        assert_eq!(roster.budget(), None);
        roster.set_budget(Some(2.5));
        assert_eq!(roster.budget(), Some(2.5));
        roster.set_budget(Some(0.0));
        assert_eq!(roster.budget(), None);
        roster.set_budget(Some(1.0));
        roster.set_budget(None);
        assert_eq!(roster.budget(), None);
    }
}
