//! What the harness tells its client, in the order it happened.

use crate::inquiry::Inquiry;
use crate::permission::Permission;
use crate::plan::PlanStep;
use crate::setting::{Setting, Slash};
use crate::tool::{Background, Tool};
use crate::usage::Usage;

/// What the agent did, in the order it did it.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Event {
    /// What can be changed about the conversation, and how it stands now.
    Settings(Vec<Setting>),
    /// The commands the agent offers behind a slash.
    Commands(Vec<Slash>),
    /// How full the conversation's context is now.
    Used(Usage),
    /// The agent said this much more of its reply.
    Said(String),
    /// The agent thought this much more.
    Thought(String),
    /// The agent started using a tool, or said more about one it was using.
    Tool(Tool),
    /// A command or task left running beyond its call started, or ended.
    Background(Background),
    /// The turn was stopped, so that nothing the agent had running goes on.
    Halted,
    /// The agent's plan, whole, as it stands now.
    Plan(Vec<PlanStep>),
    /// The agent waits for an answer to this.
    Ask(Permission),
    /// The agent waits for the answers to this form of questions.
    Inquire(Inquiry),
    /// The turn ended, with the reason it failed where it did.
    Finished(Option<String>),
    /// The turn ended because the account needs signing in again.
    SignedOut {
        /// Why authentication failed.
        reason: String,
        /// The name of the account, when it has one.
        account: Option<String>,
    },
    /// The account was signed in.
    SignedIn,
    /// The address the reader opens in a browser to sign in.
    LoginUrl(String),
    /// The harness could not start or went on, for this reason.
    Failed(String),
    /// The agent named the conversation, or named it again.
    Titled(String),
}
