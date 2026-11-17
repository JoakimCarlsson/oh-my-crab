//! The agent's plan and how far along it is.

/// How far along the agent's plan one step is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Progress {
    /// Not begun.
    Pending,
    /// Under way.
    Doing,
    /// Finished.
    Done,
}

/// One step of the agent's plan.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    /// What the step is.
    pub text: String,
    /// How far along it is.
    pub progress: Progress,
    /// The name other steps depend on it by, when the agent gave it one; its text names it too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// The ids or texts of the steps it waits on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Whether it waits on a step that is not done yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
}
