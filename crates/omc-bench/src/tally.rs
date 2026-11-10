//! What a run's events add up to: which tools the model called and how often they failed.

use std::collections::{BTreeMap, HashMap, HashSet};

use omc_wire::{Event, ToolState};

/// The tools whose calls change files.
const EDITING: &[&str] = &[
    "edit",
    "write",
    "ast_edit",
    "multiedit",
    "notebookedit",
    "apply_patch",
];

/// How often one tool was called in a run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Uses {
    /// How many calls the model made.
    pub(crate) calls: u32,
    /// How many of them did not work.
    pub(crate) failed: u32,
}

/// The count of a run's tool calls, kept as its events come in.
#[derive(Clone, Debug, Default)]
pub(crate) struct Tally {
    /// The tool each call that has started belongs to, by the call's id.
    started: HashMap<String, String>,
    /// How often each tool was called, by its name.
    tools: BTreeMap<String, Uses>,
    /// The most tokens the conversation held at once.
    context: u64,
    /// How many requests the model was sent.
    requests: u32,
    /// The tokens the model was sent, added up over the requests.
    sent: u64,
    /// The tokens the model wrote, added up over its replies.
    written: u64,
    /// What each `Used` event of the wire said: the tokens the conversation held and the tokens the model wrote. Crab sends one after each reply, and one that wrote nothing after a pruning or a compaction, which is no request.
    measured: Vec<(u64, u64)>,
    /// The ids of the requests counted so far, for a harness that reports one more than once.
    counted: HashSet<String>,
    /// Whether the model has said, thought or called anything yet.
    active: bool,
}

impl Tally {
    /// Counts what `event` says: a call that starts, a call that failed, and how full the context is.
    pub(crate) fn take(&mut self, event: &Event) {
        match event {
            Event::Tool(tool) => {
                self.active = true;
                if let (Some(name), Some(ToolState::Running)) = (&tool.title, tool.state) {
                    self.called(&tool.id, name);
                }
                if tool.state == Some(ToolState::Failed) {
                    self.failed(&tool.id);
                }
            }
            Event::Used(usage) => {
                self.measured.push((usage.used, usage.written));
                self.context = self.context.max(usage.used);
            }
            Event::Started(_) | Event::Said(_) | Event::Thought(_) => self.active = true,
            _ => {}
        }
    }

    /// Counts the call `id` of the tool `name`, once however often it is reported; the name is kept in lower case, so that the same tool counts the same whatever harness names it.
    pub(crate) fn called(&mut self, id: &str, name: &str) {
        self.active = true;
        if !self.started.contains_key(id) {
            let name = name.to_ascii_lowercase();
            self.tools.entry(name.clone()).or_default().calls += 1;
            self.started.insert(id.to_owned(), name);
        }
    }

    /// Counts the call `id` as one that did not work, when it was seen to start.
    pub(crate) fn failed(&mut self, id: &str) {
        if let Some(name) = self.started.get(id)
            && let Some(uses) = self.tools.get_mut(name)
        {
            uses.failed += 1;
        }
    }

    /// Counts one request to the model that sent it `tokens` tokens: the conversation as it stood then.
    pub(crate) fn request(&mut self, tokens: u64) {
        self.requests += 1;
        self.sent += tokens;
        self.context = self.context.max(tokens);
    }

    /// Counts the request `id` as [`Tally::request`] does, unless a request with that id was counted before.
    pub(crate) fn request_once(&mut self, id: &str, tokens: u64) {
        if self.counted.insert(id.to_owned()) {
            self.request(tokens);
        }
    }

    /// Counts `tokens` more tokens the model was sent, by a harness that says how many in all but not in how many requests.
    pub(crate) fn sent_more(&mut self, tokens: u64) {
        self.sent += tokens;
    }

    /// Counts `tokens` more tokens the model wrote.
    pub(crate) fn wrote(&mut self, tokens: u64) {
        self.written += tokens;
    }

    /// The `Used` events of the wire that stand for a request: the ones after a reply, which wrote something, or all of them when none says what was written, as from a crab that predates the figure.
    fn replies(&self) -> impl Iterator<Item = &(u64, u64)> {
        let known = self.measured.iter().any(|(_, written)| *written > 0);
        self.measured
            .iter()
            .filter(move |(_, written)| !known || *written > 0)
    }

    /// The tokens the model wrote, added up over its replies.
    pub(crate) fn written(&self) -> u64 {
        self.written + self.replies().map(|(_, written)| written).sum::<u64>()
    }

    /// How many requests the model was sent.
    pub(crate) fn requests(&self) -> u32 {
        self.requests + self.replies().count() as u32
    }

    /// The tokens the model was sent, added up over the requests.
    pub(crate) fn sent(&self) -> u64 {
        self.sent + self.replies().map(|(used, _)| used).sum::<u64>()
    }

    /// Whether the model has said, thought or called anything yet.
    pub(crate) fn active(&self) -> bool {
        self.active
    }

    /// The most tokens the conversation held at once.
    pub(crate) fn context(&self) -> u64 {
        self.context
    }

    /// How often each tool was called, by its name.
    pub(crate) fn into_tools(self) -> BTreeMap<String, Uses> {
        self.tools
    }
}

/// The calls in `tools` that change files, added up.
pub(crate) fn edits(tools: &BTreeMap<String, Uses>) -> Uses {
    tools
        .iter()
        .filter(|(name, _)| EDITING.contains(&name.as_str()))
        .fold(Uses::default(), |sum, (_, uses)| Uses {
            calls: sum.calls + uses.calls,
            failed: sum.failed + uses.failed,
        })
}

#[cfg(test)]
/// Tests for counting a run's tool calls.
mod tests {
    use super::*;
    use omc_wire::{Tool, Usage};

    /// A tool event for the call `id`, named `title` when it has a name, in `state`.
    fn tool(id: &str, title: Option<&str>, state: ToolState) -> Event {
        Event::Tool(Tool {
            id: id.to_owned(),
            title: title.map(str::to_owned),
            kind: None,
            state: Some(state),
            target: None,
            diff: None,
            output: None,
            parent: None,
            agent: None,
            worktree: None,
        })
    }

    /// Calls are counted once however often they report, failures go to the tool that started the call, and the extra event of a second changed file is no call.
    #[test]
    fn calls_and_failures_are_counted_by_tool() {
        let mut tally = Tally::default();
        assert!(!tally.active());
        for event in [
            tool("1", Some("read"), ToolState::Running),
            tool("1", Some("read"), ToolState::Running),
            tool("1", None, ToolState::Done),
            tool("2", Some("edit"), ToolState::Running),
            tool("2", None, ToolState::Failed),
            tool("3", Some("edit"), ToolState::Running),
            tool("3", None, ToolState::Done),
            tool("3:1", Some("edit"), ToolState::Done),
            Event::Used(Usage {
                used: 900,
                size: 1000,
                written: 70,
            }),
            Event::Used(Usage {
                used: 400,
                size: 1000,
                written: 30,
            }),
            Event::Used(Usage {
                used: 200,
                size: 1000,
                written: 0,
            }),
        ] {
            tally.take(&event);
        }
        assert!(tally.active());
        assert_eq!(tally.context(), 900);
        assert_eq!((tally.requests(), tally.sent()), (2, 1300));
        assert_eq!(tally.written(), 100);
        let tools = tally.into_tools();
        assert_eq!(
            tools["read"],
            Uses {
                calls: 1,
                failed: 0
            }
        );
        assert_eq!(
            tools["edit"],
            Uses {
                calls: 2,
                failed: 1
            }
        );
        assert_eq!(
            edits(&tools),
            Uses {
                calls: 2,
                failed: 1
            }
        );
    }
}
