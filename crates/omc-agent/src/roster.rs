//! The subagents of a conversation: the running ones, each under the id [`crate::Nesting`] gives it, so that the reader can stop one of them without stopping the turn, and the finished ones kept for a while, each under a handle, so that a follow-up reaches one with everything it learned.
//!
//! A subagent is announced with an [`AgentEvent::Subagent`] when it starts and again when it ends. Stopping one stops the subagents it started too, since their ids begin with its own.
//!
//! A finished subagent is kept with its conversation until a follow-up takes it, it grows older than the roster allows, or more are kept than the roster allows, when the one kept longest goes; the finished subagents it started itself go with it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::AgentEvent;
use crate::task::{EventSink, Run};

/// How many finished subagents a conversation keeps for follow-ups by default.
pub const KEPT_MOST: usize = 32;

/// How long a finished subagent is kept for follow-ups by default.
pub const KEPT_FOR: Duration = Duration::from_secs(30 * 60);

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
    /// Whether it runs in the background.
    pub background: bool,
    /// How it stands.
    pub state: SubagentState,
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
}

impl Inner {
    /// Takes out the finished subagent kept as `handle` and, at every depth, the finished subagents it started, and returns them all.
    fn discard(&mut self, handle: &str) -> Vec<Retired> {
        let mut gone = Vec::new();
        let mut handles = vec![handle.to_owned()];
        while let Some(handle) = handles.pop() {
            gone.extend(self.retired.remove(&handle));
            handles.extend(
                self.retired
                    .iter()
                    .filter(|(_, retired)| retired.keeper == handle)
                    .map(|(below, _)| below.clone()),
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
            gone.extend(self.discard(&handle));
        }
        while self.retired.len() > most {
            let oldest = self
                .retired
                .iter()
                .min_by_key(|(_, retired)| retired.since)
                .map(|(handle, _)| handle.clone());
            match oldest {
                Some(handle) => gone.extend(self.discard(&handle)),
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
}

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
        }
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

    /// Takes the finished subagent kept as `handle` off the roster, for a follow-up, when the subagent with the handle `keeper`, or the main agent when that is empty, started it and it is not kept too long; the finished subagents it started stay.
    pub(crate) fn recall(&self, keeper: &str, handle: &str) -> Option<Run> {
        let mut inner = self.lock();
        let gone = inner.prune(self.most, self.age);
        let own = inner
            .retired
            .get(handle)
            .is_some_and(|retired| retired.keeper == keeper);
        let run = own
            .then(|| inner.retired.remove(handle))
            .flatten()
            .map(|retired| retired.run);
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
            background: false,
            state: SubagentState::Running,
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
}
