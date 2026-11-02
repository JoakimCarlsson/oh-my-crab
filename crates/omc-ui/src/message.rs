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
}
