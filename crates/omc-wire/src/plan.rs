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
}
