//! The ratatui interface and event loop for oh-my-crab.

mod message;
mod run;
mod state;
mod tasks;
mod view;

pub use run::{Options, run};
