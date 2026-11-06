//! Debugging programs through the debug adapter protocol, for the `debug` tool.
//!
//! [`DebugTool`] is the tool the agent calls. It starts the debug adapter of a program's language, such as `lldb-dap`, `gdb -i dap`, `dlv dap` or `debugpy`, or connects to one already listening, and speaks the debug adapter protocol with it over its standard input and output or a socket. [`Sessions`] holds the debugging sessions of one conversation, which outlive the tool call that started them and end when the conversation does. What the debugger says is turned into short text for the model: where the program stopped and why, its stack, its variables and what it printed.

mod adapters;
mod client;
mod session;
mod sessions;
mod state;
mod summary;
mod tool;
mod wire;

#[cfg(test)]
mod fake;

pub use adapters::{Adapter, Transport, adapters_file, default_adapters};
pub use sessions::Sessions;
pub use tool::DebugTool;
