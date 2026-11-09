//! What the tools that talk to the reader need from whoever runs them.

use async_trait::async_trait;
use omc_wire::{PlanStep, Question, Reply};

/// What the reader decided about a plan put to them, with what they wrote along with it, when they wrote anything.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlanVerdict {
    /// The plan is approved and plan mode is over: the agent carries it out, in the context it planned in, or in a fresh context window that starts from the plan alone when `fresh` is set.
    Approved {
        /// Whether the agent carries the plan out in a fresh context window instead of the one it planned in.
        fresh: bool,
        /// What the reader wrote along with the approval.
        note: Option<String>,
    },
    /// The plan is not approved yet: plan mode goes on, and the agent revises the plan, minding what the reader wrote when they wrote anything. What a reader who declines the question, or is gone, amounts to.
    Refine(Option<String>),
    /// The plan is rejected outright: plan mode goes on, and the agent stops and waits for the reader.
    Rejected(Option<String>),
}

impl PlanVerdict {
    /// Whether the plan was approved, which ended plan mode.
    pub fn approved(&self) -> bool {
        matches!(self, Self::Approved { .. })
    }
}

/// The reader, as the tools that report to them and ask them see it.
#[async_trait]
pub trait Host: Send + Sync {
    /// Shows the reader the agent's plan, whole, as it stands now.
    fn plan(&self, steps: Vec<PlanStep>);

    /// Puts `questions` to the reader and waits; nothing when the reader abandons the form or is gone.
    async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<Reply>>;

    /// Names the conversation `title` when it has no name yet; whether it was named, which it is not when a name was already there, since a name the reader gave or the agent gave first stays.
    fn name(&self, title: &str) -> bool;

    /// Puts `plan` to the reader for approval and waits; what they decided, with plan mode ended when they approved.
    async fn approve_plan(&self, plan: &str) -> PlanVerdict;
}
