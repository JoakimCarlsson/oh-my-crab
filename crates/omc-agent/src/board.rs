//! The task list a crew shares: work the lead and the long-lived subagents add, claim, finish and release through the `crew_tasks` tool.
//!
//! It is kept apart from the conversation's todo list, which the main agent alone writes, whole, for the reader to follow. This list is changed one task at a time by several agents at once, so every change is made under one lock: a task claimed by one member cannot be claimed by another, and a task cannot be claimed before every task it depends on is done. It lives in the `crab` process with the crew, and goes when the crew is dismissed.

use std::sync::{Mutex, Weak};

use async_trait::async_trait;
use omc_tools::{Concurrency, Tool, ToolError, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::crew::Crew;

/// The most tasks one list holds.
pub(crate) const BOARD_MOST: usize = 200;

/// The longest result a finished task keeps, in characters.
const RESULT_MOST: usize = 200;

/// One task of the list.
struct Card {
    /// Its id, such as `t3`.
    id: String,
    /// What it is, in a line.
    title: String,
    /// More about it, when it was given.
    detail: Option<String>,
    /// The ids of the tasks that must be done before it can be claimed.
    after: Vec<String>,
    /// The member working on it, when one claimed it or was assigned it.
    owner: Option<String>,
    /// Its one-line result, once it is done.
    result: Option<String>,
}

impl Card {
    /// The line that lists it, with its state, followed by its detail when it has one; `waiting` names the tasks it depends on that are not done.
    fn line(&self, waiting: &[&str]) -> String {
        let state = match (&self.result, &self.owner) {
            (Some(result), owner) => format!(
                "done{}: {result}",
                owner
                    .as_deref()
                    .map(|owner| format!(" by {owner}"))
                    .unwrap_or_default()
            ),
            (None, Some(owner)) => format!("claimed by {owner}"),
            (None, None) if !waiting.is_empty() => format!("waiting on {}", waiting.join(", ")),
            (None, None) => "open".to_owned(),
        };
        let after = if self.after.is_empty() {
            String::new()
        } else {
            format!(" (after {})", self.after.join(", "))
        };
        let mut line = format!("- {} [{state}] {}{after}", self.id, self.title);
        if let Some(detail) = &self.detail {
            for part in detail.lines() {
                line.push_str("\n    ");
                line.push_str(part);
            }
        }
        line
    }
}

/// What the cards and the number of the next one are.
#[derive(Default)]
struct Cards {
    /// The tasks, in the order they were added.
    cards: Vec<Card>,
    /// How many tasks were ever added, which numbers the next one.
    added: usize,
}

impl Cards {
    /// The task `id`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no task with that id.
    fn find(&mut self, id: &str) -> Result<&mut Card, ToolError> {
        self.cards
            .iter_mut()
            .find(|card| card.id == id)
            .ok_or_else(|| ToolError::Failed(format!("there is no task {id}; list them first")))
    }

    /// The ids of the tasks `after` names that are not done.
    fn waiting<'a>(&'a self, after: &'a [String]) -> Vec<&'a str> {
        after
            .iter()
            .filter(|id| {
                self.cards
                    .iter()
                    .any(|card| &card.id == *id && card.result.is_none())
            })
            .map(String::as_str)
            .collect()
    }

    /// Checks that the task `id` may be taken by someone now: it is not done and every task it depends on is.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no such task, it is done, or tasks it depends on are not done.
    fn takeable(&mut self, id: &str) -> Result<(), ToolError> {
        let card = self.find(id)?;
        if card.result.is_some() {
            return Err(ToolError::Failed(format!("{id} is done already")));
        }
        let after = card.after.clone();
        let waiting = self.waiting(&after);
        if waiting.is_empty() {
            Ok(())
        } else {
            Err(ToolError::Failed(format!(
                "{id} waits on {}, which {} not done yet",
                waiting.join(", "),
                if waiting.len() == 1 { "is" } else { "are" }
            )))
        }
    }
}

/// The task list of a crew.
#[derive(Default)]
pub(crate) struct Board {
    /// The tasks, under the one lock every change is made under.
    cards: Mutex<Cards>,
}

impl Board {
    /// The tasks, locked; a poisoned lock is taken over, since every change to it is whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, Cards> {
        self.cards
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Forgets every task.
    pub(crate) fn clear(&self) {
        *self.lock() = Cards::default();
    }

    /// Adds a task called `title`, with `detail` when given, that can only be claimed once the tasks `after` are done, and returns its id.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an empty title, and [`ToolError::Failed`] when a task named in `after` does not exist or the list is full.
    pub(crate) fn add(
        &self,
        title: &str,
        detail: Option<&str>,
        after: &[String],
    ) -> Result<String, ToolError> {
        let title = title.trim();
        if title.is_empty() {
            return Err(ToolError::InvalidArguments("`title` is empty".to_owned()));
        }
        let mut cards = self.lock();
        if cards.cards.len() >= BOARD_MOST {
            return Err(ToolError::Failed(format!(
                "the list holds {BOARD_MOST} tasks, the most it may"
            )));
        }
        let after: Vec<String> = after.iter().map(|id| id.trim().to_owned()).collect();
        if let Some(missing) = after
            .iter()
            .find(|id| !cards.cards.iter().any(|card| &card.id == *id))
        {
            return Err(ToolError::Failed(format!(
                "there is no task {missing} to depend on; list them first"
            )));
        }
        cards.added += 1;
        let id = format!("t{}", cards.added);
        cards.cards.push(Card {
            id: id.clone(),
            title: title.to_owned(),
            detail: detail
                .map(str::trim)
                .filter(|detail| !detail.is_empty())
                .map(str::to_owned),
            after,
            owner: None,
            result: None,
        });
        Ok(id)
    }

    /// Every task, one line each with its state, or a line saying there are none.
    pub(crate) fn list(&self) -> String {
        let cards = self.lock();
        if cards.cards.is_empty() {
            return "The task list is empty; add tasks with action `add`.".to_owned();
        }
        cards
            .cards
            .iter()
            .map(|card| card.line(&cards.waiting(&card.after)))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Gives the task `id` to `who`, which claims it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no such task, it is done, another member holds it, or tasks it depends on are not done.
    pub(crate) fn claim(&self, id: &str, who: &str) -> Result<(), ToolError> {
        let mut cards = self.lock();
        cards.takeable(id)?;
        let card = cards.find(id)?;
        if let Some(owner) = &card.owner {
            return if owner == who {
                Ok(())
            } else {
                Err(ToolError::Failed(format!(
                    "{id} is claimed by {owner}; pick another task"
                )))
            };
        }
        card.owner = Some(who.to_owned());
        Ok(())
    }

    /// Gives the task `id` to the member `to`, whoever held it, as only the lead does.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no such task, it is done, or tasks it depends on are not done.
    pub(crate) fn assign(&self, id: &str, to: &str) -> Result<Option<String>, ToolError> {
        let mut cards = self.lock();
        cards.takeable(id)?;
        let card = cards.find(id)?;
        Ok(card.owner.replace(to.to_owned()))
    }

    /// Marks the task `id` done with `result`, its first line kept, on behalf of `who`: a member, which must hold it, or the lead when `None`.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an empty result, and [`ToolError::Failed`] when there is no such task, it is done already, or the member does not hold it.
    pub(crate) fn finish(
        &self,
        id: &str,
        who: Option<&str>,
        result: &str,
    ) -> Result<(), ToolError> {
        let line = result.lines().map(str::trim).find(|line| !line.is_empty());
        let Some(line) = line else {
            return Err(ToolError::InvalidArguments(
                "`result` is empty; say in a line what came of the task".to_owned(),
            ));
        };
        let line: String = line.chars().take(RESULT_MOST).collect();
        let mut cards = self.lock();
        let card = cards.find(id)?;
        if card.result.is_some() {
            return Err(ToolError::Failed(format!("{id} is done already")));
        }
        if let Some(who) = who
            && card.owner.as_deref() != Some(who)
        {
            return Err(ToolError::Failed(format!(
                "{id} is not yours; claim it first"
            )));
        }
        card.result = Some(line);
        Ok(())
    }

    /// Gives the task `id` up on behalf of `who`, a member, which must hold it, or the lead when `None`, so that any member may claim it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no such task, it is done, nobody holds it, or the member does not.
    pub(crate) fn release(&self, id: &str, who: Option<&str>) -> Result<(), ToolError> {
        let mut cards = self.lock();
        let card = cards.find(id)?;
        if card.result.is_some() {
            return Err(ToolError::Failed(format!("{id} is done already")));
        }
        let Some(owner) = &card.owner else {
            return Err(ToolError::Failed(format!("{id} is not claimed")));
        };
        if let Some(who) = who
            && owner != who
        {
            return Err(ToolError::Failed(format!(
                "{id} is claimed by {owner}, not by you"
            )));
        }
        card.owner = None;
        Ok(())
    }

    /// Gives up every task `who` holds and has not finished, as when it ends.
    pub(crate) fn release_all(&self, who: &str) {
        for card in &mut self.lock().cards {
            if card.result.is_none() && card.owner.as_deref() == Some(who) {
                card.owner = None;
            }
        }
    }
}

/// The `crew_tasks` tool: the task list of a crew, as the lead or one member of it reads and changes it.
pub(crate) struct CrewTasks {
    /// The crew whose list it is; gone once the conversation ended.
    pub(crate) crew: Weak<Crew>,
    /// The name of the member using it, or `None` for the lead.
    pub(crate) who: Option<String>,
}

/// Arguments of a `crew_tasks` call.
#[derive(Deserialize)]
struct BoardArgs {
    /// What to do: `add`, `list`, `claim`, `done`, `release` or `assign`.
    action: String,
    /// The task acted on, for every action but `add` and `list`.
    id: Option<String>,
    /// What a task added is, in a line.
    title: Option<String>,
    /// More about a task added.
    detail: Option<String>,
    /// The tasks a task added waits on.
    #[serde(default)]
    after: Vec<String>,
    /// What came of a task marked done, in a line.
    result: Option<String>,
    /// The member a task is assigned to.
    to: Option<String>,
}

impl BoardArgs {
    /// The id of the task acted on.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the call gives none.
    fn id(&self) -> Result<&str, ToolError> {
        self.id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| {
                ToolError::InvalidArguments(format!("`{}` needs the task's `id`", self.action))
            })
    }
}

#[async_trait]
impl Tool for CrewTasks {
    /// Returns `crew_tasks`.
    fn name(&self) -> &'static str {
        "crew_tasks"
    }

    /// Says what the tool does, as the lead or a member uses it.
    fn description(&self) -> &str {
        if self.who.is_some() {
            "The task list your crew shares with the lead. `list` shows every task with its state; `claim` takes an open task for you, which no other member can then take, and fails when another holds it or tasks it waits on are not done; `done` marks a task you hold finished with a one-line `result`; `release` gives one you hold back; `add` adds a task, with `after` naming tasks it waits on. Claim before you start on a task, and mark it done when you finish."
        } else {
            "The task list you share with your long-lived subagents, which they claim work from. `add` adds a task (`title`, optional `detail`, and `after`, the ids of tasks it waits on); `list` shows every task with its state; `assign` gives a task to a member by name, taking it from whoever held it; `release` makes a task open again; `done` marks one finished with a one-line `result`. Members see the list with their own `crew_tasks` tool; tell them with agent_send to pick tasks from it."
        }
    }

    /// Returns the argument schema; `assign` is the lead's alone, and `claim` the members'.
    fn schema(&self) -> Value {
        let actions = if self.who.is_some() {
            json!(["add", "list", "claim", "done", "release"])
        } else {
            json!(["add", "list", "assign", "done", "release"])
        };
        let mut schema = json!({
            "type": "object",
            "properties": {
                "action": { "type": "string", "enum": actions },
                "id": { "type": "string", "description": "The task acted on, such as `t3`; for every action but add and list." },
                "title": { "type": "string", "description": "For add: what the task is, in a line." },
                "detail": { "type": "string", "description": "For add: more about the task." },
                "after": { "type": "array", "items": { "type": "string" }, "description": "For add: the ids of the tasks that must be done before this one can be claimed." },
                "result": { "type": "string", "description": "For done: what came of the task, in one line." }
            },
            "required": ["action"]
        });
        if self.who.is_none() {
            schema["properties"]["to"] =
                json!({ "type": "string", "description": "For assign: the member's name." });
        }
        schema
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Shared for `list`; every other action changes the list others read, so it runs alone, in the model's order.
    fn concurrency(&self, args: &Value) -> Concurrency {
        if args["action"] == "list" {
            Concurrency::Shared
        } else {
            Concurrency::Exclusive
        }
    }

    /// Describes a call by its action and the task's id or title.
    fn describe(&self, args: &Value) -> String {
        let action = args["action"].as_str().unwrap_or_default();
        let what = args["id"]
            .as_str()
            .or_else(|| args["title"].as_str())
            .unwrap_or_default();
        format!("{action} {what}").trim().to_owned()
    }

    /// Carries out the action and says what came of it, or lists the tasks.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an unknown action, an action the caller may not take, or one missing what it needs, and [`ToolError::Failed`] when the crew is gone or the list refuses the change, saying why.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: BoardArgs = parse_args(args)?;
        let crew = self
            .crew
            .upgrade()
            .ok_or_else(|| ToolError::Failed("the crew is gone".to_owned()))?;
        let board = crew.board();
        let who = self.who.as_deref();
        let said = match (args.action.as_str(), who) {
            ("list", _) => board.list(),
            ("add", _) => {
                let title = args.title.as_deref().unwrap_or_default();
                let id = board.add(title, args.detail.as_deref(), &args.after)?;
                format!("Added {id}.")
            }
            ("claim", Some(who)) => {
                let id = args.id()?;
                board.claim(id, who)?;
                format!("{id} is yours; mark it done with a one-line result when you finish.")
            }
            ("assign", None) => {
                let id = args.id()?;
                let to = args.to.as_deref().map(str::trim).unwrap_or_default();
                if !crew.is_member(to) {
                    return Err(ToolError::Failed(format!(
                        "no long-lived subagent is called {to}; agent_list lists them"
                    )));
                }
                match board.assign(id, to)? {
                    Some(from) if from != to => format!("{id} is now {to}'s, taken from {from}."),
                    _ => format!("{id} is now {to}'s; tell it with agent_send."),
                }
            }
            ("done", _) => {
                let id = args.id()?;
                board.finish(id, who, args.result.as_deref().unwrap_or_default())?;
                format!("{id} is done.")
            }
            ("release", _) => {
                let id = args.id()?;
                board.release(id, who)?;
                format!("{id} is open again.")
            }
            ("claim", None) => {
                return Err(ToolError::InvalidArguments(
                    "the lead does not claim tasks; `assign` gives one to a member".to_owned(),
                ));
            }
            ("assign", Some(_)) => {
                return Err(ToolError::InvalidArguments(
                    "only the lead assigns tasks; `claim` takes one for you".to_owned(),
                ));
            }
            (other, _) => {
                return Err(ToolError::InvalidArguments(format!(
                    "unknown action `{other}`; use add, list, claim, done, release or assign"
                )));
            }
        };
        Ok(ToolOutput::new(said))
    }
}

#[cfg(test)]
/// Tests for the task list a crew shares.
mod tests {
    use super::*;

    /// Two members claiming one task: the first wins and the second is told who holds it.
    #[test]
    fn one_claim_wins() {
        let board = Board::default();
        let id = board.add("build", None, &[]).unwrap();
        board.claim(&id, "a").unwrap();
        let lost = board.claim(&id, "b");
        assert!(matches!(lost, Err(ToolError::Failed(text)) if text.contains("claimed by a")));
        board.claim(&id, "a").unwrap();
    }

    /// Claims racing from many threads leave exactly one winner.
    #[test]
    fn racing_claims_have_one_winner() {
        let board = std::sync::Arc::new(Board::default());
        let id = board.add("build", None, &[]).unwrap();
        let racers: Vec<_> = (0..8)
            .map(|n| {
                let (board, id) = (board.clone(), id.clone());
                std::thread::spawn(move || board.claim(&id, &format!("m{n}")).is_ok())
            })
            .collect();
        let won = racers
            .into_iter()
            .map(|racer| racer.join().unwrap())
            .filter(|won| *won)
            .count();
        assert_eq!(won, 1);
    }

    /// A task waiting on another cannot be claimed or assigned until that one is done.
    #[test]
    fn dependencies_gate_claims() {
        let board = Board::default();
        let first = board.add("schema", None, &[]).unwrap();
        let second = board
            .add("api", Some("on the schema"), std::slice::from_ref(&first))
            .unwrap();
        assert!(board.list().contains("[waiting on t1] api (after t1)"));
        let early = board.claim(&second, "b");
        assert!(matches!(early, Err(ToolError::Failed(text)) if text.contains("waits on t1")));
        assert!(board.assign(&second, "b").is_err());
        board.claim(&first, "a").unwrap();
        board.finish(&first, Some("a"), "done\nmore").unwrap();
        board.claim(&second, "b").unwrap();
        assert!(board.list().contains("- t1 [done by a: done] schema"));
        let missing = board.add("x", None, &["t9".to_owned()]);
        assert!(missing.is_err());
    }

    /// Only the holder or the lead finishes or releases a task, a done task stays done, and releasing all of a member's tasks opens them again.
    #[test]
    fn done_and_release() {
        let board = Board::default();
        let id = board.add("tests", None, &[]).unwrap();
        assert!(board.finish(&id, Some("a"), "ok").is_err());
        assert!(board.release(&id, None).is_err());
        board.claim(&id, "a").unwrap();
        assert!(board.release(&id, Some("b")).is_err());
        board.release(&id, Some("a")).unwrap();
        board.claim(&id, "b").unwrap();
        board.release_all("b");
        assert!(board.list().contains("[open] tests"));
        assert_eq!(board.assign(&id, "a").unwrap(), None);
        assert_eq!(board.assign(&id, "c").unwrap(), Some("a".to_owned()));
        assert!(board.finish(&id, None, "  ").is_err());
        board.finish(&id, None, "all pass").unwrap();
        assert!(board.claim(&id, "a").is_err());
        assert!(board.release(&id, None).is_err());
    }
}
