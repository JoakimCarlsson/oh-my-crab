//! A tool of a tool server, as a tool of the agent.

use std::sync::Arc;

use async_trait::async_trait;
use omc_tools::{Concurrency, Spill, Tool, ToolError, ToolOutput};
use serde_json::Value;

use crate::client::{Client, Offered};

/// How long a name a tool may have, which is what the providers take.
const NAME_LIMIT: usize = 64;

/// Most bytes of a tool's text shown to the model; longer text is cut in the middle and saved whole.
const OUTPUT_LIMIT: usize = 20 * 1024;

/// A tool a server offers.
pub(crate) struct McpTool {
    /// The name the agent calls it by.
    name: &'static str,
    /// What it does.
    description: &'static str,
    /// The schema of its arguments.
    schema: Value,
    /// What the server calls it.
    remote: String,
    /// The server it belongs to.
    client: Arc<Client>,
    /// The name of the server, for describing a call.
    server: String,
    /// Where text too long to show is saved whole.
    spill: Spill,
    /// Whether the server says the tool changes nothing.
    read_only_hint: bool,
}

impl McpTool {
    /// The tool `offered` by the server called `server` that `client` is connected to, saving text too long to show in `spill`.
    pub(crate) fn new(server: &str, offered: Offered, client: Arc<Client>, spill: Spill) -> Self {
        let name = filed(server, &offered.name);
        Self {
            name: Box::leak(name.into_boxed_str()),
            description: Box::leak(offered.description.into_boxed_str()),
            schema: offered.schema,
            read_only_hint: offered.read_only_hint,
            remote: offered.name,
            client,
            server: server.to_owned(),
            spill,
        }
    }
}

/// The name a tool of `server` called `tool` has for the agent: both in one word, shortened to what providers take.
fn filed(server: &str, tool: &str) -> String {
    let clean = |text: &str| -> String {
        text.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    let mut name = format!("mcp__{}__{}", clean(server), clean(tool));
    name.truncate(NAME_LIMIT);
    name
}

#[async_trait]
impl Tool for McpTool {
    /// The name the agent calls the tool by.
    fn name(&self) -> &'static str {
        self.name
    }

    /// What the tool does.
    fn description(&self) -> &'static str {
        self.description
    }

    /// The schema of the tool's arguments.
    fn schema(&self) -> Value {
        self.schema.clone()
    }

    /// Tools of other programs always ask first.
    fn needs_approval(&self) -> bool {
        true
    }

    /// Shared when the server marks the tool with the `readOnlyHint` annotation, and otherwise exclusive, since a tool of another program can do anything.
    ///
    /// The hint only lets calls overlap; the tool still asks first and plan mode still refuses it, since the server's word is not checked.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        if self.read_only_hint {
            Concurrency::Shared
        } else {
            Concurrency::Exclusive
        }
    }

    /// The server and tool, with the arguments.
    fn describe(&self, args: &Value) -> String {
        format!("{}: {} {args}", self.server, self.remote)
    }

    /// Calls the tool on its server, cutting text longer than [`OUTPUT_LIMIT`] in the middle and saving the whole of it.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let capped = |text| self.spill.cap("mcp", text, OUTPUT_LIMIT);
        match self.client.call(&self.remote, args).await {
            Ok(text) => Ok(ToolOutput::new(capped(text))),
            Err(text) => Err(ToolError::Failed(capped(text))),
        }
    }
}

#[cfg(test)]
/// Tests for naming tools.
mod tests {
    use super::{NAME_LIMIT, filed};

    /// Names are one word of safe characters, no longer than providers take.
    #[test]
    fn names_are_safe_and_short() {
        assert_eq!(filed("my server", "do.it"), "mcp__my_server__do_it");
        assert!(filed(&"s".repeat(80), "t").len() <= NAME_LIMIT);
    }
}
