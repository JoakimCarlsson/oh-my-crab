//! Tests of handing assignments to work categories: the `category` argument of `task` and `agent_spawn`.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use omc_context::builtin_agents;
use omc_providers::{ChatModel, Effort, ProviderError, Reply, Request, Sink};
use omc_tools::{Jobs, Registry, Tool, ToolError};
use serde_json::{Value, json};

use crate::testkit::{Router, deny_all, said, say, scratch};
use crate::{
    AgentEvent, Category, Crew, EventSink, ModelPicker, Models, Nesting, Picked, SubagentNotice,
    SubagentState, Task, TaskSettings, Throttle, crew_tools,
};

/// The events a test's `task` tool reported.
type Log = Arc<Mutex<Vec<AgentEvent>>>;

/// A picker of the slots `@quick`, whose model is `small-1`, `@deep`, whose model is the conversation's, `big-1`, and `@frail`, whose model is always down; it also knows `small-1` and `spare` by name.
struct Slots {
    /// The model of `@quick`, also found as `small-1` and `spare`.
    small: Arc<Router>,
    /// The conversation's model, which `@deep` is left to.
    big: Arc<Router>,
}

#[async_trait]
impl ModelPicker for Slots {
    /// Hands out the model the slot or the name stands for, and fails for anything else.
    async fn pick(&self, wanted: &str) -> Result<Picked, String> {
        let (model, name): (Arc<dyn ChatModel>, &str) = match wanted {
            "@quick" | "small-1" => (self.small.clone(), "small-1"),
            "spare" => (self.small.clone(), "spare"),
            "@deep" => (self.big.clone(), "big-1"),
            "@frail" => (Arc::new(Down), "frail-1"),
            other => return Err(format!("no model matching {other}")),
        };
        Ok(Picked {
            model,
            name: name.to_owned(),
            effort: Some(Effort::Low),
        })
    }
}

/// A model that is overloaded whenever it is asked.
struct Down;

#[async_trait]
impl ChatModel for Down {
    /// Fails as overloaded, to be asked again at once.
    async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
        Err(ProviderError::Status {
            status: "529".to_owned(),
            body: "overloaded".to_owned(),
            retry_after: Some(Duration::ZERO),
        })
    }
}

/// The categories of the tests: `quick`, which adds to the prompt, `deep`, which does not, and `frail`, whose backup is `spare`.
fn categories() -> Vec<Category> {
    vec![
        Category {
            name: "quick".to_owned(),
            description: "Small edits.".to_owned(),
            prompt: Some("Be brief.".to_owned()),
            backups: Vec::new(),
        },
        Category {
            name: "deep".to_owned(),
            description: "Hard work.".to_owned(),
            prompt: None,
            backups: Vec::new(),
        },
        Category {
            name: "frail".to_owned(),
            description: String::new(),
            prompt: None,
            backups: vec!["spare".to_owned()],
        },
    ]
}

/// The settings of a `task` tool over the built-in tools of a scratch folder called `name`, asking `big`, called `big-1`, with `categories` and the picker of the tests; with the log of its events. A subagent's system prompt is `SUB:` and its kind.
fn settings(
    name: &str,
    big: Arc<Router>,
    small: Arc<Router>,
    categories: Vec<Category>,
) -> (TaskSettings, Log) {
    let dir = scratch(name);
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let sink = log.clone();
    let events: Arc<EventSink> = Arc::new(move |event| sink.lock().unwrap().push(event));
    let settings = TaskSettings {
        model: big.clone(),
        models: Models {
            current: "big-1".to_owned(),
            picker: Some(Arc::new(Slots { small, big })),
            ..Models::default()
        },
        categories,
        registry: Arc::new(Registry::builtin(&dir).unwrap()),
        approvals: deny_all(),
        events,
        agents: builtin_agents(),
        effort: Some(Effort::High),
        prompt: Arc::new(|agent, _, _, _| format!("SUB:{}", agent.name)),
        jobs: Arc::new(Jobs::new(|_| {})),
        isolation: None,
        hooks: None,
        nesting: Nesting::new(1, Arc::new(Throttle::new(8))),
    };
    (settings, log)
}

/// A `task` tool as [`settings`] makes it, with the two models and the log of its events.
fn tool(name: &str, categories: Vec<Category>) -> (Task, Arc<Router>, Arc<Router>, Log) {
    let big = Router::new(|_| say("big answered"));
    let small = Router::new(|_| say("small answered"));
    let (settings, log) = settings(name, big.clone(), small.clone(), categories);
    (Task::new(settings), big, small, log)
}

/// The models the `Spawned` events of the log name, in order.
fn spawned(log: &Log) -> Vec<Option<String>> {
    log.lock()
        .unwrap()
        .iter()
        .filter_map(|event| match event {
            AgentEvent::Spawned { model, .. } => Some(model.clone()),
            _ => None,
        })
        .collect()
}

/// The subagent announcements of the log, nested ones included, in order.
fn announced(log: &Log) -> Vec<SubagentNotice> {
    /// The announcement `event` holds, under the calls that wrap it.
    fn unwrap(event: &AgentEvent) -> Option<SubagentNotice> {
        match event {
            AgentEvent::Subagent(notice) => Some(notice.clone()),
            AgentEvent::Nested { event, .. } => unwrap(event),
            _ => None,
        }
    }
    log.lock().unwrap().iter().filter_map(unwrap).collect()
}

/// The category and the model of each announcement in `notices`.
fn placed(notices: &[SubagentNotice]) -> Vec<(Option<&str>, Option<&str>)> {
    notices
        .iter()
        .map(|notice| (notice.category.as_deref(), notice.model.as_deref()))
        .collect()
}

/// A subagent started in a category is announced, when it starts and when it ends, with the category and the model its slot picked; one started without a category is announced with its model and no category.
#[tokio::test]
async fn a_category_started_subagent_is_announced_with_its_category_and_model() {
    let (task, _big, _small, log) = tool("category-announced", categories());
    task.call_as("t1", json!({"task": "x", "category": "quick"}))
        .await
        .unwrap();
    task.call_as("t2", json!({"task": "y"})).await.unwrap();
    let notices = announced(&log);
    assert_eq!(
        notices
            .iter()
            .map(|notice| (notice.id.as_str(), notice.state))
            .collect::<Vec<_>>(),
        [
            ("t1", SubagentState::Running),
            ("t1", SubagentState::Finished),
            ("t2", SubagentState::Running),
            ("t2", SubagentState::Finished),
        ]
    );
    assert_eq!(
        placed(&notices),
        [
            (Some("quick"), Some("small-1")),
            (Some("quick"), Some("small-1")),
            (None, Some("big-1")),
            (None, Some("big-1")),
        ]
    );
}

/// Without categories the schema has no `category` and the description says nothing of them; with them both the call and the items of `tasks` take `category`, and the description is the same text followed by the categories and what each is for, naming no model.
#[test]
fn categories_shape_the_schema_and_the_description() {
    let (off, ..) = tool("category-schema-off", Vec::new());
    let (on, ..) = tool("category-schema-on", categories());
    let item = |schema: &Value| schema["properties"]["tasks"]["items"]["properties"].clone();
    assert!(off.schema()["properties"].get("category").is_none());
    assert!(item(&off.schema()).get("category").is_none());
    assert_eq!(on.schema()["properties"]["category"]["type"], "string");
    assert_eq!(item(&on.schema())["category"]["type"], "string");
    assert!(!off.description().contains("ategor"));
    let added = on
        .description()
        .strip_prefix(off.description())
        .expect("the description changed before the categories");
    assert!(added.starts_with("\n\n# Categories\n"), "{added}");
    assert!(added.contains("`category` rather than naming a `model`"));
    assert!(added.ends_with("- `quick`: Small edits.\n- `deep`: Hard work.\n- `frail`"));
    assert!(!added.contains("small-1") && !added.contains("big-1"));
}

/// A category runs the subagent on the model of its slot, a slot left to the conversation on the conversation's model, and what the category adds to the prompt follows the subagent's system prompt under a heading naming it; the kind is still the one `agent` names.
#[tokio::test]
async fn a_category_picks_the_model_and_adds_to_the_prompt() {
    let (task, big, small, log) = tool("category-model", categories());
    let quick = task
        .call_as("t1", json!({"task": "x", "category": "quick"}))
        .await
        .unwrap();
    assert_eq!(said(&quick.text), "small answered");
    assert_eq!(
        small.seen.lock().unwrap()[0].system,
        "SUB:task\n\n# Category: quick\nBe brief."
    );
    let deep = task
        .call_as(
            "t2",
            json!({"task": "y", "agent": "scout", "category": "deep"}),
        )
        .await
        .unwrap();
    assert_eq!(said(&deep.text), "big answered");
    assert_eq!(big.seen.lock().unwrap()[0].system, "SUB:scout");
    assert_eq!(
        spawned(&log),
        [Some("small-1".to_owned()), Some("big-1".to_owned())]
    );
}

/// The call's category goes to the items of `tasks` that name neither a category nor a model; an item naming its own keeps it.
#[tokio::test]
async fn the_call_category_fills_the_items_that_choose_nothing() {
    let (task, big, small, _log) = tool("category-items", categories());
    let out = task
        .call_as(
            "t1",
            json!({
                "category": "deep",
                "tasks": [
                    {"task": "a"},
                    {"task": "b", "category": "quick"},
                    {"task": "c", "model": "small-1"}
                ]
            }),
        )
        .await
        .unwrap();
    assert_eq!(out.text.matches("small answered").count(), 2);
    assert_eq!(big.seen.lock().unwrap().len(), 1);
    let small = small.seen.lock().unwrap();
    assert_eq!(
        small
            .iter()
            .filter(|seen| seen.system.contains("# Category: quick"))
            .count(),
        1
    );
}

/// A category given with a model, an unknown category, and a category given while there are none are errors the model reads, and nothing runs.
#[tokio::test]
async fn categories_that_do_not_fit_are_errors() {
    let (task, big, small, _log) = tool("category-errors", categories());
    let both = task
        .call_as(
            "t1",
            json!({"task": "x", "category": "quick", "model": "small-1"}),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&both, ToolError::InvalidArguments(why) if why.contains("only one of `category` and `model`")),
        "{both}"
    );
    let unknown = task
        .call_as("t2", json!({"task": "x", "category": "nope"}))
        .await
        .unwrap_err();
    assert!(
        matches!(&unknown, ToolError::Failed(why) if why.contains("unknown category nope; available: quick, deep, frail")),
        "{unknown}"
    );
    let together = task
        .call_as(
            "t3",
            json!({"category": "quick", "model": "small-1", "tasks": [{"task": "a"}]}),
        )
        .await
        .unwrap_err();
    assert!(together.to_string().contains("only one of"), "{together}");
    let (off, ..) = tool("category-errors-off", Vec::new());
    let refused = off
        .call_as("t4", json!({"task": "x", "category": "quick"}))
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, ToolError::InvalidArguments(why) if why.contains("orchestrator mode, which is off")),
        "{refused}"
    );
    assert!(big.seen.lock().unwrap().is_empty() && small.seen.lock().unwrap().is_empty());
}

/// A category whose model keeps failing hands the subagent to the backups of its slot, and the event names the category as the slot.
#[tokio::test]
async fn a_category_falls_back_on_the_backups_of_its_slot() {
    let (task, _big, small, log) = tool("category-backups", categories());
    let out = task
        .call_as("t1", json!({"task": "x", "category": "frail"}))
        .await
        .unwrap();
    assert_eq!(said(&out.text), "small answered");
    assert_eq!(small.seen.lock().unwrap().len(), 1);
    /// The slot, the model left and the model taken of the fallback `event` is or holds.
    fn fallback(event: &AgentEvent) -> Option<(String, String, String)> {
        match event {
            AgentEvent::ModelFallback { slot, from, to, .. } => {
                Some((slot.clone(), from.clone(), to.clone()))
            }
            AgentEvent::Nested { event, .. } => fallback(event),
            _ => None,
        }
    }
    let switched: Vec<_> = log.lock().unwrap().iter().filter_map(fallback).collect();
    assert_eq!(
        switched,
        [("frail".to_owned(), "frail-1".to_owned(), "spare".to_owned())]
    );
}

/// `agent_spawn` takes `category` only while there are categories, and runs the long-lived subagent on the model of the category's slot with what the category adds to the prompt, announcing it with the category and that model.
#[tokio::test]
async fn agent_spawn_takes_a_category() {
    let by_name = |settings: TaskSettings| {
        crew_tools(Arc::new(Task::new(settings)), Arc::new(Crew::default()))
            .into_iter()
            .map(|tool| (tool.name(), tool))
            .collect::<std::collections::HashMap<_, _>>()
    };
    let big = Router::new(|_| say("big answered"));
    let small = Router::new(|_| say("small answered"));
    let (off, _) = settings("category-spawn-off", big.clone(), small.clone(), Vec::new());
    let off = by_name(off);
    assert!(
        off["agent_spawn"].schema()["properties"]
            .get("category")
            .is_none()
    );
    let (on, log) = settings(
        "category-spawn-on",
        big.clone(),
        small.clone(),
        categories(),
    );
    let on = by_name(on);
    assert_eq!(
        on["agent_spawn"].schema()["properties"]["category"]["type"],
        "string"
    );
    on["agent_spawn"]
        .call_as(
            "c1",
            json!({"name": "helper", "prompt": "look", "category": "quick"}),
        )
        .await
        .unwrap();
    let reply = on["agent_wait"]
        .call(json!({"name": "helper", "timeout": 10}))
        .await
        .unwrap();
    assert_eq!(reply.text, "small answered");
    assert!(
        small.seen.lock().unwrap()[0]
            .system
            .ends_with("# Category: quick\nBe brief.")
    );
    assert!(big.seen.lock().unwrap().is_empty());
    assert_eq!(
        placed(&announced(&log)).first(),
        Some(&(Some("quick"), Some("small-1")))
    );
}
