//! The `remember`, `memory_search` and `memory_edit` tools: the agent's memory that outlives conversations.

use std::sync::Arc;

use async_trait::async_trait;
use omc_index::{Embed, Embedder};
use omc_memory::{Memories, Memory, MemoryError, Scope};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Tool, ToolError, ToolOutput};

/// How many results `memory_search` gives when it is not told.
const DEFAULT_RESULTS: usize = 10;

/// The most results `memory_search` gives.
const MOST_RESULTS: usize = 50;

/// How close in meaning, as the cosine of their vectors, a memory must come to the query for the meaning alone to find it.
const MEANING_FLOOR: f32 = 0.25;

/// Runs `work` on the memories on a thread for blocking work, since it waits for file locks and reads and writes files.
///
/// # Errors
///
/// Returns [`ToolError::Failed`] with the reason when `work` fails or the thread is lost.
async fn on_disk<T: Send + 'static>(
    memories: &Arc<Memories>,
    work: impl FnOnce(&Memories) -> Result<T, MemoryError> + Send + 'static,
) -> Result<T, ToolError> {
    let memories = memories.clone();
    tokio::task::spawn_blocking(move || work(&memories))
        .await
        .map_err(|error| ToolError::Failed(error.to_string()))?
        .map_err(|error| ToolError::Failed(error.to_string()))
}

/// The scope `name` stands for.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when it stands for none.
fn scope(name: &str) -> Result<Scope, ToolError> {
    Scope::parse(name).ok_or_else(|| {
        ToolError::InvalidArguments(format!("unknown scope `{name}`; use `project` or `user`"))
    })
}

/// `memory` as the tools show it: its id, scope, when it last changed, its tags and its text.
fn shown(memory: &Memory) -> String {
    let tags = if memory.tags.is_empty() {
        String::new()
    } else {
        format!(", tags: {}", memory.tags.join(", "))
    };
    format!(
        "[{}] ({}, {}{tags})\n{}",
        memory.id,
        memory.scope.as_str(),
        memory.changed().get(..10).unwrap_or(memory.changed()),
        memory.text
    )
}

/// Arguments of a `remember` call.
#[derive(Deserialize)]
struct RememberArgs {
    /// What to remember.
    text: String,
    /// What to file it under.
    #[serde(default)]
    tags: Vec<String>,
    /// Who it is for: `project` or `user`.
    scope: Option<String>,
}

/// Saves a memory.
pub struct Remember {
    /// The memories of the conversation.
    memories: Arc<Memories>,
}

impl Remember {
    /// The tool, saving to `memories`.
    pub fn new(memories: Arc<Memories>) -> Self {
        Self { memories }
    }
}

#[async_trait]
impl Tool for Remember {
    /// Returns `remember`.
    fn name(&self) -> &'static str {
        "remember"
    }

    /// Changes nothing in the project, so that plan mode allows it.
    fn read_only(&self) -> bool {
        true
    }

    /// Changes the memories, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/remember.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The fact or lesson, as one self-contained statement."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "A few short words to file it under."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "user"],
                    "description": "`project` (default) for this repository, `user` for the user's preferences in every project."
                }
            },
            "required": ["text"]
        })
    }

    /// Describes a call by the start of what it saves.
    fn describe(&self, args: &Value) -> String {
        let text = args["text"].as_str().unwrap_or_default();
        let start: String = text.chars().take(80).collect();
        if start.len() < text.len() {
            format!("{start}…")
        } else {
            start
        }
    }

    /// Saves the memory and says its id.
    ///
    /// # Errors
    ///
    /// Fails when the scope is unknown, the text or tags are refused, or the files cannot be written.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: RememberArgs = parse_args(args)?;
        let scope = args.scope.as_deref().map_or(Ok(Scope::Project), scope)?;
        let added = on_disk(&self.memories, move |memories| {
            memories.remember(&args.text, &args.tags, scope)
        })
        .await?;
        let id = &added.memory.id;
        let whose = scope.as_str();
        Ok(ToolOutput::new(if added.duplicate {
            format!("Already remembered as `{id}` in the {whose} memory; its tags are updated.")
        } else {
            format!("Remembered as `{id}` in the {whose} memory.")
        }))
    }
}

/// Arguments of a `memory_search` call.
#[derive(Deserialize)]
struct SearchArgs {
    /// The words to look for.
    #[serde(default)]
    query: String,
    /// The tags every result must carry.
    #[serde(default)]
    tags: Vec<String>,
    /// The scope to look in: `project`, `user` or `all`.
    scope: Option<String>,
    /// The most results to give.
    limit: Option<usize>,
}

/// Searches the memories.
pub struct MemorySearch {
    /// The memories of the conversation.
    memories: Arc<Memories>,
    /// The embedding model of the code index, which finds memories by meaning too once it is on disk.
    embedder: Option<Arc<Embedder>>,
}

impl MemorySearch {
    /// The tool, searching `memories` by their words and, once the code index's embedding model is on disk, by their meaning too.
    pub fn new(memories: Arc<Memories>) -> Self {
        Self::with_embedder(memories, Embedder::shared())
    }

    /// The tool, searching `memories` by their words and, when `embedder` is given and its model can be had without downloading, by their meaning too.
    pub fn with_embedder(memories: Arc<Memories>, embedder: Option<Arc<Embedder>>) -> Self {
        Self { memories, embedder }
    }
}

/// The memories of `candidates` that match `query` and carry every tag of `tags`, best first: those [`omc_memory::search`] finds by their words fused by reciprocal rank with those whose meaning, as `model` embeds it, comes within [`MEANING_FLOOR`] of the query's, closest first.
fn hybrid(candidates: Vec<Memory>, query: &str, tags: &[String], model: &dyn Embed) -> Vec<Memory> {
    let keyword = omc_memory::search(candidates.clone(), query, tags);
    let wanted: Vec<String> = tags.iter().map(|tag| tag.trim().to_lowercase()).collect();
    let tagged: Vec<Memory> = candidates
        .into_iter()
        .filter(|memory| wanted.iter().all(|tag| memory.tags.contains(tag)))
        .collect();
    let mut texts = vec![query.to_owned()];
    texts.extend(
        tagged
            .iter()
            .map(|memory| format!("{}\n{}", memory.text, memory.tags.join(" "))),
    );
    let vectors = model.embed(&texts);
    let Some((asked, rest)) = vectors.split_first() else {
        return keyword;
    };
    let mut close: Vec<(f32, &Memory)> = tagged
        .iter()
        .zip(rest)
        .map(|(memory, vector)| (omc_index::cosine(asked, vector), memory))
        .filter(|(closeness, _)| *closeness >= MEANING_FLOOR)
        .collect();
    close.sort_by(|a, b| b.0.total_cmp(&a.0));
    let key = |memory: &Memory| (memory.scope.as_str(), memory.id.clone());
    let rankings = [
        keyword.iter().map(key).collect::<Vec<_>>(),
        close.iter().map(|(_, memory)| key(memory)).collect(),
    ];
    let fused = omc_index::fuse(&rankings);
    fused
        .into_iter()
        .filter_map(|((scope, id), _)| {
            tagged
                .iter()
                .find(|memory| memory.scope.as_str() == scope && memory.id == id)
                .cloned()
        })
        .collect()
}

#[async_trait]
impl Tool for MemorySearch {
    /// Returns `memory_search`.
    fn name(&self) -> &'static str {
        "memory_search"
    }

    /// Only reads.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/memory_search.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Words to look for; leave out to list the most recent memories."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Keep only memories carrying every one of these tags."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "user", "all"],
                    "description": "Where to look; `all` by default."
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MOST_RESULTS,
                    "description": "The most results to give; 10 by default."
                }
            }
        })
    }

    /// Describes a call by its query.
    fn describe(&self, args: &Value) -> String {
        match args["query"].as_str().filter(|query| !query.is_empty()) {
            Some(query) => query.to_owned(),
            None => "recent memories".to_owned(),
        }
    }

    /// Lists the memories that match, best first.
    ///
    /// # Errors
    ///
    /// Fails when the scope is unknown or the files cannot be read.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: SearchArgs = parse_args(args)?;
        let only = match args.scope.as_deref() {
            None | Some("all") => None,
            Some(name) => Some(scope(name)?),
        };
        let limit = args.limit.unwrap_or(DEFAULT_RESULTS).clamp(1, MOST_RESULTS);
        let (query, tags) = (args.query, args.tags);
        let embedder = self.embedder.clone();
        let found = on_disk(&self.memories, move |memories| {
            let candidates = match only {
                Some(scope) => memories.shelf(scope).list()?,
                None => memories.all()?,
            };
            let model = embedder
                .filter(|_| !omc_index::query_terms(&query).is_empty())
                .and_then(|embedder| embedder.loaded());
            Ok(match model {
                Some(model) => hybrid(candidates, &query, &tags, model.as_ref()),
                None => omc_memory::search(candidates, &query, &tags),
            })
        })
        .await?;
        if found.is_empty() {
            return Ok(ToolOutput::new("No memories match."));
        }
        let mut text = found
            .iter()
            .take(limit)
            .map(shown)
            .collect::<Vec<_>>()
            .join("\n\n");
        if found.len() > limit {
            text.push_str(&format!(
                "\n\n{} more match; narrow the query or raise `limit`.",
                found.len() - limit
            ));
        }
        Ok(ToolOutput::new(text))
    }
}

/// Arguments of a `memory_edit` call.
#[derive(Deserialize)]
struct EditArgs {
    /// The memory to change.
    id: String,
    /// Its new text.
    text: Option<String>,
    /// Its new tags.
    tags: Option<Vec<String>>,
    /// Whether to remove it.
    #[serde(default)]
    forget: bool,
}

/// Corrects or removes a memory.
pub struct MemoryEdit {
    /// The memories of the conversation.
    memories: Arc<Memories>,
}

impl MemoryEdit {
    /// The tool, changing `memories`.
    pub fn new(memories: Arc<Memories>) -> Self {
        Self { memories }
    }
}

#[async_trait]
impl Tool for MemoryEdit {
    /// Returns `memory_edit`.
    fn name(&self) -> &'static str {
        "memory_edit"
    }

    /// Changes nothing in the project, so that plan mode allows it.
    fn read_only(&self) -> bool {
        true
    }

    /// Changes the memories, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/memory_edit.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "The id of the memory."
                },
                "text": {
                    "type": "string",
                    "description": "Its new text."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Its new tags, replacing the old ones."
                },
                "forget": {
                    "type": "boolean",
                    "description": "Remove the memory for good."
                }
            },
            "required": ["id"]
        })
    }

    /// Describes a call as an update or a removal of its memory.
    fn describe(&self, args: &Value) -> String {
        let id = args["id"].as_str().unwrap_or_default();
        if args["forget"].as_bool() == Some(true) {
            format!("forget {id}")
        } else {
            format!("update {id}")
        }
    }

    /// Updates or removes the memory.
    ///
    /// # Errors
    ///
    /// Fails when no memory has the id, nothing is to change, the new text or tags are refused, or the files cannot be written.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: EditArgs = parse_args(args)?;
        if args.forget {
            let id = args.id.clone();
            let gone = on_disk(&self.memories, move |memories| memories.forget(&id)).await?;
            return Ok(ToolOutput::new(format!(
                "Forgot `{}` from the {} memory.",
                gone.id,
                gone.scope.as_str()
            )));
        }
        let changed = on_disk(&self.memories, move |memories| {
            memories.update(&args.id, args.text.as_deref(), args.tags.as_deref())
        })
        .await?;
        Ok(ToolOutput::new(format!("Updated:\n{}", shown(&changed))))
    }
}

#[cfg(test)]
/// Tests for the memory tools.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// The three tools over the memories of a made-up project in a scratch folder named `name`.
    fn tools(name: &str) -> (Remember, MemorySearch, MemoryEdit) {
        let root = scratch(name);
        let store = omc_memory::Store::new(root.join("memory"));
        let project = omc_memory::Project {
            root: root.join("app"),
            key: root.join("app/.git"),
        };
        let memories = Arc::new(Memories::new(&store, project, Some("c1".to_owned())));
        (
            Remember::new(memories.clone()),
            MemorySearch::with_embedder(memories.clone(), None),
            MemoryEdit::new(memories),
        )
    }

    /// The first word of a tool's reply that is an id in backticks.
    fn id_in(text: &str) -> String {
        text.split('`').nth(1).unwrap().to_owned()
    }

    /// A saved memory is found by a search, updated, and forgotten, after which the search finds nothing.
    #[tokio::test]
    async fn remember_search_edit_forget() {
        let (remember, search, edit) = tools("memory-tools");
        let saved = remember
            .call(json!({"text": "Staging listens on port 6543.", "tags": ["db"]}))
            .await
            .unwrap();
        let id = id_in(&saved.text);
        assert!(saved.text.contains("project memory"));
        let found = search.call(json!({"query": "staging port"})).await.unwrap();
        assert!(found.text.starts_with(&format!("[{id}] (project, ")));
        assert!(found.text.contains("tags: db") && found.text.contains("6543"));
        let user = remember
            .call(json!({"text": "Answers in British English.", "scope": "user"}))
            .await
            .unwrap();
        assert!(user.text.contains("user memory"));
        let only_user = search.call(json!({"scope": "user"})).await.unwrap();
        assert!(only_user.text.contains("British") && !only_user.text.contains("6543"));
        let updated = edit
            .call(json!({"id": id, "text": "Staging listens on port 7000."}))
            .await
            .unwrap();
        assert!(updated.text.contains("7000"));
        let gone = edit.call(json!({"id": id, "forget": true})).await.unwrap();
        assert!(gone.text.contains("Forgot"));
        let none = search.call(json!({"query": "staging"})).await.unwrap();
        assert_eq!(none.text, "No memories match.");
        assert!(edit.call(json!({"id": id, "forget": true})).await.is_err());
        assert!(
            remember
                .call(json!({"text": "x", "scope": "team"}))
                .await
                .is_err()
        );
    }

    /// An embedder that knows a few concepts: each text points along the concepts any of whose words it holds.
    struct Concepts;

    impl Embed for Concepts {
        /// Names the test model.
        fn model(&self) -> &str {
            "concepts"
        }

        /// One number per concept.
        fn dimensions(&self) -> usize {
            3
        }

        /// Marks the concepts each text mentions.
        fn embed(&self, texts: &[String]) -> Vec<Vec<f32>> {
            let concepts: [&[&str]; 3] = [
                &["database", "postgres", "sql"],
                &["docker", "containers"],
                &["deploys", "ship", "production"],
            ];
            texts
                .iter()
                .map(|text| {
                    let lower = text.to_lowercase();
                    concepts
                        .iter()
                        .map(|words| f32::from(u8::from(words.iter().any(|w| lower.contains(w)))))
                        .collect()
                })
                .collect()
        }
    }

    /// With a model, a memory is found by its meaning though it shares no word with the query, keyword matches still come, tags still filter, and an unrelated query finds nothing.
    #[tokio::test]
    async fn search_by_meaning_with_a_model() {
        let (remember, _, _) = tools("memory-tools-meaning");
        let memories = remember.memories.clone();
        let search = MemorySearch::with_embedder(
            memories,
            Some(Arc::new(Embedder::with_model(Arc::new(Concepts)))),
        );
        for (text, tags) in [
            ("The staging database listens on port 6543.", vec!["db"]),
            ("Integration tests need Docker running.", vec![]),
            ("Deploys go through the release workflow.", vec![]),
        ] {
            remember
                .call(json!({"text": text, "tags": tags}))
                .await
                .unwrap();
        }
        let postgres = search.call(json!({"query": "postgres"})).await.unwrap();
        assert!(postgres.text.contains("6543"), "{}", postgres.text);
        assert_eq!(postgres.text.matches("(project, ").count(), 1);
        let both = search
            .call(json!({"query": "ship containers"}))
            .await
            .unwrap();
        assert!(both.text.contains("Docker") && both.text.contains("Deploys"));
        let keyword = search.call(json!({"query": "integration"})).await.unwrap();
        assert!(keyword.text.contains("Docker"));
        let tagged = search
            .call(json!({"query": "containers", "tags": ["db"]}))
            .await
            .unwrap();
        assert_eq!(tagged.text, "No memories match.");
        let none = search.call(json!({"query": "kubernetes"})).await.unwrap();
        assert_eq!(none.text, "No memories match.");
    }

    /// A search over more matches than its limit says how many more there are.
    #[tokio::test]
    async fn search_limits_its_results() {
        let (remember, search, _) = tools("memory-tools-limit");
        for n in 0..4 {
            remember
                .call(json!({"text": format!("Fact number {n}.")}))
                .await
                .unwrap();
        }
        let found = search
            .call(json!({"query": "fact", "limit": 2}))
            .await
            .unwrap();
        assert_eq!(found.text.matches("(project, ").count(), 2);
        assert!(
            found
                .text
                .ends_with("2 more match; narrow the query or raise `limit`.")
        );
    }
}
