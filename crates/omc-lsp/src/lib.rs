//! Language servers the harness runs itself, for when whoever runs it has none to offer.
//!
//! [`Servers`] is a [`omc_tools::LanguageService`] that starts the usual language server of a file's language the first time a file of it is formatted or checked, speaks the language server protocol with it over its standard input and output, and stops it when it is dropped. A server whose program is not installed is left out, and the files it would read get neither formatting nor diagnostics.

mod client;
mod servers;
mod spec;
mod text;

pub use servers::{LOADING_WAIT, Servers};
pub use spec::{Spec, default_specs};
