//! The memories a project keeps across conversations, as a client reads them.

/// One memory, as a client shows it.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryEntry {
    /// What the agent's tools name it by.
    pub id: String,
    /// Who it is for: `project` or `user`.
    pub scope: String,
    /// What it says.
    pub text: String,
    /// Words it is filed under.
    #[serde(default)]
    pub tags: Vec<String>,
    /// When it was saved, as `YYYY-MM-DDTHH:MM:SSZ`.
    pub created: String,
    /// The conversation that saved it, when that conversation had an id.
    #[serde(default)]
    pub conversation: Option<String>,
    /// When it was last changed, when it was.
    #[serde(default)]
    pub updated: Option<String>,
    /// The conversation that last changed it, when one with an id did.
    #[serde(default)]
    pub updated_in: Option<String>,
}

/// The memories a conversation in a folder would start with: its project's and the user's.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MemoryListing {
    /// Whether memory is on for conversations started like this one: off when the environment turns it off for every conversation, or there is no folder to keep memories in.
    pub enabled: bool,
    /// The short name of the project.
    pub name: String,
    /// The folder the project is shown as: the main checkout of a repository, or the folder itself outside one.
    pub project: String,
    /// The folder the project's memories are kept in; empty when there is none.
    #[serde(default)]
    pub project_folder: String,
    /// The folder the user's memories are kept in; empty when there is none.
    #[serde(default)]
    pub user_folder: String,
    /// The memories, the project's first, each scope oldest first.
    #[serde(default)]
    pub memories: Vec<MemoryEntry>,
}
