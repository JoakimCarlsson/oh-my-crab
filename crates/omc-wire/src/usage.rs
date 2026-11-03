//! How full the agent's context is.

/// How full the agent's context is: what a conversation holds of what it can.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// How many tokens the conversation holds.
    pub used: u64,
    /// How many tokens the context window takes in all.
    pub size: u64,
}
