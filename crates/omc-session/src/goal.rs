//! A goal the agent keeps working toward across turns: the reader sets it with `/goal`, a reminder of it opens every turn, and after each turn that ends with it open crab starts the next one itself, until the agent says it is done, two turns in a row make no progress, or its token budget runs out.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use omc_providers::{ChatModel, Message, ProviderError, Reply, Request, Sink, Tokens};
use omc_tools::{Tool, ToolError, ToolOutput};
use omc_wire::{Goal, GoalState};
use serde_json::{Value, json};

use crate::shared::Shared;

/// The fact a conversation's goal is saved under.
pub(crate) const GOAL_FACT: &str = "goal";

/// The command that sets, shows and changes the goal.
pub(crate) const GOAL_COMMAND: &str = "goal";

/// The command that has the agent ask the reader what the goal should be before it is set.
pub(crate) const GUIDED_COMMAND: &str = "guided-goal";

/// The name of the tool the agent ends the goal with.
pub(crate) const GOAL_TOOL: &str = "goal";

/// Why a goal paused when two turns crab started in a row did the same thing, or nothing.
pub(crate) const NO_PROGRESS: &str = "no progress: the last turn made no tool calls, or the same calls with the same results as the turn before it";

/// Why a goal paused when the reader stopped a turn.
pub(crate) const STOPPED: &str = "the reader stopped the turn";

/// Why a goal that was active when the conversation was opened again is paused.
const REOPENED: &str = "the conversation was opened again; /goal resume goes on";

/// Whether the agent was told its budget is used up.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum WrapUp {
    /// The budget is not used up.
    #[default]
    NotDue,
    /// The budget is used up and the agent has not been told yet.
    Due,
    /// The agent was told.
    Told,
}

/// What crab does once a turn ended with no prompt of the reader's waiting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Next {
    /// Nothing: there is no goal, or none crab works toward.
    Rest,
    /// Starts a turn of its own with this prompt.
    Continue(String),
    /// Stops working toward the goal, which now stands as this.
    Halt(Goal),
}

/// The goal of a conversation, when it has one, and what crab keeps about the turns that work toward it.
#[derive(Default)]
struct Held {
    /// The goal.
    goal: Option<Goal>,
    /// Whether the agent was told its budget is used up.
    wrap_up: WrapUp,
    /// Whether the turn running, or the last one, was started by crab rather than the reader.
    continuing: bool,
    /// What the tool calls of the last turn crab started came to, when it made any.
    last: Option<u64>,
    /// Whether the reader asked the agent to set the goal with them, so that the agent may create it.
    interviewing: bool,
}

/// The goal of a conversation, shared by its commands, its turns and the `goal` tool.
#[derive(Default)]
pub(crate) struct Goals {
    /// The goal and what is kept about its turns.
    held: Mutex<Held>,
}

impl Goals {
    /// Runs `change` on what is held.
    fn with<T>(&self, change: impl FnOnce(&mut Held) -> T) -> T {
        change(&mut self.held.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// The goal, when there is one.
    pub(crate) fn current(&self) -> Option<Goal> {
        self.with(|held| held.goal.clone())
    }

    /// Takes back the goal a saved conversation held, paused when it was active, since nobody asked crab to go on with it in this run; nothing for a conversation without one.
    pub(crate) fn restore(&self, goal: Option<Goal>) -> Option<Goal> {
        self.with(|held| {
            *held = Held::default();
            held.goal = goal
                .filter(|goal| goal.state != GoalState::Cleared)
                .map(|mut goal| {
                    if goal.state == GoalState::Active {
                        goal.state = GoalState::Paused;
                        goal.note = Some(REOPENED.to_owned());
                    }
                    goal
                });
            held.goal.clone()
        })
    }

    /// Sets `objective` as the goal, active, with `budget` when one is given, in place of any goal there was.
    pub(crate) fn set(&self, objective: &str, budget: Option<u64>) -> Goal {
        self.with(|held| {
            let goal = Goal {
                objective: objective.to_owned(),
                state: GoalState::Active,
                budget,
                used: 0,
                note: None,
            };
            *held = Held {
                goal: Some(goal.clone()),
                ..Held::default()
            };
            goal
        })
    }

    /// Lets the agent create the goal, since the reader asked it to work the goal out with them; a goal that was active pauses meanwhile, so no turn toward it starts while they talk. The goal as it now stands when it paused.
    pub(crate) fn begin_interview(&self) -> Option<Goal> {
        self.with(|held| {
            held.interviewing = true;
            held.continuing = false;
            held.last = None;
            let goal = held
                .goal
                .as_mut()
                .filter(|goal| goal.state == GoalState::Active)?;
            goal.state = GoalState::Paused;
            goal.note = Some("a new goal is being worked out with the reader".to_owned());
            Some(goal.clone())
        })
    }

    /// Sets `objective` as the goal the agent worked out with the reader, with `budget` when one is given, in place of any goal there was.
    ///
    /// # Errors
    ///
    /// Says so when the reader did not ask the agent to set the goal with `/guided-goal`.
    pub(crate) fn create(&self, objective: &str, budget: Option<u64>) -> Result<Goal, String> {
        if !self.with(|held| held.interviewing) {
            return Err(format!(
                "only the reader sets a goal, with /goal, unless they asked you to work it out with them through /{GUIDED_COMMAND}"
            ));
        }
        Ok(self.set(objective, budget))
    }

    /// Pauses the goal for `note`, when it is active; the goal as it now stands, or nothing when it was not active.
    pub(crate) fn pause(&self, note: &str) -> Option<Goal> {
        self.with(|held| {
            held.continuing = false;
            let goal = held
                .goal
                .as_mut()
                .filter(|goal| goal.state == GoalState::Active)?;
            goal.state = GoalState::Paused;
            goal.note = Some(note.to_owned());
            Some(goal.clone())
        })
    }

    /// Makes the paused goal active again, forgetting what earlier turns did.
    ///
    /// # Errors
    ///
    /// Says why when there is no goal, it is not paused, or it has no budget left.
    pub(crate) fn resume(&self) -> Result<Goal, String> {
        self.with(|held| {
            let goal = held.goal.as_mut().ok_or("no goal is set")?;
            match goal.state {
                GoalState::Paused => {}
                GoalState::Active => return Err("the goal is active already".to_owned()),
                GoalState::BudgetLimited => {
                    return Err(
                        "the goal used up its budget; raise it with /goal budget <tokens> first"
                            .to_owned(),
                    );
                }
                GoalState::Complete | GoalState::Cleared => {
                    return Err(
                        "the goal is complete; set a new one with /goal <objective>".to_owned()
                    );
                }
            }
            if goal.budget.is_some_and(|budget| goal.used >= budget) {
                return Err(
                    "the goal used up its budget; raise it with /goal budget <tokens> first"
                        .to_owned(),
                );
            }
            goal.state = GoalState::Active;
            goal.note = None;
            let goal = goal.clone();
            held.wrap_up = WrapUp::NotDue;
            held.continuing = false;
            held.last = None;
            Ok(goal)
        })
    }

    /// Forgets the goal; it as cleared, or nothing when there was none.
    pub(crate) fn clear(&self) -> Option<Goal> {
        self.with(|held| {
            let mut goal = held.goal.take()?;
            *held = Held::default();
            goal.state = GoalState::Cleared;
            goal.note = None;
            Some(goal)
        })
    }

    /// Gives the goal the budget `budget`, or none; a goal that had used up its budget and now has some left is paused, for `/goal resume` to go on with it.
    ///
    /// # Errors
    ///
    /// Says so when there is no goal, or it is complete.
    pub(crate) fn rebudget(&self, budget: Option<u64>) -> Result<Goal, String> {
        self.with(|held| {
            let goal = held.goal.as_mut().ok_or("no goal is set")?;
            if goal.state == GoalState::Complete {
                return Err("the goal is complete; set a new one with /goal <objective>".to_owned());
            }
            goal.budget = budget;
            let left = budget.is_none_or(|budget| goal.used < budget);
            if goal.state == GoalState::BudgetLimited && left {
                goal.state = GoalState::Paused;
                goal.note = Some("the budget was raised; /goal resume goes on".to_owned());
            }
            let goal = goal.clone();
            if left {
                held.wrap_up = WrapUp::NotDue;
            }
            Ok(goal)
        })
    }

    /// Counts what a request cost toward the goal, when crab works toward one: input, output and cache writes, since cache reads cost little; the goal as it now stands when that used up its budget.
    pub(crate) fn meter(&self, tokens: &Tokens) -> Option<Goal> {
        self.with(|held| {
            let goal = held.goal.as_mut().filter(|goal| {
                matches!(goal.state, GoalState::Active | GoalState::BudgetLimited)
            })?;
            goal.used += tokens.input + tokens.output + tokens.cache_write;
            let spent = goal.budget.is_some_and(|budget| goal.used >= budget);
            if !spent || goal.state != GoalState::Active {
                return None;
            }
            goal.state = GoalState::BudgetLimited;
            goal.note = Some("the token budget is used up".to_owned());
            held.wrap_up = WrapUp::Due;
            Some(goal.clone())
        })
    }

    /// The message that tells the agent to wrap up, the first time it is asked for once the budget is used up; nothing otherwise.
    pub(crate) fn take_wrap_up(&self) -> Option<String> {
        self.with(|held| {
            if held.wrap_up != WrapUp::Due {
                return None;
            }
            held.wrap_up = WrapUp::Told;
            held.goal.as_ref().map(wrap_up)
        })
    }

    /// The reminder of the goal a turn's prompt starts with, while crab works toward it.
    pub(crate) fn reminder(&self) -> Option<String> {
        self.with(|held| {
            held.goal
                .as_ref()
                .filter(|goal| matches!(goal.state, GoalState::Active | GoalState::BudgetLimited))
                .map(reminder)
        })
    }

    /// Ends the goal as achieved, with the agent's `summary`.
    ///
    /// # Errors
    ///
    /// Says why when there is no goal crab works toward.
    pub(crate) fn complete(&self, summary: &str) -> Result<Goal, String> {
        self.with(|held| {
            let goal = held.goal.as_mut().ok_or("no goal is set")?;
            if !matches!(goal.state, GoalState::Active | GoalState::BudgetLimited) {
                return Err(format!(
                    "the goal is not being worked toward; it is {}",
                    state_name(goal.state)
                ));
            }
            goal.state = GoalState::Complete;
            goal.note = Some(summary.to_owned());
            let goal = goal.clone();
            held.continuing = false;
            held.last = None;
            Ok(goal)
        })
    }

    /// Notes that the reader spoke, so that the turns crab starts from now on are judged afresh.
    pub(crate) fn reader_spoke(&self) {
        self.with(|held| {
            held.continuing = false;
            held.last = None;
        });
    }

    /// What crab does after a turn whose tool calls and results came to `digest`, `None` when it made none, ended with no prompt of the reader's waiting.
    ///
    /// While the goal is active crab starts another turn, unless the turn was one crab started and made no tool calls, or the same calls with the same results as the turn crab started before it: then the goal pauses. Once the budget is used up, a turn is started to tell the agent to wrap up when it was not told during the turn, and then nothing more.
    pub(crate) fn after_turn(&self, digest: Option<u64>) -> Next {
        self.with(|held| {
            let Some(goal) = held.goal.as_mut() else {
                return Next::Rest;
            };
            match goal.state {
                GoalState::BudgetLimited if held.wrap_up == WrapUp::Due => {
                    held.wrap_up = WrapUp::Told;
                    held.continuing = true;
                    Next::Continue(wrap_up(goal))
                }
                GoalState::Active => {
                    let stalled = held.continuing && (digest.is_none() || digest == held.last);
                    if stalled {
                        goal.state = GoalState::Paused;
                        goal.note = Some(NO_PROGRESS.to_owned());
                        held.continuing = false;
                        held.last = None;
                        return Next::Halt(goal.clone());
                    }
                    held.last = if held.continuing { digest } else { None };
                    held.continuing = true;
                    Next::Continue(continuation(goal))
                }
                _ => {
                    held.continuing = false;
                    Next::Rest
                }
            }
        })
    }
}

/// The name a goal's state goes by in what the reader and the agent are told.
fn state_name(state: GoalState) -> &'static str {
    match state {
        GoalState::Active => "active",
        GoalState::Paused => "paused",
        GoalState::BudgetLimited => "out of budget",
        GoalState::Complete => "complete",
        GoalState::Cleared => "cleared",
    }
}

/// How much of its budget `goal` used, in words.
fn usage(goal: &Goal) -> String {
    match goal.budget {
        Some(budget) => format!("{} of {} tokens used", goal.used, budget),
        None => format!("{} tokens used, no budget", goal.used),
    }
}

/// A few words on `goal`: its objective, how it stands, its budget and why it stands so, for the reader.
pub(crate) fn described(goal: &Goal) -> String {
    let mut said = format!(
        "Goal: {}\nState: {}\n{}.",
        goal.objective,
        state_name(goal.state),
        usage(goal)
    );
    if let Some(note) = &goal.note {
        said.push_str(&format!("\nNote: {note}"));
    }
    said
}

/// The reminder of `goal` a turn's prompt starts with.
fn reminder(goal: &Goal) -> String {
    format!(
        "<goal>\nGoal mode is on. The objective: {}\n{}. Keep working until the objective is fully achieved, never a smaller version of it. Once you have checked with real evidence that it is achieved, call the `goal` tool with `complete` and a summary of that evidence; nothing else ends goal mode.\n</goal>",
        goal.objective,
        usage(goal)
    )
}

/// An id for a turn crab starts itself, unlike any other: the time to the nanosecond, after `goal-`.
pub(crate) fn turn_id() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("goal-{now}")
}

/// The prompt of the turn in which the agent works out the goal with the reader, starting from their `idea`.
pub(crate) fn interview(idea: &str) -> String {
    let idea = if idea.is_empty() {
        "none given yet; start by asking what they want done"
    } else {
        idea
    };
    format!(
        "Help me set a goal for you to work toward across turns. My starting idea: {idea}\n\nDo not start the work yet and change no files. First pin down five things with me:\n1. What done means, as checks that either pass or fail.\n2. The command or steps that verify it.\n3. How many attempts, or how much effort, is reasonable before stopping.\n4. What is in scope, and what must not be touched.\n5. When to stop and ask me instead of going on.\n\nRead the project first where that lets you ask better questions. Ask with the `ask` tool, at most six questions in all, each with the answer you recommend, and skip what my idea already settles. Once all five are settled, call the `goal` tool with `create`, a `budget` in tokens only when I gave one, and an `objective` laid out as:\n\n## Objective\n## Success criteria\n## Verification\n## Boundaries\n## Stop conditions\n\nThen end the turn; goal mode starts the work from the next one."
    )
}

/// The prompt crab starts the first turn toward `objective` with.
pub(crate) fn opening(objective: &str) -> String {
    format!(
        "Work toward this goal until it is fully achieved: {objective}\n\nStart by saying what done means for it and how you will check it, then work. When it is achieved and you have checked it, call the `goal` tool with `complete` and a summary of the evidence."
    )
}

/// The prompt of a turn crab starts itself to keep working toward `goal`.
pub(crate) fn continuation(goal: &Goal) -> String {
    format!(
        "Keep working toward the goal: {}\n\nBefore anything else, audit where it stands:\n1. Say what done means for the objective, concretely.\n2. Check the current state against it with real evidence: run the tests, builds or commands that show it, rather than trusting what earlier turns said.\n3. List what is still missing or failing.\n4. When nothing is missing and you checked it in this turn, call `goal` with `complete` and a summary of the evidence.\n5. Otherwise take the next concrete step and carry it out; do not stop to ask whether to go on.\n6. When you are blocked on something only the reader can give, say exactly what and end the turn without calling tools; goal mode then pauses.\n\n{}.",
        goal.objective,
        usage(goal)
    )
}

/// The message that tells the agent the budget of `goal` is used up.
fn wrap_up(goal: &Goal) -> String {
    format!(
        "The goal's token budget is used up ({}). Start no new work. Finish the step you are on only when it is nearly done, leave the work in a coherent state, and report what is done, what is not, and what should come next. Call `goal` with `complete` only when the objective is truly achieved; running out of budget is not completing it.",
        usage(goal)
    )
}

/// What the tool calls and their results among `messages` come to, so that two turns that did the same come to the same; nothing when there are none.
pub(crate) fn digest(messages: &[Message]) -> Option<u64> {
    let mut hasher = DefaultHasher::new();
    let mut any = false;
    for message in messages {
        match message {
            Message::Assistant(reply) => {
                for call in &reply.tool_calls {
                    any = true;
                    call.name.hash(&mut hasher);
                    call.arguments.to_string().hash(&mut hasher);
                }
            }
            Message::ToolResults(results) => {
                for result in results {
                    result.name.hash(&mut hasher);
                    result.text.hash(&mut hasher);
                    result.is_error.hash(&mut hasher);
                }
            }
            _ => {}
        }
    }
    any.then(|| hasher.finish())
}

/// The messages a turn added to a history that held `before` messages when it started and holds `after` now; when a compaction made it shorter meanwhile, those after the last compaction or pruning.
pub(crate) fn added(before: usize, after: &[Message]) -> &[Message] {
    if after.len() >= before {
        return &after[before..];
    }
    let start = after
        .iter()
        .rposition(|message| matches!(message, Message::Compaction(_) | Message::Pruning(_)))
        .map_or(0, |at| at + 1);
    &after[start..]
}

/// The count of tokens `text` gives: digits, with `k` for thousands or `m` for millions after them; a bare number counts only from a thousand, so that a sentence starting with a small number is not read as a budget.
pub(crate) fn tokens(text: &str) -> Option<u64> {
    let lower = text.to_ascii_lowercase();
    let (digits, scale) = match lower.strip_suffix('k') {
        Some(digits) => (digits, 1_000.0),
        None => match lower.strip_suffix('m') {
            Some(digits) => (digits, 1_000_000.0),
            None => (lower.as_str(), 1.0),
        },
    };
    let digits = digits.replace('_', "");
    if digits.is_empty() || !digits.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let count = (digits.parse::<f64>().ok()? * scale).round();
    (count >= 1_000.0 && count.is_finite()).then_some(count as u64)
}

/// What the reader asked of the goal with `/goal`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Asked {
    /// Show it.
    Show,
    /// Have the agent ask the reader what the goal should be, starting from this idea, and then set it.
    Guide(String),
    /// Set this objective, with this budget when one was given.
    Set(String, Option<u64>),
    /// Pause it.
    Pause,
    /// Go on with it.
    Resume,
    /// Forget it.
    Clear,
    /// Give it this budget, or none.
    Budget(Option<u64>),
}

/// What `text` asks of the goal, when it is a `/goal` command.
///
/// # Errors
///
/// Says why when it is a `/goal budget` whose count cannot be read.
pub(crate) fn request(text: &str) -> Option<Result<Asked, String>> {
    let rest = text.trim().strip_prefix('/')?;
    let (name, rest) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
    let rest = rest.trim();
    if name == GUIDED_COMMAND {
        return Some(Ok(Asked::Guide(rest.to_owned())));
    }
    if name != GOAL_COMMAND {
        return None;
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    Some(Ok(match words.as_slice() {
        [] => Asked::Show,
        ["pause"] => Asked::Pause,
        ["resume"] => Asked::Resume,
        ["clear"] => Asked::Clear,
        ["budget", "off" | "none"] => Asked::Budget(None),
        ["budget", count] => match tokens(count) {
            Some(count) => Asked::Budget(Some(count)),
            None => {
                return Some(Err(format!(
                    "/goal budget takes a count of tokens such as 500k, 2m or 1200000, or off; not {count}"
                )));
            }
        },
        [first, objective @ ..] if !objective.is_empty() && tokens(first).is_some() => {
            let objective = rest[first.len()..].trim().to_owned();
            Asked::Set(objective, tokens(first))
        }
        _ => Asked::Set(rest.to_owned(), None),
    }))
}

/// The model a turn asks while a conversation may have a goal: it passes every request on and counts what each cost toward the goal.
pub(crate) struct Metered {
    /// The model requests go to.
    pub(crate) inner: Arc<dyn ChatModel>,
    /// The conversation, whose goal the cost is counted toward and whose client is told when the budget runs out.
    pub(crate) shared: Arc<Shared>,
}

impl Metered {
    /// Counts what `reply` cost toward the goal, telling the client when that used up its budget.
    fn count(&self, reply: &Reply) {
        if let Some(goal) = reply
            .tokens
            .and_then(|tokens| self.shared.goals.meter(&tokens))
        {
            self.shared.announce_goal(&goal);
        }
    }
}

#[async_trait]
impl ChatModel for Metered {
    /// Sends `request` and counts what it cost.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let reply = self.inner.complete(request, sink).await?;
        self.count(&reply);
        Ok(reply)
    }

    /// Sends `request` without writing to the cache, and counts what it cost.
    async fn complete_uncached(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let reply = self.inner.complete_uncached(request, sink).await?;
        self.count(&reply);
        Ok(reply)
    }

    /// Warms `request` with the wrapped model; keeping the cache warm between turns is not work toward the goal, so it is not counted.
    async fn warm(&self, request: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
        self.inner.warm(request).await
    }
}

/// The tool the agent ends the goal with, or reads it with.
pub(crate) struct GoalTool {
    /// The conversation whose goal it is.
    pub(crate) shared: Arc<Shared>,
}

/// What the `goal` tool says it does.
const GOAL_DESCRIPTION: &str = "Read, create or end the goal the reader set with /goal. While goal mode is on, crab keeps starting turns toward the goal; only `complete` ends it.\n- `status`: the objective, how it stands and how much of its token budget is used.\n- `create` with `objective`, and `budget` when the reader gave one: sets the goal you worked out with the reader; only after they asked for it with /guided-goal.\n- `complete` with `summary`: the objective is fully achieved and you checked it with real evidence in this conversation, such as tests or builds you ran; the summary gives that evidence. Never call it to stop early, for a smaller version of the objective, or because the budget ran out.";

#[async_trait]
impl Tool for GoalTool {
    /// The tool's name.
    fn name(&self) -> &'static str {
        GOAL_TOOL
    }

    /// What the tool does.
    fn description(&self) -> &str {
        GOAL_DESCRIPTION
    }

    /// The tool takes an operation and, to complete the goal, a summary.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "op": { "type": "string", "enum": ["status", "create", "complete"] },
                "objective": { "type": "string", "description": "For `create`: the goal, laid out with Objective, Success criteria, Verification, Boundaries and Stop conditions." },
                "budget": { "type": "integer", "description": "For `create`: the most tokens the goal may use, when the reader gave a budget." },
                "summary": { "type": "string", "description": "For `complete`: what was achieved and the evidence that it was." },
            },
            "required": ["op"],
        })
    }

    /// Reading or ending the goal changes no file.
    fn read_only(&self) -> bool {
        true
    }

    /// The call in a few words.
    fn describe(&self, args: &Value) -> String {
        match args["op"].as_str() {
            Some("complete") => "complete the goal".to_owned(),
            Some("create") => "set the goal".to_owned(),
            _ => "read the goal".to_owned(),
        }
    }

    /// Reads the goal, or ends it as achieved and tells the client.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when there is no goal, it is not being worked toward, the operation is unknown, or `complete` comes without a summary.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let goals = &self.shared.goals;
        match args["op"].as_str() {
            Some("status") => {
                let goal = goals.current().ok_or_else(|| {
                    ToolError::Failed("no goal is set; the reader sets one with /goal".to_owned())
                })?;
                Ok(ToolOutput::new(described(&goal)))
            }
            Some("create") => {
                let objective = args["objective"]
                    .as_str()
                    .map(str::trim)
                    .filter(|objective| !objective.is_empty())
                    .ok_or_else(|| ToolError::Failed("`create` needs an `objective`".to_owned()))?;
                let goal = goals
                    .create(objective, args["budget"].as_u64())
                    .map_err(ToolError::Failed)?;
                self.shared.announce_goal(&goal);
                Ok(ToolOutput::new(format!(
                    "The goal is set and goal mode is on. {}. End this turn now; the work starts in the next one.",
                    usage(&goal)
                )))
            }
            Some("complete") => {
                let summary = args["summary"]
                    .as_str()
                    .map(str::trim)
                    .filter(|summary| !summary.is_empty())
                    .ok_or_else(|| {
                        ToolError::Failed(
                            "`complete` needs a `summary` of what was achieved and the evidence"
                                .to_owned(),
                        )
                    })?;
                let goal = goals.complete(summary).map_err(ToolError::Failed)?;
                self.shared.announce_goal(&goal);
                Ok(ToolOutput::new(format!(
                    "The goal is complete and goal mode is off. {}. Finish this turn by telling the reader what was achieved.",
                    usage(&goal)
                )))
            }
            other => Err(ToolError::Failed(format!(
                "`op` is status, create or complete, not {}",
                other.unwrap_or("missing")
            ))),
        }
    }
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
