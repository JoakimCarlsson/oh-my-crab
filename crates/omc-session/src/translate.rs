//! What the agent loop reports, as the events the client reads.

use omc_agent::AgentEvent;
use omc_wire::{Event, Kind, Tool, ToolState, Usage};

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
            id, text, is_error, ..
        } => Event::Tool(Tool {
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
}
