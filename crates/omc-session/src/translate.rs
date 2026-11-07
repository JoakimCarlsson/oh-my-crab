//! What the agent loop reports, as the events the client reads.

use omc_agent::AgentEvent;
use omc_providers::Part;
use omc_wire::{Diff, Event, Kind, PastTurn, Tool, ToolState, Usage};

/// What kind of tool use a call of the tool called `name` is, its target
/// shown as `target`: a `read` whose target is a web address, as a URL or a
/// GitHub path shows, fetches rather than reads, and a `debug` call runs a
/// command when it starts or runs the program, and is something else when it
/// only looks at it or sets breakpoints. An `lsp` call edits when it renames
/// and is not a dry run, and searches otherwise.
fn kind_of(name: &str, target: &str) -> Kind {
    match name {
        "read" if is_web(target) => Kind::Fetch,
        "debug" if runs_program(target) => Kind::Execute,
        "lsp" if target.starts_with("rename") && !target.ends_with("[dry run]") => Kind::Edit,
        "lsp" => Kind::Search,
        "read" | "skill" => Kind::Read,
        "edit" | "write" | "ast_edit" => Kind::Edit,
        "grep" | "glob" | "ast_grep" | "web_search" => Kind::Search,
        "bash" => Kind::Execute,
        _ => Kind::Other,
    }
}

/// Whether the `debug` call shown as `target` starts or runs the program: a launch, an attach, a continue or a step.
fn runs_program(target: &str) -> bool {
    let action = target.split_whitespace().next().unwrap_or_default();
    matches!(
        action,
        "launch" | "attach" | "continue" | "step_over" | "step_in" | "step_out"
    )
}

/// Whether `target` is an `http://` or `https://` address.
fn is_web(target: &str) -> bool {
    let start = target.get(..8).unwrap_or(target).to_ascii_lowercase();
    start.starts_with("http://") || start.starts_with("https://")
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
        worktree: None,
    }
}

/// The wire form of what became of an isolated subagent's changes.
fn merge_of(merge: omc_agent::Merge) -> omc_wire::Merge {
    use omc_agent::Merge as Agent;
    let shown = |path: std::path::PathBuf| path.display().to_string();
    match merge {
        Agent::Applied(files) => omc_wire::Merge::Applied { files },
        Agent::Unchanged => omc_wire::Merge::Unchanged,
        Agent::Conflicted { patch, files } => omc_wire::Merge::Conflicted {
            patch: shown(patch),
            files,
        },
        Agent::Held { reason, patch } => omc_wire::Merge::Held {
            reason,
            patch: patch.map(shown),
        },
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

/// The branches of a saved conversation as the client is told them: each with the turns it holds, in the order they happened, a lost turn before the turn that started where it would have.
pub(crate) fn branches(saved: &[omc_agent::Branch]) -> Vec<omc_wire::Branch> {
    saved
        .iter()
        .map(|branch| {
            let mut turns = Vec::with_capacity(branch.marks.len() + branch.lost.len());
            let mut ended = 0;
            for (at, mark) in &branch.marks {
                while ended < branch.lost.len() && branch.lost[ended].at <= *at {
                    turns.push(branch_turn(
                        &branch.lost[ended].mark,
                        Some(&branch.lost[ended].reason),
                    ));
                    ended += 1;
                }
                turns.push(branch_turn(mark, None));
            }
            for lost in &branch.lost[ended..] {
                turns.push(branch_turn(&lost.mark, Some(&lost.reason)));
            }
            omc_wire::Branch {
                id: branch.id.clone(),
                parent: branch.parent.clone(),
                before: branch.before.clone(),
                turns,
            }
        })
        .collect()
}

/// The turn `mark` describes, as a branch lists it, which ended for `reason` when it has no answer.
fn branch_turn(mark: &omc_agent::Mark, reason: Option<&str>) -> omc_wire::BranchTurn {
    omc_wire::BranchTurn {
        id: mark.id.clone(),
        prompt: mark.text.clone(),
        ended: reason.map(str::to_owned),
    }
}

/// The turns a saved `history` tells, as the events a live conversation would have sent for them, with the context size measured against `window`.
///
/// A turn starts at each message the reader wrote. The target of a call is taken from its tool in `registry` when it is still there. The files a call changed come back as its diff when they were saved with it. A compaction shows as a finished `compact` call whose output is its summary, followed by how full the context was after it, as it did live: in a turn of its own when the reader asked for it, which its mark says, and otherwise in the turn it happened in. A pruning shows only as how full the context was after it.
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
        let mark = marks
            .iter()
            .find(|(position, _)| *position == at)
            .map(|(_, mark)| mark);
        let asked_for = matches!(message, Message::Compaction(_)) && mark.is_some();
        if matches!(message, Message::User(_)) || asked_for {
            while ended < lost.len() && lost[ended].at <= at {
                turns.push(lost_turn(&lost[ended]));
                ended += 1;
            }
        }
        if asked_for && let Some(mark) = mark {
            turns.push(PastTurn {
                id: Some(mark.id.clone()),
                prompt: mark.text.clone(),
                files: mark.files.clone(),
                events: Vec::new(),
            });
        }
        match message {
            Message::User(text) => {
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
                    turn.events.push(Event::Started(omc_wire::Part::Thinking));
                    turn.events.push(Event::Thought(reply.thinking.clone()));
                }
                if !reply.text.is_empty() {
                    turn.events.push(Event::Started(omc_wire::Part::Text));
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
                        kind: Some(kind_of(&call.name, &target)),
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
            Message::Compaction(compaction) => {
                let Some(turn) = turns.last_mut() else {
                    continue;
                };
                for event in omc_agent::replayed(at, compaction) {
                    if let (AgentEvent::Used(_), None) = (&event, window) {
                        continue;
                    }
                    turn.events
                        .extend(translate_all(event, window.unwrap_or(0)));
                }
            }
            Message::Pruning(pruning) => {
                if let (Some(turn), Some(window)) = (turns.last_mut(), window)
                    && pruning.tokens_after > 0
                {
                    turn.events.push(Event::Used(Usage {
                        used: pruning.tokens_after,
                        size: window,
                    }));
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

/// The event that says what `event` says, given a context window of `window` tokens; nothing when it says nothing the client shows. Work nested under a subagent keeps the kind of the innermost subagent it names: the one that made the call, or the one a nested `task` call starts.
pub(crate) fn translate(event: AgentEvent, window: u64) -> Option<Event> {
    Some(match event {
        AgentEvent::Started(Part::Thinking) => Event::Started(omc_wire::Part::Thinking),
        AgentEvent::Started(Part::Text) => Event::Started(omc_wire::Part::Text),
        AgentEvent::Delta(text) => Event::Said(text),
        AgentEvent::Thought(text) => Event::Thought(text),
        AgentEvent::Used(tokens) => Event::Used(Usage {
            used: tokens.context(),
            size: window,
        }),
        AgentEvent::ToolStart { id, name, summary } => Event::Tool(Tool {
            title: Some(name.clone()),
            kind: Some(kind_of(&name, &summary)),
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
        AgentEvent::Spawned { id, agent, .. } => Event::Tool(Tool {
            agent: Some(agent),
            ..bare(id)
        }),
        AgentEvent::Worktree { id, path, merge } => Event::Tool(Tool {
            worktree: Some(Box::new(omc_wire::Worktree {
                path: path.display().to_string(),
                merge: merge.map(merge_of),
            })),
            ..bare(id)
        }),
        AgentEvent::Nested {
            parent,
            agent,
            event,
        } => match translate(*event, window)? {
            Event::Tool(tool) => Event::Tool(Tool {
                id: format!("{parent}:{}", tool.id),
                parent: Some(match tool.parent {
                    Some(inner) => format!("{parent}:{inner}"),
                    None => parent,
                }),
                agent: tool.agent.or(Some(agent)),
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

    /// A read of a web address or GitHub path fetches, a web search searches,
    /// a read of a file reads, and a debug call executes only when it runs the
    /// program.
    #[test]
    fn kinds_follow_the_target() {
        assert_eq!(
            kind_of("read", "https://github.com/o/r/pull/1"),
            Kind::Fetch
        );
        assert_eq!(kind_of("read", "HTTP://example.com"), Kind::Fetch);
        assert_eq!(kind_of("read", "src/http.rs"), Kind::Read);
        assert_eq!(kind_of("web_search", "rust async"), Kind::Search);
        assert_eq!(kind_of("bash", "https://x"), Kind::Execute);
        assert_eq!(kind_of("debug", "launch build/app -v"), Kind::Execute);
        assert_eq!(kind_of("debug", "continue [dbg-1]"), Kind::Execute);
        assert_eq!(
            kind_of("lsp", "rename `a` in src/a.rs:3 to `b`"),
            Kind::Edit
        );
        assert_eq!(
            kind_of("lsp", "rename `a` in src/a.rs:3 to `b` [dry run]"),
            Kind::Search
        );
        assert_eq!(kind_of("lsp", "references `a` in src/a.rs:3"), Kind::Search);
        assert_eq!(kind_of("debug", "variables frame 0"), Kind::Other);
        assert_eq!(kind_of("debug", "break a.c:3"), Kind::Other);
    }

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
                model: Some("claude-haiku-4-5".into()),
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

    /// A subagent starting one of its own reports the call under its own, with the kind of the one it starts; the work of that one is filed under both calls, as its own.
    #[test]
    fn nested_subagents_chain_their_calls() {
        let Some(Event::Tool(started)) = translate(
            AgentEvent::Nested {
                parent: "c1".into(),
                agent: "task".into(),
                event: Box::new(AgentEvent::Spawned {
                    id: "s1".into(),
                    agent: "scout".into(),
                    model: Some("claude-haiku-4-5".into()),
                }),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(started.id, "c1:s1");
        assert_eq!(started.parent.as_deref(), Some("c1"));
        assert_eq!(started.agent.as_deref(), Some("scout"));
        let Some(Event::Tool(deep)) = translate(
            AgentEvent::Nested {
                parent: "c1".into(),
                agent: "task".into(),
                event: Box::new(AgentEvent::Nested {
                    parent: "s1".into(),
                    agent: "scout".into(),
                    event: Box::new(AgentEvent::ToolStart {
                        id: "g".into(),
                        name: "grep".into(),
                        summary: "x".into(),
                    }),
                }),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(deep.id, "c1:s1:g");
        assert_eq!(deep.parent.as_deref(), Some("c1:s1"));
        assert_eq!(deep.agent.as_deref(), Some("scout"));
    }

    /// The worktree of an isolated subagent is reported on its call, or on its step of a call, with what became of its changes once it ended.
    #[test]
    fn worktrees_are_reported_on_the_call_or_its_step() {
        let Some(Event::Tool(alone)) = translate(
            AgentEvent::Worktree {
                id: "c1".into(),
                path: "/cache/wt/repo".into(),
                merge: None,
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(alone.id, "c1");
        assert_eq!(
            alone.worktree,
            Some(Box::new(omc_wire::Worktree {
                path: "/cache/wt/repo".into(),
                merge: None
            }))
        );
        let Some(Event::Tool(step)) = translate(
            AgentEvent::Nested {
                parent: "c1".into(),
                agent: "task".into(),
                event: Box::new(AgentEvent::Worktree {
                    id: "2".into(),
                    path: "/cache/wt/repo".into(),
                    merge: Some(omc_agent::Merge::Conflicted {
                        patch: "/cache/out/p.log".into(),
                        files: vec!["a.txt".into()],
                    }),
                }),
            },
            1,
        ) else {
            panic!()
        };
        assert_eq!(
            (step.id.as_str(), step.parent.as_deref()),
            ("c1:2", Some("c1"))
        );
        assert_eq!(
            step.worktree.and_then(|worktree| worktree.merge),
            Some(omc_wire::Merge::Conflicted {
                patch: "/cache/out/p.log".into(),
                files: vec!["a.txt".into()],
            })
        );
    }

    /// A subagent that is one of several is a step of the call, and its own work is filed under that step.
    #[test]
    fn subagents_of_a_batch_nest_under_their_step() {
        let nested = |event: AgentEvent| AgentEvent::Nested {
            parent: "c1".into(),
            agent: "scout".into(),
            event: Box::new(event),
        };
        let Some(Event::Tool(step)) = translate(
            nested(AgentEvent::ToolStart {
                id: "2".into(),
                name: "task".into(),
                summary: "look".into(),
            }),
            1,
        ) else {
            panic!()
        };
        assert_eq!(
            (
                step.id.as_str(),
                step.parent.as_deref(),
                step.agent.as_deref()
            ),
            ("c1:2", Some("c1"), Some("scout"))
        );
        let Some(Event::Tool(work)) = translate(
            nested(AgentEvent::Nested {
                parent: "2".into(),
                agent: "scout".into(),
                event: Box::new(AgentEvent::ToolStart {
                    id: "t".into(),
                    name: "grep".into(),
                    summary: "x".into(),
                }),
            }),
            1,
        ) else {
            panic!()
        };
        assert_eq!(
            (work.id.as_str(), work.parent.as_deref()),
            ("c1:2:t", Some("c1:2"))
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
                images: Vec::new(),
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
        assert_eq!(events[0], Event::Started(omc_wire::Part::Thinking));
        assert_eq!(events[1], Event::Thought("hm".into()));
        let Event::Tool(started) = &events[2] else {
            panic!("no tool start");
        };
        assert_eq!(
            (started.id.as_str(), started.state),
            ("c1", Some(ToolState::Running))
        );
        assert_eq!(started.kind, Some(Kind::Read));
        let Event::Tool(done) = &events[3] else {
            panic!("no tool result");
        };
        assert_eq!(
            (done.id.as_str(), done.state),
            ("c1", Some(ToolState::Done))
        );
        assert_eq!(done.output.as_deref(), Some("alpha"));
        assert_eq!(events[4], Event::Started(omc_wire::Part::Text));
        assert_eq!(events[5], Event::Said("it says alpha".into()));
        assert_eq!(
            turns[1].events,
            vec![
                Event::Started(omc_wire::Part::Text),
                Event::Said("welcome".into())
            ]
        );
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

    /// Every turn from before a compaction still comes back, and the compaction shows in its turn as a finished `compact` call with the summary.
    #[test]
    fn replay_keeps_turns_before_a_compaction() {
        let registry = omc_tools::Registry::new();
        let mut history = saved();
        history.insert(
            4,
            omc_providers::Message::Compaction(omc_providers::Compaction {
                summary: "read a, said alpha".into(),
                first_kept: 3,
                tokens_before: 900,
                ..Default::default()
            }),
        );
        let turns = replay(&history, &[], &[], &Default::default(), &registry, None);
        let prompts: Vec<&str> = turns.iter().map(|turn| turn.prompt.as_str()).collect();
        assert_eq!(prompts, vec!["read a", "thanks"]);
        let tools: Vec<&Tool> = turns[0]
            .events
            .iter()
            .filter_map(|event| match event {
                Event::Tool(tool) if tool.id == "compact-4" => Some(tool),
                _ => None,
            })
            .collect();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].title.as_deref(), Some(omc_agent::COMPACT));
        assert_eq!(tools[1].state, Some(ToolState::Done));
        assert_eq!(tools[1].output.as_deref(), Some("read a, said alpha"));
    }

    /// A compaction the reader asked for comes back as a turn of its own under its mark, ending with how full the context was after it.
    #[test]
    fn replay_gives_an_asked_for_compaction_its_own_turn() {
        let registry = omc_tools::Registry::new();
        let mut history = saved();
        history.push(omc_providers::Message::Compaction(
            omc_providers::Compaction {
                summary: "all said".into(),
                first_kept: 4,
                tokens_before: 900,
                tokens_after: 120,
                ..Default::default()
            },
        ));
        let marks = vec![(
            6,
            omc_agent::Mark {
                id: "c".into(),
                text: "/compact".into(),
                files: Vec::new(),
            },
        )];
        let turns = replay(
            &history,
            &marks,
            &[],
            &Default::default(),
            &registry,
            Some(1_000),
        );
        let prompts: Vec<&str> = turns.iter().map(|turn| turn.prompt.as_str()).collect();
        assert_eq!(prompts, vec!["read a", "thanks", "/compact"]);
        assert_eq!(turns[2].id.as_deref(), Some("c"));
        assert!(matches!(
            turns[2].events.last(),
            Some(Event::Used(Usage {
                used: 120,
                size: 1_000
            }))
        ));
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

    /// A branch lists its answered and lost turns in the order they happened, with where it went back from.
    #[test]
    fn branches_list_their_turns_in_order() {
        let mark = |id: &str| omc_agent::Mark {
            id: id.into(),
            text: format!("p{id}"),
            files: Vec::new(),
        };
        let listed = branches(&[omc_agent::Branch {
            id: "2".into(),
            parent: Some("1".into()),
            before: Some("x".into()),
            marks: vec![(0, mark("a")), (2, mark("c"))],
            lost: vec![
                omc_agent::Lost {
                    at: 2,
                    mark: mark("b"),
                    reason: omc_agent::STOPPED.into(),
                },
                omc_agent::Lost {
                    at: 4,
                    mark: mark("d"),
                    reason: "boom".into(),
                },
            ],
        }]);
        let turns: Vec<(&str, Option<&str>)> = listed[0]
            .turns
            .iter()
            .map(|turn| (turn.id.as_str(), turn.ended.as_deref()))
            .collect();
        assert_eq!(
            turns,
            [
                ("a", None),
                ("b", Some("stopped")),
                ("c", None),
                ("d", Some("boom"))
            ]
        );
        assert_eq!(listed[0].parent.as_deref(), Some("1"));
        assert_eq!(listed[0].turns[0].prompt, "pa");
    }
}
