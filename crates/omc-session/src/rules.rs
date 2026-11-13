//! Instruction files that apply only to some of the project's files, handed to the agent at the end of the result of the first call that touches a matching file.
//!
//! The system prompt holds only the files that always apply, so that it stays the same for the whole conversation. A rule whose front matter names paths, and an instruction file kept in a folder below the ones the system prompt is built from, reach the model here instead. What the model was handed is read back from what each request sends, so a file whose result was pruned, compacted, rewound or dropped for a fresh context window is handed over again with the next matching call.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use omc_agent::{Followed, Hooks, Screened};
use omc_context::{Discovered, Locations, instructions_for, path_rules, path_rules_in};
use omc_providers::{Message, ToolCall};
use omc_tools::{Registry, Subject};
use serde_json::Value;

/// What the instruction files of a touched file are found with.
struct Source {
    /// Where the work is.
    locations: Arc<Locations>,
    /// What was found on disk, the scoped rules among it.
    discovered: Arc<Discovered>,
    /// The instruction files the agent is not given, by path.
    disabled: Arc<Vec<String>>,
    /// The tools, asked which files a call touches.
    tools: Arc<Registry>,
}

impl Source {
    /// The files a call of the tool `name` with `arguments` touches: the files it reads or changes, or the one file it works through; none for a tool that touches no file of its own.
    fn touched(&self, name: &str, arguments: &Value) -> Vec<PathBuf> {
        match self.tools.get(name).map(|tool| tool.subject(arguments)) {
            Some(Subject::Files(files)) => files,
            Some(Subject::Tree(path)) if path.is_file() => vec![path],
            _ => Vec::new(),
        }
    }
}

/// The checks that hand one agent the instruction files of the files it touches, each once for as long as the model still sees it.
pub(crate) struct PathRules {
    /// What the files are found with.
    source: Arc<Source>,
    /// The instruction files the model has been handed and still sees, by path.
    told: Arc<Mutex<HashSet<PathBuf>>>,
}

impl PathRules {
    /// The checks for the main agent of a conversation working in `locations` with what was `discovered`, which is not given the files `disabled` names and whose calls are those of `tools`.
    pub(crate) fn new(
        locations: Arc<Locations>,
        discovered: Arc<Discovered>,
        disabled: Arc<Vec<String>>,
        tools: Arc<Registry>,
    ) -> Self {
        Self {
            source: Arc::new(Source {
                locations,
                discovered,
                disabled,
                tools,
            }),
            told: Arc::default(),
        }
    }
}

#[async_trait]
impl Hooks for PathRules {
    /// Lets every call through as it is.
    async fn before_tool(&self, _call: &ToolCall, _arguments: &Value) -> Screened {
        Screened::default()
    }

    /// Adds the instruction files that apply to what `call` touched, and that the model does not hold yet, to the end of `output`.
    async fn after_tool(&self, call: &ToolCall, arguments: &Value, output: &str) -> Followed {
        let files = self.source.touched(&call.name, arguments);
        if files.is_empty() {
            return Followed::default();
        }
        let source = self.source.clone();
        let told = self.told.clone();
        let fresh = tokio::task::spawn_blocking(move || {
            let mut fresh = Vec::new();
            for file in &files {
                let applying = instructions_for(
                    &source.locations,
                    &source.discovered,
                    &source.disabled,
                    file,
                );
                let mut told = told.lock().unwrap_or_else(PoisonError::into_inner);
                fresh.extend(
                    applying
                        .into_iter()
                        .filter(|found| told.insert(found.path.clone())),
                );
            }
            fresh
        })
        .await
        .unwrap_or_default();
        if fresh.is_empty() {
            return Followed::default();
        }
        Followed {
            output: Some(format!("{output}\n\n{}", path_rules(&fresh).trim_end())),
            ..Followed::default()
        }
    }

    /// Takes the instruction files the tool results among `messages` still hand over as the ones the model holds.
    fn sending(&self, messages: &[Message]) {
        let held = messages
            .iter()
            .filter_map(|message| match message {
                Message::ToolResults(results) => Some(results),
                _ => None,
            })
            .flatten()
            .flat_map(|result| path_rules_in(&result.text))
            .collect();
        *self.told.lock().unwrap_or_else(PoisonError::into_inner) = held;
    }

    /// Never keeps a turn going.
    async fn stop(&self, _answer: &str, _again: bool) -> Option<String> {
        None
    }

    /// The same checks for a subagent, which starts having been handed nothing.
    fn for_subagent(&self, _id: &str, _kind: &str) -> Arc<dyn Hooks> {
        Arc::new(Self {
            source: self.source.clone(),
            told: Arc::default(),
        })
    }
}

#[cfg(test)]
/// Tests for the instruction files handed over with tool results.
mod tests {
    use super::*;
    use omc_providers::ToolResult;
    use serde_json::json;

    /// A scratch project under the workspace's git-ignored build folder, named `name`, with a nested instruction file and a rule for Rust files.
    fn project(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        for (path, text) in [
            ("AGENTS.md", "Root."),
            ("app/AGENTS.md", "App rules."),
            ("app/main.rs", "fn main() {}\n"),
            ("app/notes.txt", "notes\n"),
            (
                ".oh-my-crab/rules/rust.md",
                "---\npaths: \"*.rs\"\n---\nNo unwrap.",
            ),
        ] {
            let file = dir.join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
        dir.canonicalize().unwrap()
    }

    /// The checks for a conversation working in `dir`.
    fn rules(dir: &std::path::Path) -> PathRules {
        let locations = Locations {
            directory: dir.to_path_buf(),
            root: None,
            home: None,
            claude: None,
            codex: None,
            crab: None,
            project: true,
        };
        let discovered = omc_context::discover(&locations, &[]);
        PathRules::new(
            Arc::new(locations),
            Arc::new(discovered),
            Arc::new(Vec::new()),
            Arc::new(Registry::builtin(dir).unwrap()),
        )
    }

    /// A call of the tool `name` with `arguments`.
    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "c1".into(),
            name: name.into(),
            arguments,
        }
    }

    /// What `rules` add to the result `output` of `call`, when they add anything.
    async fn added(rules: &dyn Hooks, call: &ToolCall, output: &str) -> Option<String> {
        rules.after_tool(call, &call.arguments, output).await.output
    }

    /// The first touch of a file hands over its folder's file and the rules that cover it, once; a later touch hands over only what is new, and what the model no longer sees is handed over again.
    #[tokio::test]
    async fn rules_arrive_once_with_the_first_touch() {
        let dir = project("rules-once");
        let rules = rules(&dir);
        let read = call("read", json!({"path": "app/main.rs:1-1"}));
        let first = added(&rules, &read, "1\tfn main() {}").await.unwrap();
        assert!(first.starts_with("1\tfn main() {}\n\n<path-rules>\n"));
        let app = first.find("App rules.").unwrap();
        assert!(app < first.find("No unwrap.").unwrap());
        assert!(!first.contains("Root."));
        assert_eq!(added(&rules, &read, "again").await, None);
        let edit = call("edit", json!({"files": [{"path": "app/notes.txt"}]}));
        assert_eq!(added(&rules, &edit, "edited").await, None);
        let result = |text: &str| {
            Message::ToolResults(vec![ToolResult {
                call_id: "c1".into(),
                name: "read".into(),
                text: text.to_owned(),
                is_error: false,
                images: Vec::new(),
            }])
        };
        rules.sending(&[result(&first)]);
        assert_eq!(added(&rules, &read, "again").await, None);
        rules.sending(&[result("[left out]")]);
        let write = call("write", json!({"path": "app/new.txt", "content": "x"}));
        let again = added(&rules, &write, "written").await.unwrap();
        assert!(again.contains("App rules.") && !again.contains("No unwrap."));
    }

    /// A subagent is handed the files again, and calls that touch no file, or only files with nothing of their own, are left as they are.
    #[tokio::test]
    async fn subagents_start_fresh_and_other_calls_pass() {
        let dir = project("rules-subagent");
        let rules = rules(&dir);
        let read = call("read", json!({"path": "app/notes.txt"}));
        assert!(added(&rules, &read, "notes").await.is_some());
        let sub = rules.for_subagent("t1", "scout");
        assert!(added(sub.as_ref(), &read, "notes").await.is_some());
        assert_eq!(added(sub.as_ref(), &read, "notes").await, None);
        let root = call("read", json!({"path": "AGENTS.md"}));
        assert_eq!(added(&rules, &root, "Root.").await, None);
        let bash = call("bash", json!({"command": "cat app/main.rs"}));
        assert_eq!(added(&rules, &bash, "fn main() {}").await, None);
        let web = call("read", json!({"path": "https://example.com/a.rs"}));
        assert_eq!(added(&rules, &web, "page").await, None);
    }
}
