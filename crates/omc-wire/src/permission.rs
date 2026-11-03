//! Questions the agent puts to the reader before it goes on.

/// What a permission choice amounts to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Verdict {
    /// The agent may go ahead this once.
    Allow,
    /// The agent may go ahead, now and whenever it asks again.
    Always,
    /// The agent may not.
    Reject,
}

/// One answer the reader can give to a permission request.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Choice {
    /// What the agent calls the answer.
    pub id: String,
    /// What it is called to the reader.
    pub name: String,
    /// What the answer lets the agent do.
    pub verdict: Verdict,
}

/// A question the agent puts to the reader before it goes on.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Permission {
    /// Names this question in the answer.
    pub ticket: u64,
    /// What the agent wants to do.
    pub title: String,
    /// The command it wants to run, when it is one.
    pub command: Option<String>,
    /// The plan it wants approved before it starts, when it is in plan mode.
    #[serde(default)]
    pub plan: Option<String>,
    /// The answers on offer.
    pub choices: Vec<Choice>,
}
