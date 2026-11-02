//! The registry that maps tool names to implementations.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::Value;

use crate::tools::{Bash, Edit, Glob, Grep, Read, Write};
use crate::{Sandbox, Tool, ToolError, ToolOutput};

/// A set of tools addressable by name.
#[derive(Clone, Default)]
pub struct Registry {
    /// The tools, keyed and ordered by name.
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl Registry {
    /// Creates an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a registry holding every built-in tool, confined to `root`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Io`] when `root` cannot be canonicalized.
    pub fn builtin(root: impl AsRef<std::path::Path>) -> Result<Self, ToolError> {
        let sandbox = Sandbox::new(root)?;
        let mut registry = Self::new();
        registry.register(Read::new(sandbox.clone()));
        registry.register(Grep::new(sandbox.clone()));
        registry.register(Glob::new(sandbox.clone()));
        registry.register(Bash::new(sandbox.clone()));
        registry.register(Edit::new(sandbox.clone()));
        registry.register(Write::new(sandbox));
        Ok(registry)
    }

    /// Adds `tool`, replacing any tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name(), Arc::new(tool));
    }

    /// Returns the tool called `name`, if registered.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Iterates over the registered tools in name order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// Runs the tool called `name` with `args`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Unknown`] when no such tool exists, or whatever the
    /// tool itself fails with.
    pub async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        tool.call(args).await
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;

    /// A tool that echoes its `text` argument.
    struct Echo;

    #[async_trait]
    impl Tool for Echo {
        /// Returns the tool name.
        fn name(&self) -> &'static str {
            "echo"
        }

        /// Returns the tool description.
        fn description(&self) -> &'static str {
            "Echoes text."
        }

        /// Returns an empty object schema.
        fn schema(&self) -> Value {
            json!({ "type": "object" })
        }

        /// Echoes the `text` argument back.
        async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new(args["text"].as_str().unwrap_or_default()))
        }
    }

    /// A registered tool runs by name and an unknown name is an error.
    #[tokio::test]
    async fn dispatches_by_name() {
        let mut registry = Registry::new();
        registry.register(Echo);
        let output = registry
            .call("echo", json!({ "text": "hi" }))
            .await
            .unwrap();
        assert_eq!(output.text, "hi");
        let missing = registry.call("nope", json!({})).await;
        assert!(matches!(missing, Err(ToolError::Unknown(_))));
    }
}
