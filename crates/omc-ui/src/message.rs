//! Messages delivered to the UI event loop.

use crossterm::event::Event;
use omc_agent::{AgentEvent, ApprovalRequest, Decision};
use omc_auth::Provider;
use omc_providers::Message;
use tokio::sync::oneshot;

/// Everything the event loop reacts to.
pub enum Msg {
    /// A terminal input or resize event.
    Term(Event),
    /// The authorization URL of a login in progress.
    LoginUrl(String),
    /// A login finished successfully.
    LoggedIn(Provider),
    /// A login failed with the given description.
    LoginFailed(String),
    /// Streamed text or tool activity from the running turn.
    Agent(AgentEvent),
    /// A tool call needs the user's decision, which goes back through `reply`.
    Approval {
        /// The call being asked about.
        request: ApprovalRequest,
        /// Where to send the decision.
        reply: oneshot::Sender<Decision>,
    },
    /// The turn is complete; carries the conversation including its messages.
    Done(Vec<Message>),
    /// Sending the message failed with the given description.
    Failed(String),
    /// A default model was resolved for a provider on its first message.
    ModelChosen(Provider, String),
    /// The models a provider offers the account, best first.
    ModelList(Provider, Vec<String>),
    /// Fetching the model list failed with the given description.
    ModelListFailed(String),
}
