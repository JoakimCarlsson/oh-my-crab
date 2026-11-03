//! What a tool the agent uses looks like, from the first call to its result.

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
