//! A conversation with an agent, driven by commands and reported as events.
//!
//! [`run`] holds one conversation: it takes [`omc_wire::Command`]s, runs the
//! agent loop for each prompt, and reports what happens as
//! [`omc_wire::Event`]s. [`serve`] carries that over a pair of byte streams
//! so another program can hold the conversation.

mod bridge;
mod listing;
mod prompt;
mod serve;
mod session;
mod settings;
mod shared;
mod toolset;
mod translate;
mod turn;

pub use listing::tools;
pub use serve::serve;
pub use session::run;
