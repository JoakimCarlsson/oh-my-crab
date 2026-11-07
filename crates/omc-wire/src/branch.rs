//! The branches of a saved conversation: going back to before a turn starts a new branch, and the turns left behind stay on the branch they were on.

/// One branch of a saved conversation: the turns from its start to its tip.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Branch {
    /// The branch's id, given by the harness: `1` for the first, counting up from there; what [`crate::Command::Switch`] names it by.
    pub id: String,
    /// The branch it went back from, or nothing for the first.
    #[serde(default)]
    pub parent: Option<String>,
    /// The id of the turn of `parent` it went back to before, or nothing for the first; the turns of `parent` before that one are the turns the two share.
    #[serde(default)]
    pub before: Option<String>,
    /// Its turns that have an id, oldest first, the ones it shares with `parent` included.
    #[serde(default)]
    pub turns: Vec<BranchTurn>,
}

/// One turn of a [`Branch`].
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BranchTurn {
    /// The id the client gave the turn.
    pub id: String,
    /// What the reader prompted with.
    pub prompt: String,
    /// How a turn that has no answer ended: `stopped`, `interrupted`, or the failure; nothing for a turn that answered.
    #[serde(default)]
    pub ended: Option<String>,
}
