//! What a tool the agent uses looks like, from the first call to its result.

/// One tool the agent can be given, as a client lists it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ToolInfo {
    /// The name the agent calls the tool by.
    pub name: String,
    /// What the tool does, in a sentence.
    pub description: String,
}

/// How far a tool the agent is using has got.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ToolState {
    /// It is being run.
    Running,
    /// It finished.
    Done,
    /// It did not work.
    Failed,
}

/// What a tool does, as far as the agent says.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Kind {
    /// It reads a file.
    Read,
    /// It changes a file.
    Edit,
    /// It looks for something.
    Search,
    /// It runs a command.
    Execute,
    /// It fetches something from the network.
    Fetch,
    /// It is something else.
    Other,
}

/// How a file changed in one edit.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Diff {
    /// The file.
    pub path: String,
    /// What the file held before, when it existed.
    pub old: Option<String>,
    /// What it holds after.
    pub new: String,
}

/// A tool the agent is using, as far as it has said.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Tool {
    /// What the agent calls this use of the tool.
    pub id: String,
    /// What it is doing, when the agent has said.
    pub title: Option<String>,
    /// What kind of tool it is, when the agent has said.
    pub kind: Option<Kind>,
    /// How far it has got, when the agent has said.
    pub state: Option<ToolState>,
    /// The file, command or query it works on, when the agent has said.
    pub target: Option<String>,
    /// What it changed in a file, when it did.
    pub diff: Option<Diff>,
    /// What it printed, when it did.
    pub output: Option<String>,
    /// The call of the subagent whose work this is, when it is a subagent's.
    #[serde(default)]
    pub parent: Option<String>,
    /// The kind of subagent this call starts, when it starts one.
    #[serde(default)]
    pub agent: Option<String>,
    /// The git worktree of its own the subagent this call starts works in, and what became of its changes, when it works apart from the main agent; boxed, since most tool events carry none.
    #[serde(default)]
    pub worktree: Option<Box<Worktree>>,
}

/// The git worktree of its own an isolated subagent works in, so that its edits stay apart from the main agent's until it ends.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Worktree {
    /// The folder the subagent works in.
    pub path: String,
    /// What became of its changes once it ended; nothing while it works.
    #[serde(default)]
    pub merge: Option<Merge>,
}

/// What became of the changes an isolated subagent made in its worktree.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Merge {
    /// They were applied to the main agent's working tree, uncommitted.
    Applied {
        /// The files that changed, from the root of the repository.
        files: Vec<String>,
    },
    /// It changed nothing.
    Unchanged,
    /// They did not apply cleanly to the main agent's working tree, so none of them were applied.
    Conflicted {
        /// The file the whole patch was saved in.
        patch: String,
        /// The files whose part of the patch does not apply, from the root of the repository.
        files: Vec<String>,
    },
    /// They were not applied, for a reason other than a conflict, such as the subagent failing.
    Held {
        /// Why they were not applied.
        reason: String,
        /// The file the whole patch was saved in, when it could be saved.
        #[serde(default)]
        patch: Option<String>,
    },
}

/// A command or task that goes on running beyond the call that started it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Background {
    /// The handle the agent knows it by.
    pub id: String,
    /// The command or task that was started.
    pub label: String,
    /// Whether it still runs.
    pub running: bool,
    /// The call that started it, when the agent said so.
    #[serde(default)]
    pub call: Option<String>,
}

/// A subagent the agent started, sent when it starts and again when it ends.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Subagent {
    /// The id it is known by for as long as the conversation lasts, which [`crate::Command::StopSubagent`] names; the id of a subagent started by another subagent begins with that one's id and a `:`.
    pub id: String,
    /// The id of the `task` call that started it, as the [`Tool`] events of that call carry it.
    pub call: String,
    /// The kind of subagent.
    pub agent: String,
    /// What it is doing, in a few words.
    pub label: String,
    /// What the model it asks is called, when that is known.
    #[serde(default)]
    pub model: Option<String>,
    /// Whether it runs in the background.
    pub background: bool,
    /// How it stands.
    pub state: SubagentState,
}

/// How a subagent stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SubagentState {
    /// It runs, or waits for a seat to run.
    Running,
    /// It ended with a result.
    Finished,
    /// It ended with an error.
    Failed,
    /// The reader stopped it, or the turn it ran in was stopped.
    Stopped,
}
