//! The tool calls a turn has announced and not yet ended, so that a turn dropped while they run, as when it is cancelled or the subagent running it is stopped, still ends each of them for whoever shows them.

use std::sync::Mutex;

use crate::agent::AgentEvent;

/// The text a call is ended with when the turn running it was dropped first.
pub(crate) const STOPPED_TEXT: &str = "stopped";

/// Where the place of a call is kept: the ids of the calls it is nested under, outermost first, then its own.
type Place = Vec<String>;

/// Passes events on to a sink, keeping each call it was told started until it is told the call ended; when it is dropped, every call still open is ended there as failed with [`STOPPED_TEXT`], the innermost first.
///
/// A call is ended at most once: one ended already is no longer kept, and one announced twice is kept once.
pub(crate) struct OpenCalls<'a> {
    /// Where the events go.
    events: &'a (dyn Fn(AgentEvent) + Send + Sync),
    /// The calls started and not yet ended, in the order they started, each with the event that started it.
    open: Mutex<Vec<(Place, AgentEvent)>>,
}

impl<'a> OpenCalls<'a> {
    /// Keeps the calls of the events passed on to `events`.
    pub(crate) fn new(events: &'a (dyn Fn(AgentEvent) + Send + Sync)) -> Self {
        Self {
            events,
            open: Mutex::new(Vec::new()),
        }
    }

    /// Notes the call `event` starts or ends, when it does either, then passes it on.
    pub(crate) fn see(&self, event: AgentEvent) {
        if let Some((place, starts)) = place_of(&event) {
            let mut open = self.lock();
            if starts {
                if !open.iter().any(|(kept, _)| *kept == place) {
                    open.push((place, event.clone()));
                }
            } else {
                open.retain(|(kept, _)| *kept != place);
            }
        }
        (self.events)(event);
    }

    /// The calls still open, held; a poisoned lock is taken over, since every change to it is whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<(Place, AgentEvent)>> {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Drop for OpenCalls<'_> {
    /// Ends every call still open as stopped, the last started first.
    fn drop(&mut self) {
        let open = std::mem::take(&mut *self.lock());
        for (_, started) in open.into_iter().rev() {
            if let Some(ended) = stopped(started) {
                (self.events)(ended);
            }
        }
    }
}

/// Where the call `event` starts or ends is, and whether it starts there; nothing for an event that neither starts nor ends a call.
fn place_of(event: &AgentEvent) -> Option<(Place, bool)> {
    match event {
        AgentEvent::ToolStart { id, .. } => Some((vec![id.clone()], true)),
        AgentEvent::ToolDone { id, .. } => Some((vec![id.clone()], false)),
        AgentEvent::Nested { parent, event, .. } => {
            let (mut place, starts) = place_of(event)?;
            place.insert(0, parent.clone());
            Some((place, starts))
        }
        _ => None,
    }
}

/// The event ending, as failed with [`STOPPED_TEXT`], the call that `started` started, filed where it was; nothing when `started` starts no call.
fn stopped(started: AgentEvent) -> Option<AgentEvent> {
    match started {
        AgentEvent::ToolStart { id, name, .. } => Some(AgentEvent::ToolDone {
            id,
            name,
            text: STOPPED_TEXT.to_owned(),
            is_error: true,
            changes: Vec::new(),
        }),
        AgentEvent::Nested {
            parent,
            agent,
            event,
        } => Some(AgentEvent::Nested {
            parent,
            agent,
            event: Box::new(stopped(*event)?),
        }),
        _ => None,
    }
}

#[cfg(test)]
/// Tests for keeping the open calls.
mod tests {
    use super::*;

    /// A call `id` of `read` starting.
    fn start(id: &str) -> AgentEvent {
        AgentEvent::ToolStart {
            id: id.to_owned(),
            name: "read".to_owned(),
            summary: String::new(),
        }
    }

    /// A call `id` of `read` ending well.
    fn done(id: &str) -> AgentEvent {
        AgentEvent::ToolDone {
            id: id.to_owned(),
            name: "read".to_owned(),
            text: "ok".to_owned(),
            is_error: false,
            changes: Vec::new(),
        }
    }

    /// `event` filed under the call `parent` of `script`.
    fn under(parent: &str, event: AgentEvent) -> AgentEvent {
        AgentEvent::Nested {
            parent: parent.to_owned(),
            agent: "script".to_owned(),
            event: Box::new(event),
        }
    }

    /// The events `see` passes on, then those dropping the keeper adds.
    fn seen_through(events: Vec<AgentEvent>) -> Vec<AgentEvent> {
        let log = Mutex::new(Vec::new());
        let sink = |event: AgentEvent| log.lock().unwrap().push(event);
        {
            let open = OpenCalls::new(&sink);
            for event in events {
                open.see(event);
            }
        }
        log.into_inner().unwrap()
    }

    /// Calls that ended are not ended again, and those still open are ended as stopped, nested ones under their parent and before it.
    #[test]
    fn open_calls_end_as_stopped_once() {
        let log = seen_through(vec![
            start("a"),
            start("b"),
            done("a"),
            under("b", start("1")),
            under("b", start("2")),
            under("b", done("2")),
            start("b"),
        ]);
        let added = &log[7..];
        assert_eq!(added.len(), 2);
        assert!(matches!(
            &added[0],
            AgentEvent::Nested { parent, event, .. }
                if parent == "b" && matches!(event.as_ref(), AgentEvent::ToolDone { id, is_error: true, text, .. } if id == "1" && text == STOPPED_TEXT)
        ));
        assert!(matches!(
            &added[1],
            AgentEvent::ToolDone { id, is_error: true, text, .. } if id == "b" && text == STOPPED_TEXT
        ));
    }

    /// A keeper whose calls all ended adds nothing when dropped.
    #[test]
    fn ended_calls_add_nothing() {
        let log = seen_through(vec![start("a"), done("a")]);
        assert_eq!(log.len(), 2);
    }
}
