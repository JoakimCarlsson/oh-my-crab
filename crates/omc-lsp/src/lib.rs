//! Language servers the harness runs itself, for when whoever runs it has none to offer.
//!
//! [`Servers`] is a [`omc_tools::LanguageService`] that starts the usual language server of a file's language the first time a file of it is formatted or checked, speaks the language server protocol with it over its standard input and output, and stops it when it is dropped. It also answers the `lsp` tool's questions about code: definitions, references, hovers, symbols, renames and the diagnostics of the whole project. A server whose program is not installed is left out, and the files it would read get neither formatting nor diagnostics.

mod client;
mod navigate;
mod servers;
mod spec;
mod text;

pub use servers::{LOADING_WAIT, Servers};
pub use spec::{Spec, default_specs};
