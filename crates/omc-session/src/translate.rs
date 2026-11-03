//! What the agent loop reports, as the events the client reads.

use omc_agent::AgentEvent;
use omc_wire::{Diff, Event, Kind, PastTurn, Tool, ToolState, Usage};

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

/// The turn `lost` as a client shows it: what the reader prompted, and the end it came to.
fn lost_turn(lost: &omc_agent::Lost) -> PastTurn {
    let events = if lost.reason == omc_agent::STOPPED || lost.reason == omc_agent::INTERRUPTED {
        vec![Event::Halted, Event::Finished(None)]
    } else {
        vec![Event::Finished(Some(lost.reason.clone()))]
    };
    PastTurn {
        id: Some(lost.mark.id.clone()),
        prompt: lost.mark.text.clone(),
        files: lost.mark.files.clone(),
        events,
    }
}

/// The turns a saved `history` tells, as the events a live conversation would have sent for them, with the context size measured against `window`.
///
/// A turn starts at each message the reader wrote. The target of a call is taken from its tool in `registry` when it is still there. The files a call changed come back as its diff when they were saved with it.
pub(crate) fn replay(
    history: &[omc_providers::Message],
    marks: &[omc_agent::Marked],
    lost: &[omc_agent::Lost],
    changes: &std::collections::HashMap<String, Vec<omc_tools::FileChange>>,
    registry: &omc_tools::Registry,
    window: Option<u64>,
) -> Vec<PastTurn> {
    use omc_providers::Message;
    let mut turns: Vec<PastTurn> = Vec::new();
    let mut ended = 0;
    for (at, message) in history.iter().enumerate() {
        if matches!(message, Message::User(_)) {
            while ended < lost.len() && lost[ended].at <= at {
                turns.push(lost_turn(&lost[ended]));
                ended += 1;
            }
        }
        match message {
            Message::User(text) => {
                let mark = marks
                    .iter()
                    .find(|(position, _)| *position == at)
                    .map(|(_, mark)| mark);
                turns.push(PastTurn {
                    id: mark.map(|mark| mark.id.clone()),
                    prompt: mark.map_or_else(|| text.clone(), |mark| mark.text.clone()),
                    files: mark.map(|mark| mark.files.clone()).unwrap_or_default(),
                    events: Vec::new(),
                });
            }
            Message::Assistant(reply) => {
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                if !reply.thinking.is_empty() {
                    turn.events.push(Event::Thought(reply.thinking.clone()));
                }
                if !reply.text.is_empty() {
                    turn.events.push(Event::Said(reply.text.clone()));
                }
                for call in &reply.tool_calls {
                    let target = registry.get(&call.name).map_or_else(
                        || call.arguments.to_string(),
                        |tool| tool.describe(&call.arguments),
                    );
                    if call.name == "todo"
                        && let Some(plan) = plan_of(&call.arguments)
                    {
                        turn.events.push(Event::Plan(plan));
                    }
                    turn.events.push(Event::Tool(Tool {
                        title: Some(call.name.clone()),
                        kind: Some(kind_of(&call.name)),
                        state: Some(ToolState::Running),
                        target: Some(target),
                        ..bare(call.id.clone())
                    }));
                }
                if let (Some(tokens), Some(window)) = (reply.tokens, window) {
                    turn.events.push(Event::Used(Usage {
                        used: tokens.context(),
                        size: window,
                    }));
                }
            }
            Message::ToolResults(results) => {
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                for result in results {
                    turn.events.extend(translate_all(
                        AgentEvent::ToolDone {
                            id: result.call_id.clone(),
                            name: result.name.clone(),
                            text: result.text.clone(),
                            is_error: result.is_error,
                            changes: changes.get(&result.call_id).cloned().unwrap_or_default(),
                        },
                        window.unwrap_or(0),
                    ));
                }
            }
        }
    }
    turns.extend(lost[ended..].iter().map(lost_turn));
    turns
}

/// The plan a `todo` call's `arguments` lay out, when they lay one out.
fn plan_of(arguments: &serde_json::Value) -> Option<Vec<omc_wire::PlanStep>> {
    let todos = arguments.get("todos")?.as_array()?;
    Some(
        todos
            .iter()
            .filter_map(|item| {
                let text = item.get("content")?.as_str()?.trim();
                (!text.is_empty()).then(|| omc_wire::PlanStep {
                    text: text.to_owned(),
                    progress: match item.get("status").and_then(|status| status.as_str()) {
                        Some("in_progress" | "doing" | "active" | "started") => {
                            omc_wire::Progress::Doing
                        }
                        Some("completed" | "done" | "complete") => omc_wire::Progress::Done,
                        _ => omc_wire::Progress::Pending,
                    },
                })
            })
            .collect(),
    )
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

    /// A saved history of two turns, the first with a tool call.
    fn saved() -> Vec<omc_providers::Message> {
        use omc_providers::{Message, Reply, ToolCall, ToolResult};
        vec![
            Message::User("read a".into()),
            Message::Assistant(Reply {
                thinking: "hm".into(),
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: "alpha".into(),
                is_error: false,
            }]),
            Message::Assistant(Reply {
                text: "it says alpha".into(),
                ..Reply::default()
            }),
            Message::User("thanks".into()),
            Message::Assistant(Reply {
                text: "welcome".into(),
                ..Reply::default()
            }),
        ]
    }

    /// Each reader message starts a turn, and a call and its result become the tool events a live turn sends.
    #[test]
    fn replay_tells_turns_as_live_events() {
        let registry = omc_tools::Registry::new();
        let turns = replay(
            &saved(),
            &[],
            &[],
            &Default::default(),
            &registry,
            Some(1000),
        );
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].prompt, "read a");
        assert_eq!(turns[1].prompt, "thanks");
        let events = &turns[0].events;
        assert_eq!(events[0], Event::Thought("hm".into()));
        let Event::Tool(started) = &events[1] else {
            panic!("no tool start");
        };
        assert_eq!(
            (started.id.as_str(), started.state),
            ("c1", Some(ToolState::Running))
        );
        assert_eq!(started.kind, Some(Kind::Read));
        let Event::Tool(done) = &events[2] else {
            panic!("no tool result");
        };
        assert_eq!(
            (done.id.as_str(), done.state),
            ("c1", Some(ToolState::Done))
        );
        assert_eq!(done.output.as_deref(), Some("alpha"));
        assert_eq!(events[3], Event::Said("it says alpha".into()));
        assert_eq!(turns[1].events, vec![Event::Said("welcome".into())]);
    }

    /// A turn the client marked comes back with its id, what the reader wrote and the files, instead of the composed text.
    #[test]
    fn replay_uses_the_marks() {
        let registry = omc_tools::Registry::new();
        let marks = vec![(
            0,
            omc_agent::Mark {
                id: "7".into(),
                text: "read a".into(),
                files: vec!["a.txt".into()],
            },
        )];
        let turns = replay(&saved(), &marks, &[], &Default::default(), &registry, None);
        assert_eq!(turns[0].id.as_deref(), Some("7"));
        assert_eq!(turns[0].files, vec!["a.txt".to_owned()]);
        assert_eq!(turns[1].id, None);
        assert!(
            turns[0]
                .events
                .iter()
                .all(|event| !matches!(event, Event::Used(_)))
        );
    }

    /// A stopped turn and a failed one come back between the turns they were lost between, as a stopped and a failed turn.
    #[test]
    fn replay_tells_lost_turns_in_order() {
        let registry = omc_tools::Registry::new();
        let lost = |id: &str, at: usize, reason: &str| omc_agent::Lost {
            at,
            mark: omc_agent::Mark {
                id: id.into(),
                text: format!("lost {id}"),
                files: Vec::new(),
            },
            reason: reason.into(),
        };
        let turns = replay(
            &saved(),
            &[],
            &[
                lost("a", 4, omc_agent::STOPPED),
                lost("b", 4, "boom"),
                lost("c", 6, "late"),
            ],
            &Default::default(),
            &registry,
            None,
        );
        let prompts: Vec<&str> = turns.iter().map(|turn| turn.prompt.as_str()).collect();
        assert_eq!(
            prompts,
            vec!["read a", "lost a", "lost b", "thanks", "lost c"]
        );
        assert_eq!(turns[1].events, vec![Event::Halted, Event::Finished(None)]);
        assert_eq!(turns[2].events, vec![Event::Finished(Some("boom".into()))]);
        assert_eq!(turns[2].id.as_deref(), Some("b"));
    }

    /// A call whose changes were saved comes back with its diff, and one with several files gets an event for each.
    #[test]
    fn replay_restores_saved_diffs() {
        let registry = omc_tools::Registry::new();
        let change = |path: &str| omc_tools::FileChange {
            path: path.into(),
            old: "a\n".into(),
            new: "b\n".into(),
        };
        let mut changes = std::collections::HashMap::new();
        changes.insert("c1".to_owned(), vec![change("a.rs"), change("b.rs")]);
        let turns = replay(&saved(), &[], &[], &changes, &registry, Some(1000));
        let diffs: Vec<Option<&str>> = turns[0]
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Tool(tool) if tool.state == Some(ToolState::Done) => {
                    Some(tool.diff.as_ref().map(|diff| diff.path.as_str()))
                }
                _ => None,
            })
            .collect();
        assert_eq!(diffs, vec![Some("a.rs"), Some("b.rs")]);
    }
}
