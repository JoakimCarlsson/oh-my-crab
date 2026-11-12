//! The subagents running in a conversation, each under the id [`crate::Nesting`] gives it, so that the reader can stop one of them without stopping the turn.
//!
//! A subagent is announced with an [`AgentEvent::Subagent`] when it starts and again when it ends. Stopping one stops the subagents it started too, since their ids begin with its own.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::watch;

use crate::AgentEvent;
use crate::task::EventSink;

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

/// What the roster holds.
#[derive(Default)]
struct Inner {
    /// The key the next subagent enrolled gets.
    next: u64,
    /// The running subagents, by key.
    entries: HashMap<u64, Entry>,
}

/// The subagents running in a conversation, at every level, shared by every `task` tool of it.
#[derive(Default)]
pub struct Roster {
    /// The running subagents.
    inner: Mutex<Inner>,
}

impl Roster {
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
