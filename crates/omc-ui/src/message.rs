//! Messages delivered to the UI event loop.

use crossterm::event::Event;
use omc_auth::Provider;

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
    /// A streamed chunk of the assistant reply.
    Delta(String),
    /// The assistant reply is complete.
    Done,
    /// Sending the message failed with the given description.
    Failed(String),
    /// A default model was resolved for a provider on its first message.
    ModelChosen(Provider, String),
    /// The models a provider offers the account, best first.
    ModelList(Provider, Vec<String>),
    /// Fetching the model list failed with the given description.
    ModelListFailed(String),
}
