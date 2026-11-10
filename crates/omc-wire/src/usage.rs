//! How full the agent's context is.

/// How full the agent's context is: what a conversation holds of what it can.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Usage {
    /// How many tokens the conversation holds.
    pub used: u64,
    /// How many tokens the context window takes in all.
    pub size: u64,
    /// How many tokens the model wrote in the reply this was measured on, reasoning included; 0 when it was measured after a compaction or a pruning, and when the harness that wrote it predates the field.
    #[serde(default)]
    pub written: u64,
}
