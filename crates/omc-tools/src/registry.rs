//! The registry that maps tool names to implementations.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::Value;

use crate::coerce::{coerce, prepare};
use crate::tools::{
    AstEdit, AstGrep, Bash, Edit, Eval, Glob, Grep, LOAD_TOOLS, LoadTools, Lsp, Read, WebSearch,
    Write,
};
use crate::{Deferral, Jobs, Languages, Sandbox, Tool, ToolError, ToolOutput};

/// A set of tools addressable by name.
#[derive(Clone, Default)]
pub struct Registry {
    /// The tools, keyed and ordered by name.
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
    /// The names of the tools left out of the tool list until the model looks them up with `load_tools`.
    deferred: BTreeSet<&'static str>,
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
        Self::builtin_with(root, None)
    }

    /// Creates a registry holding every built-in tool, confined to `root`, in which `bash` can start background runs in `jobs` when they are given.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Io`] when `root` cannot be canonicalized.
    pub fn builtin_with(
        root: impl AsRef<std::path::Path>,
        jobs: Option<Arc<Jobs>>,
    ) -> Result<Self, ToolError> {
        Self::builtin_in(root, jobs, Languages::default())
    }

    /// Creates a registry holding every built-in tool, confined to `root`, in which `bash` can start background runs in `jobs` when they are given, and `edit` and `write` have the files they write formatted by `languages` and report what it says is wrong with them.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Io`] when `root` cannot be canonicalized.
    pub fn builtin_in(
        root: impl AsRef<std::path::Path>,
        jobs: Option<Arc<Jobs>>,
        languages: Languages,
    ) -> Result<Self, ToolError> {
        Ok(Self::builtin_for(Sandbox::new(root)?, jobs, languages))
    }

    /// Creates a registry holding every built-in tool, confined to `sandbox` and saving long output where it says, in which `bash` can start background runs in `jobs` when they are given, and `edit` and `write` have the files they write formatted by `languages` and report what it says is wrong with them.
    pub fn builtin_for(sandbox: Sandbox, jobs: Option<Arc<Jobs>>, languages: Languages) -> Self {
        let mut registry = Self::new();
        registry.register(Read::new(sandbox.clone()));
        registry.register(Grep::new(sandbox.clone()));
        registry.register(Glob::new(sandbox.clone()));
        registry.register(AstGrep::new(sandbox.clone()));
        registry.register(match jobs {
            Some(jobs) => Bash::with_jobs(sandbox.clone(), jobs),
            None => Bash::new(sandbox.clone()),
        });
        registry.register(Edit::with_languages(sandbox.clone(), languages.clone()));
        registry.register(AstEdit::new(sandbox.clone()));
        registry.register(Eval::new(sandbox.clone()));
        registry.register(Lsp::new(sandbox.clone(), languages.clone()));
        registry.register(Write::with_languages(sandbox, languages));
        registry.register(WebSearch::new());
        registry
    }

    /// Adds `tool`, replacing any tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name(), Arc::new(tool));
    }

    /// Adds a tool that is already shared, replacing any tool with the same name.
    pub fn register_shared(&mut self, tool: Arc<dyn Tool>) {
        self.tools.insert(tool.name(), tool);
    }

    /// Takes the tool called `name` out, when it is registered.
    pub fn remove(&mut self, name: &str) {
        self.tools.remove(name);
    }

    /// Leaves the tools `deferral` names out of the tool list until the model looks them up; they stay registered and run like any other when called.
    ///
    /// [`Registry::fitted`] then gives the registry the `load_tools` tool that names them and hands over their schemas.
    pub fn defer(&mut self, deferral: &Deferral) {
        let names: Vec<&'static str> = self
            .tools
            .keys()
            .copied()
            .filter(|name| deferral.defers(name))
            .collect();
        self.deferred.extend(names);
    }

    /// Leaves the tool called `name` out of the tool list until the model looks it up, when it is registered.
    pub fn defer_named(&mut self, name: &str) {
        if let Some((name, _)) = self.tools.get_key_value(name) {
            self.deferred.insert(*name);
        }
    }

    /// Whether the tool called `name` is registered and left out of the tool list until looked up.
    pub fn is_deferred(&self, name: &str) -> bool {
        self.deferred.contains(name) && self.tools.contains_key(name)
    }

    /// Whether any registered tool is left out of the tool list until looked up.
    pub fn has_deferred(&self) -> bool {
        self.deferred
            .iter()
            .any(|name| self.tools.contains_key(name))
    }

    /// Iterates over the tools on the tool list, the ones not left out until looked up, in name order.
    pub fn offered(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools
            .iter()
            .filter(|(name, _)| !self.deferred.contains(*name))
            .map(|(_, tool)| tool)
    }

    /// A registry with only the tools that satisfy `keep`.
    pub fn filtered(&self, keep: impl Fn(&dyn Tool) -> bool) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .filter(|(_, tool)| keep(tool.as_ref()))
                .map(|(name, tool)| (*name, tool.clone()))
                .collect(),
            deferred: self.deferred.clone(),
        }
    }

    /// A registry in which every tool that can start background runs is replaced by a copy that cannot.
    pub fn foreground(&self) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .map(|(name, tool)| (*name, tool.foreground().unwrap_or_else(|| tool.clone())))
                .collect(),
            deferred: self.deferred.clone(),
        }
    }

    /// The same registry with every tool that depends on what else the agent has replaced by its copy made for the tools here, and with a `load_tools` tool naming the tools left out of the tool list when there are any, or without one when there are none.
    pub fn fitted(&self) -> Self {
        let mut fitted = Self {
            tools: self
                .tools
                .iter()
                .map(|(name, tool)| (*name, tool.fitted(self).unwrap_or_else(|| tool.clone())))
                .collect(),
            deferred: self.deferred.clone(),
        };
        fitted.remove(LOAD_TOOLS);
        if fitted.has_deferred() {
            let lookup = LoadTools::listing(&fitted);
            fitted.register(lookup);
        }
        fitted
    }

    /// A registry with only the tools that never change anything, and the copies that only look of the tools that change something in some calls only.
    pub fn read_only(&self) -> Self {
        Self {
            tools: self
                .tools
                .iter()
                .filter_map(|(name, tool)| {
                    if tool.read_only() {
                        Some((*name, tool.clone()))
                    } else {
                        tool.looking().map(|looking| (*name, looking))
                    }
                })
                .collect(),
            deferred: self.deferred.clone(),
        }
    }

    /// The names of the registered tools, in name order.
    pub fn names(&self) -> Vec<String> {
        self.tools.keys().map(|name| (*name).to_owned()).collect()
    }

    /// Returns the tool called `name`, if registered.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Iterates over the registered tools in name order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    /// Runs the tool called `name` with `args` for the call numbered `id`, after putting the arguments right against the tool's schema and checking them.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Unknown`] when no such tool exists,
    /// [`ToolError::InvalidArguments`] naming every place the arguments break
    /// the schema, or whatever the tool itself fails with.
    pub async fn call_as(
        &self,
        id: &str,
        name: &str,
        args: Value,
    ) -> Result<ToolOutput, ToolError> {
        let (tool, args) = self.prepared(name, args)?;
        tool.call_as(id, args).await
    }

    /// Runs the tool called `name` with `args`, after putting the arguments right against the tool's schema and checking them.
    ///
    /// # Errors
    ///
    /// Same as [`Registry::call_as`].
    pub async fn call(&self, name: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let (tool, args) = self.prepared(name, args)?;
        tool.call(args).await
    }

    /// The tool called `name` and `args` put right against its schema: checked as well, unless the tool checks its arguments itself.
    ///
    /// A caller that shows or approves a call before running it prepares the arguments with this first, so that what is shown and approved is what runs, and then runs the tool with them.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Unknown`] when no such tool exists, and
    /// [`ToolError::InvalidArguments`] when the arguments break the schema.
    pub fn prepared(&self, name: &str, args: Value) -> Result<(&Arc<dyn Tool>, Value), ToolError> {
        let tool = self
            .get(name)
            .ok_or_else(|| ToolError::Unknown(name.to_string()))?;
        let schema = tool.schema();
        let args = if tool.checks_arguments() {
            coerce(&schema, args)
        } else {
            prepare(&schema, args)?
        };
        Ok((tool, args))
    }
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::Concurrency;

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

    /// Read-only filtering keeps the tools that look and drops those that change things.
    #[test]
    fn read_only_filtering() {
        let registry = Registry::builtin(crate::testutil::scratch("registry-read-only")).unwrap();
        assert_eq!(
            registry.read_only().names(),
            vec!["ast_grep", "glob", "grep", "lsp", "read", "web_search"]
        );
        assert!(registry.read_only().get("lsp").unwrap().read_only());
        assert!(!registry.get("lsp").unwrap().read_only());
        assert_eq!(registry.names().len(), 11);
    }

    /// The tools that only look are shared and the ones that change things exclusive; `lsp` is shared except for the actions that write files.
    #[test]
    fn concurrency_classes() {
        let registry = Registry::builtin(crate::testutil::scratch("registry-concurrency")).unwrap();
        let class = |name: &str, args: Value| registry.get(name).unwrap().concurrency(&args);
        for name in ["read", "grep", "glob", "ast_grep", "web_search"] {
            assert_eq!(class(name, json!({})), Concurrency::Shared, "{name}");
        }
        for name in ["bash", "edit", "write", "ast_edit"] {
            assert_eq!(class(name, json!({})), Concurrency::Exclusive, "{name}");
        }
        assert_eq!(
            class("lsp", json!({"action": "references"})),
            Concurrency::Shared
        );
        assert_eq!(
            class("lsp", json!({"action": "rename", "dry_run": true})),
            Concurrency::Shared
        );
        assert_eq!(
            class("lsp", json!({"action": "rename"})),
            Concurrency::Exclusive
        );
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

    /// Arguments a model got slightly wrong are put right before a built-in tool sees them, and ones that cannot be put right are turned away naming what is wrong.
    #[tokio::test]
    async fn arguments_are_put_right_or_turned_away() {
        let dir = crate::testutil::scratch("registry-coerce");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let registry = Registry::builtin(&dir).unwrap();
        let found = registry
            .call(
                "grep",
                json!({ "pattern": "one", "case_insensitive": "true", "limit": "5", "glob": null }),
            )
            .await
            .unwrap();
        assert!(found.text.contains("a.txt"));
        let Err(ToolError::InvalidArguments(message)) =
            registry.call("grep", json!({ "limit": "lots" })).await
        else {
            panic!("expected the arguments to be turned away");
        };
        assert!(message.contains("args: missing required property `pattern`"));
        assert!(message.contains("args.limit: expected integer, got string"));
    }
}
