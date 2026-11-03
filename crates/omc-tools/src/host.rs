//! What the tools that talk to the reader need from whoever runs them.

use async_trait::async_trait;
use omc_wire::{PlanStep, Question, Reply};

/// The reader, as the tools that report to them and ask them see it.
#[async_trait]
pub trait Host: Send + Sync {
    /// Shows the reader the agent's plan, whole, as it stands now.
    fn plan(&self, steps: Vec<PlanStep>);

    /// Puts `questions` to the reader and waits; nothing when the reader abandons the form or is gone.
    async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<Reply>>;

    /// Puts `plan` to the reader for approval and waits; whether they approved, with plan mode ended when they did.
    async fn approve_plan(&self, plan: &str) -> bool;
}
