//! Language servers the harness runs itself, for when whoever runs it has none to offer.
//!
//! [`Servers`] is a [`omc_tools::LanguageService`] that starts the usual language server of a file's language the first time a file of it is formatted or checked, speaks the language server protocol with it over its standard input and output, and stops it when it is dropped. It also answers the `lsp` tool's questions about code: definitions, references, hovers, symbols, renames, code actions, formatting a stretch of a file, the edits moving a file makes and the diagnostics of the whole project, and says how its servers stand and restarts them. A server whose program is not installed is left out, and the files it would read get neither formatting nor diagnostics.

mod client;
mod navigate;
mod servers;
mod spec;
mod text;

pub use servers::{LOADING_WAIT, Servers};
pub use spec::{Spec, default_specs};
