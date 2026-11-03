//! What the agent loop reports, as the events the client reads.

use omc_agent::AgentEvent;
use omc_wire::{Diff, Event, Kind, Tool, ToolState, Usage};

/// What kind of tool the tool called `name` is.
fn kind_of(name: &str) -> Kind {
    match name {
        "read" | "skill" => Kind::Read,
        "edit" | "write" => Kind::Edit,
        "grep" | "glob" => Kind::Search,
        "bash" => Kind::Execute,
        _ => Kind::Other,
    }
}

/// A tool event with only its `id` set.
fn bare(id: String) -> Tool {
    Tool {
        id,
        title: None,
        kind: None,
        state: None,
        target: None,
        diff: None,
        output: None,
        parent: None,
        agent: None,
    }
}

/// The wire form of a file change.
fn diff_of(change: omc_tools::FileChange) -> Diff {
    Diff {
        path: change.path,
        old: Some(change.old),
        new: change.new,
    }
}

/// The events that say what `event` says: what [`translate`] gives, and one more tool event for each file after the first that a finished call changed.
pub(crate) fn translate_all(event: AgentEvent, window: u64) -> Vec<Event> {
    let extra: Vec<Event> = match &event {
        AgentEvent::ToolDone { id, changes, .. } => changes
            .iter()
            .enumerate()
            .skip(1)
            .map(|(index, change)| {
                Event::Tool(Tool {
                    title: Some("edit".into()),
                    kind: Some(Kind::Edit),
                    state: Some(ToolState::Done),
                    target: Some(change.path.clone()),
                    diff: Some(diff_of(change.clone())),
                    ..bare(format!("{id}:{index}"))
                })
            })
            .collect(),
        _ => Vec::new(),
    };
    translate(event, window).into_iter().chain(extra).collect()
}

/// The event that says what `event` says, given a context window of `window` tokens; nothing when it says nothing the client shows.
pub(crate) fn translate(event: AgentEvent, window: u64) -> Option<Event> {
    Some(match event {
        AgentEvent::Delta(text) => Event::Said(text),
        AgentEvent::Thought(text) => Event::Thought(text),
        AgentEvent::Used(tokens) => Event::Used(Usage {
            used: tokens.context(),
            size: window,
        }),
        AgentEvent::ToolStart { id, name, summary } => Event::Tool(Tool {
            title: Some(name.clone()),
            kind: Some(kind_of(&name)),
            state: Some(ToolState::Running),
            target: Some(summary),
            ..bare(id)
        }),
        AgentEvent::ToolDone {
            id,
            text,
            is_error,
            changes,
            ..
        } => Event::Tool(Tool {
            diff: changes.into_iter().next().map(diff_of),
            state: Some(if is_error {
                ToolState::Failed
            } else {
                ToolState::Done
            }),
            output: Some(text),
            ..bare(id)
        }),
        AgentEvent::Spawned { id, agent } => Event::Tool(Tool {
            agent: Some(agent),
            ..bare(id)
        }),
        AgentEvent::Nested {
            parent,
            agent,
            event,
        } => match translate(*event, window)? {
            Event::Tool(tool) => Event::Tool(Tool {
                id: format!("{parent}:{}", tool.id),
                parent: Some(parent),
                agent: Some(agent),
                ..tool
            }),
            _ => return None,
        },
    })
}

#[cfg(test)]
/// Tests for the event translation.
mod tests {
    use super::*;

    /// A started tool carries its kind and target, and a failed one its state.
    #[test]
    fn tools() {
        let Some(Event::Tool(started)) = translate(
            AgentEvent::ToolStart {
                id: "1".into(),
                name: "edit".into(),
                summary: "a.rs".into(),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(started.kind, Some(Kind::Edit));
        assert_eq!(started.target.as_deref(), Some("a.rs"));
        let Some(Event::Tool(done)) = translate(
            AgentEvent::ToolDone {
                id: "1".into(),
                name: "edit".into(),
                text: "no".into(),
                is_error: true,
                changes: Vec::new(),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(done.state, Some(ToolState::Failed));
    }

    /// The context size follows the window it is given.
    #[test]
    fn usage_uses_the_window() {
        let tokens = omc_providers::Tokens {
            input: 10,
            output: 5,
            cache_read: 5,
            cache_write: 0,
        };
        assert_eq!(
            translate(AgentEvent::Used(tokens), 1_000_000),
            Some(Event::Used(Usage {
                used: 15,
                size: 1_000_000
            }))
        );
    }

    /// A subagent's tool activity is filed under the call that started it, and the call carries the kind.
    #[test]
    fn subagent_activity_is_nested() {
        let Some(Event::Tool(spawned)) = translate(
            AgentEvent::Spawned {
                id: "c1".into(),
                agent: "scout".into(),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(
            (spawned.id.as_str(), spawned.agent.as_deref()),
            ("c1", Some("scout"))
        );
        assert!(spawned.parent.is_none() && spawned.state.is_none());
        let Some(Event::Tool(inner)) = translate(
            AgentEvent::Nested {
                parent: "c1".into(),
                agent: "scout".into(),
                event: Box::new(AgentEvent::ToolStart {
                    id: "t".into(),
                    name: "grep".into(),
                    summary: "x".into(),
                }),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(inner.parent.as_deref(), Some("c1"));
        assert_eq!(inner.agent.as_deref(), Some("scout"));
        assert_eq!(inner.id, "c1:t");
        assert_eq!(inner.kind, Some(Kind::Search));
        assert!(
            translate(
                AgentEvent::Nested {
                    parent: "c1".into(),
                    agent: "scout".into(),
                    event: Box::new(AgentEvent::Delta("x".into())),
                },
                1
            )
            .is_none()
        );
    }

    /// A finished edit carries its diff, and each further file gets an event of its own.
    #[test]
    fn changed_files_become_diffs() {
        let change = |path: &str| omc_tools::FileChange {
            path: path.into(),
            old: "a\n".into(),
            new: "b\n".into(),
        };
        let events = translate_all(
            AgentEvent::ToolDone {
                id: "7".into(),
                name: "edit".into(),
                text: "ok".into(),
                is_error: false,
                changes: vec![change("a.rs"), change("b.rs")],
            },
            1,
        );
        assert_eq!(events.len(), 2);
        let Event::Tool(first) = &events[0] else {
            panic!()
        };
        assert_eq!(
            first.diff,
            Some(Diff {
                path: "a.rs".into(),
                old: Some("a\n".into()),
                new: "b\n".into()
            })
        );
        let Event::Tool(second) = &events[1] else {
            panic!()
        };
        assert_eq!(second.diff.as_ref().map(|d| d.path.as_str()), Some("b.rs"));
    }
}
