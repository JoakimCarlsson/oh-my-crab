//! Language servers the harness runs itself, for when whoever runs it has none to offer.
//!
//! [`Servers`] is a [`omc_tools::LanguageService`] that starts the usual language server of a file's language the first time a file of it is formatted or checked, speaks the language server protocol with it over its standard input and output, and stops it when it is dropped. It also answers the `lsp` tool's questions about code: definitions, references, hovers, symbols, renames, code actions, formatting a stretch of a file, the edits moving a file makes and the diagnostics of the whole project, and says how its servers stand and restarts them. A server whose program is not installed is left out, and the files it would read get neither formatting nor diagnostics.

//!
//! On Unix the servers can also be shared: [`run_daemon`] is the language server daemon of a user, which keeps one [`Servers`] per project root, and [`DaemonLanguages`] is the [`omc_tools::LanguageService`] of a conversation that asks it, falling back to servers of its own when the daemon cannot be used.

mod client;
#[cfg(unix)]
mod daemon;
mod navigate;
#[cfg(unix)]
mod protocol;
mod servers;
#[cfg(unix)]
mod shared;
mod spec;
mod text;

#[cfg(unix)]
pub use daemon::run_daemon;
#[cfg(unix)]
pub use protocol::SOCKET_VARIABLE;
pub use servers::{LOADING_WAIT, Servers};
#[cfg(unix)]
pub use shared::DaemonLanguages;
pub use spec::{Spec, default_specs};
