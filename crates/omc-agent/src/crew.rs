//! Long-lived subagents: named subagents the agent starts with `agent_spawn`, talks to with `agent_send`, hears from with `agent_wait`, looks over with `agent_list` and ends with `agent_kill`.
//!
//! Each runs in a background worker of its own that keeps its conversation between messages, so it lives across turns until it is killed, the reader stops it, or the conversation ends and its [`Crew`] is dropped. Its replies queue up in the order of the messages it was sent.
//!
//! The members of a crew talk to each other with `crew_message`, see who is on it with `crew_members`, and share a task list with the lead through `crew_tasks` (see [`crate::board`]). A message reaches a member that works at its next step boundary, and wakes one that is idle, as `agent_send` does.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use omc_tools::{Concurrency, Tool, ToolError, ToolOutput, parse_args};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::{
    UnboundedReceiver, UnboundedSender, WeakUnboundedSender, unbounded_channel,
};
use tokio::task::JoinHandle;

use crate::board::{Board, CrewTasks};
use crate::mailbox::{self, Mailbox};
use crate::nesting::Throttle;
use crate::roster::{Enrolment, SubagentState};
use crate::task::{EventSink, Run, Task};

/// How long `agent_wait` waits by default, in seconds.
const WAIT_DEFAULT: f64 = 300.0;

/// The longest `agent_wait` waits, in seconds.
const WAIT_MOST: f64 = 1800.0;

/// The most long-lived subagents alive at once.
pub const CREW_MOST: usize = 16;

/// The longest name a long-lived subagent may have.
const NAME_MOST: usize = 40;

/// The most messages a member may send with `crew_message` in one run of its work, from one message it was sent to its reply.
pub(crate) const MESSAGES_PER_RUN: usize = 20;

/// The longest message `crew_message` sends, in characters.
const MESSAGE_MOST: usize = 4000;

/// The name members call the agent that started them by.
const LEAD: &str = "lead";

/// The name members call every other member by at once.
const ALL: &str = "all";

/// A message for a long-lived subagent.
struct Letter {
    /// The id of the call that sent it, which its work is reported under.
    call: String,
    /// What it is told.
    text: String,
    /// Where its work on the message is reported.
    events: Arc<EventSink>,
}

/// The replies of a long-lived subagent, in order, each its last words or why it failed.
type Replies = Arc<tokio::sync::Mutex<UnboundedReceiver<Result<String, String>>>>;

/// One long-lived subagent.
struct Member {
    /// Its kind.
    kind: String,
    /// What it was started to do, in a few words.
    label: String,
    /// Whether every tool it has only looks.
    looks: bool,
    /// Where messages for it go.
    letters: UnboundedSender<Letter>,
    /// Its replies, in order, each its last words or why it failed.
    replies: Replies,
    /// Where its messages to the lead are queued among its replies; it does not keep the queue open once its worker ended.
    outbox: WeakUnboundedSender<Result<String, String>>,
    /// Where the messages other members send it wait.
    mailbox: Arc<Mailbox>,
    /// How many replies it owes: messages sent whose replies were not taken yet.
    owed: Arc<AtomicUsize>,
    /// Whether it is working on a message now.
    busy: Arc<AtomicBool>,
    /// Its worker.
    worker: JoinHandle<()>,
}

impl Member {
    /// Whether its worker still runs: it was not stopped by the reader or by a failure of its own.
    fn alive(&self) -> bool {
        !self.worker.is_finished()
    }

    /// How it stands, in a word.
    fn state(&self) -> &'static str {
        if !self.alive() {
            "stopped"
        } else if self.busy.load(Ordering::SeqCst) {
            "working"
        } else {
            "idle"
        }
    }
}

/// The long-lived subagents of a conversation, by name, and the task list they share with the lead. Dropping it stops them all.
#[derive(Default)]
pub struct Crew {
    /// The subagents.
    members: Mutex<BTreeMap<String, Member>>,
    /// The task list.
    board: Board,
}

impl Crew {
    /// The members, locked; a poisoned lock is taken over, since every change to it is whole.
    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, Member>> {
        self.members
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// How many long-lived subagents are alive.
    pub fn alive(&self) -> usize {
        self.lock().values().filter(|member| member.alive()).count()
    }

    /// Stops every long-lived subagent, forgets them and empties the task list.
    pub fn dismiss(&self) {
        for (_, member) in std::mem::take(&mut *self.lock()) {
            member.worker.abort();
        }
        self.board.clear();
    }

    /// The task list the crew shares with the lead.
    pub(crate) fn board(&self) -> &Board {
        &self.board
    }

    /// Whether a long-lived subagent called `name` is on the crew, alive or not.
    pub(crate) fn is_member(&self, name: &str) -> bool {
        self.lock().contains_key(name)
    }

    /// The replies of the member `name`, how many it owes, and whether it is alive.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when no long-lived subagent has that name.
    fn queue(&self, name: &str) -> Result<(Replies, Arc<AtomicUsize>, bool), ToolError> {
        let members = self.lock();
        let member = members.get(name).ok_or_else(|| unknown(name, &members))?;
        Ok((member.replies.clone(), member.owed.clone(), member.alive()))
    }

    /// Delivers `text` from the member `from` to `to`: another member by name, whose mailbox takes it as a notice naming the sender, every other live member for `all`, or the lead for `lead`, among whose replies from `from` it is queued for `agent_wait`. Says who it reached.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when `from` is no member, no member is called `to`, the one called so was stopped, or `all` reaches nobody.
    pub(crate) fn post(&self, from: &str, to: &str, text: &str) -> Result<String, ToolError> {
        let members = self.lock();
        match to {
            LEAD => {
                let sender = members
                    .get(from)
                    .ok_or_else(|| ToolError::Failed(format!("{from} is no longer on the crew")))?;
                let outbox = sender
                    .outbox
                    .upgrade()
                    .ok_or_else(|| ToolError::Failed("the lead no longer hears you".to_owned()))?;
                sender.owed.fetch_add(1, Ordering::SeqCst);
                if outbox
                    .send(Ok(format!("Message from {from}:\n{}", text.trim())))
                    .is_err()
                {
                    sender.owed.fetch_sub(1, Ordering::SeqCst);
                    return Err(ToolError::Failed("the lead no longer hears you".to_owned()));
                }
                Ok("Sent to the lead, which reads it with your replies.".to_owned())
            }
            ALL => {
                let reached: Vec<&str> = members
                    .iter()
                    .filter(|(name, member)| name.as_str() != from && member.alive())
                    .map(|(name, member)| {
                        member.mailbox.post(mailbox::notice(from, text));
                        name.as_str()
                    })
                    .collect();
                if reached.is_empty() {
                    return Err(ToolError::Failed(
                        "no other member of the crew is alive; send it to the lead instead"
                            .to_owned(),
                    ));
                }
                Ok(format!("Sent to {}.", reached.join(", ")))
            }
            name => {
                let member = members.get(name).ok_or_else(|| unknown(name, &members))?;
                if !member.alive() {
                    return Err(ToolError::Failed(format!(
                        "{name} was stopped and gets no messages"
                    )));
                }
                member.mailbox.post(mailbox::notice(from, text));
                let when = if member.busy.load(Ordering::SeqCst) {
                    "it is working and reads it after its current step"
                } else {
                    "it was idle and starts on it now"
                };
                Ok(format!("Sent to {name}; {when}."))
            }
        }
    }
}

impl Drop for Crew {
    /// Stops every long-lived subagent left.
    fn drop(&mut self) {
        self.dismiss();
    }
}

/// The failure for `name`, which names no long-lived subagent among `members`.
fn unknown(name: &str, members: &BTreeMap<String, Member>) -> ToolError {
    let known: Vec<&str> = members.keys().map(String::as_str).collect();
    let listed = if known.is_empty() {
        "there are none; start one with agent_spawn".to_owned()
    } else {
        format!("there are: {}", known.join(", "))
    };
    ToolError::Failed(format!("no long-lived subagent is called {name}; {listed}"))
}

/// `name` trimmed, when it is a name a long-lived subagent may have: letters, digits, `-` and `_`, at most [`NAME_MOST`] of them, and neither `lead` nor `all`, which members address the lead and the whole crew by.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] for any other name.
fn checked_name(name: &str) -> Result<String, ToolError> {
    let name = name.trim();
    let fits = !name.is_empty()
        && name.chars().count() <= NAME_MOST
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if [LEAD, ALL].contains(&name) {
        return Err(ToolError::InvalidArguments(format!(
            "`{name}` is reserved; pick another name"
        )));
    }
    if fits {
        Ok(name.to_owned())
    } else {
        Err(ToolError::InvalidArguments(format!(
            "`name` must be 1 to {NAME_MOST} letters, digits, `-` or `_`"
        )))
    }
}

/// What the worker of a long-lived subagent keeps besides its conversation: how it says it works, where the messages of its crewmates wait, the count of replies it owes, the count of messages it sent this run, and the call and event sink its work on a crewmate's message is reported under.
struct Desk {
    /// Marked while it works.
    busy: Arc<AtomicBool>,
    /// The messages of its crewmates.
    mailbox: Arc<Mailbox>,
    /// How many replies it owes.
    owed: Arc<AtomicUsize>,
    /// How many messages it sent with `crew_message` in its current run.
    sent: Arc<AtomicUsize>,
    /// The `agent_spawn` call that started it.
    call: String,
    /// Where the work of that call is reported.
    events: Arc<EventSink>,
}

impl Desk {
    /// The letter made of every message waiting in the mailbox, which the lead is then owed a reply to, reported under the call that started the subagent; nothing when none waits.
    fn mail(&self) -> Option<Letter> {
        let notes = self.mailbox.take_all();
        if notes.is_empty() {
            return None;
        }
        self.owed.fetch_add(1, Ordering::SeqCst);
        Some(Letter {
            call: self.call.clone(),
            text: notes.join("\n\n"),
            events: self.events.clone(),
        })
    }
}

/// Answers the letters `run` is sent, and the messages its crewmates leave at `desk` while it is idle, one after another, sending each reply to `replies`, until its letters run out or the reader stops it through `enrolment`; its turns take seats of `throttle`. Marks it busy while it works, and starts the count of messages it sent anew with each run.
async fn serve(
    mut run: Run,
    mut letters: UnboundedReceiver<Letter>,
    replies: UnboundedSender<Result<String, String>>,
    desk: Desk,
    throttle: Arc<Throttle>,
    mut enrolment: Enrolment,
) {
    let kind = run.kind().to_owned();
    let stopped = || {
        Err(format!(
            "the {kind} agent was stopped by the reader. Do not start it again unless the reader asks you to."
        ))
    };
    loop {
        let letter = match desk.mail() {
            Some(letter) => Some(letter),
            None => tokio::select! {
                biased;
                () = enrolment.stopped() => break,
                letter = letters.recv() => letter,
                () = desk.mailbox.rung() => continue,
            },
        };
        let Some(letter) = letter else {
            enrolment.end(SubagentState::Finished);
            return;
        };
        run.address(&letter.call, letter.text, letter.events.clone());
        (letter.events)(run.opened());
        desk.sent.store(0, Ordering::SeqCst);
        desk.busy.store(true, Ordering::SeqCst);
        let outcome = tokio::select! {
            biased;
            () = enrolment.stopped() => None,
            outcome = run.answer(&throttle) => Some(outcome),
        };
        desk.busy.store(false, Ordering::SeqCst);
        match outcome {
            Some(outcome) => {
                let _ = replies.send(outcome.map_err(|error| error.to_string()));
            }
            None => {
                let _ = replies.send(stopped());
                break;
            }
        }
    }
    enrolment.end(SubagentState::Stopped);
}

/// Starts a long-lived subagent.
pub struct AgentSpawn {
    /// What starts it.
    task: Arc<Task>,
    /// Where it is kept.
    crew: Arc<Crew>,
}

/// Arguments of an `agent_spawn` call.
#[derive(Deserialize)]
struct SpawnArgs {
    /// Its name.
    name: String,
    /// What it is to do first.
    prompt: String,
    /// Its kind.
    agent: Option<String>,
    /// A few words naming its work.
    description: Option<String>,
    /// The model it asks.
    model: Option<String>,
    /// The work category it is handed to, whose slot decides the model it asks.
    category: Option<String>,
}

#[async_trait]
impl Tool for AgentSpawn {
    /// Returns `agent_spawn`.
    fn name(&self) -> &'static str {
        "agent_spawn"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "Start a long-lived subagent under a name: it works on `prompt` in the background and stays alive with its conversation across turns until agent_kill or the end of the conversation. Get its reply with agent_wait, send it more with agent_send, see all of them with agent_list. Members can message each other and pick work from a task list they share with you (crew_tasks, from your next turn). For a one-off assignment use `task` instead."
    }

    /// Returns the argument schema, which takes `category` only while there are work categories.
    fn schema(&self) -> Value {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The name it is known by, such as `tester`: letters, digits, `-` and `_`." },
                "prompt": { "type": "string", "description": "Its first, self-contained assignment." },
                "agent": { "type": "string", "description": "The kind of subagent, as `task` lists them; omit for the default." },
                "description": { "type": "string", "description": "A few words naming its work, shown to the user." },
                "model": { "type": "string", "description": "The model it asks instead of its kind's or yours." }
            },
            "required": ["name", "prompt"]
        });
        if let Some(category) = crate::task::category_schema(self.task.categories()) {
            schema["properties"]["category"] = category;
        }
        schema
    }

    /// Exclusive, since the subagent may change files.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Runs the call without an id.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Prepares the subagent with the tools of a crew member, puts it on the roster, starts its worker with `prompt` as its first message, its work reported under the call `id`, and returns its name as the handle.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for a bad name or an empty prompt, and [`ToolError::Failed`] when a live subagent has the name already, too many are alive, or the subagent cannot be prepared.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: SpawnArgs = parse_args(args)?;
        let name = checked_name(&args.name)?;
        if args.prompt.trim().is_empty() {
            return Err(ToolError::InvalidArguments("`prompt` is empty".to_owned()));
        }
        {
            let mut members = self.crew.lock();
            members.retain(|_, member| member.alive());
            if members.contains_key(&name) {
                return Err(ToolError::Failed(format!(
                    "a long-lived subagent is called {name} already; send it a message with agent_send, or pick another name"
                )));
            }
            if members.len() >= CREW_MOST {
                return Err(ToolError::Failed(format!(
                    "{CREW_MOST} long-lived subagents are alive; end one with agent_kill first"
                )));
            }
        }
        let mut run = self
            .task
            .enlist(
                id,
                args.agent,
                args.prompt.clone(),
                args.description,
                args.model,
                args.category,
            )
            .await?;
        let mailbox = Arc::new(Mailbox::default());
        let sent = Arc::new(AtomicUsize::new(0));
        self.task.join(
            &mut run,
            member_tools(&self.crew, &name, sent.clone()),
            mailbox.clone(),
        );
        let nesting = self.task.nesting();
        let events = self.task.events();
        let enrolment = nesting.roster.enrol(run.notice(), events.clone());
        let (kind, label, looks) = (run.kind().to_owned(), run.label().to_owned(), run.looks());
        let (letters, inbox) = unbounded_channel();
        let (outbox, replies) = unbounded_channel();
        let busy = Arc::new(AtomicBool::new(false));
        let owed = Arc::new(AtomicUsize::new(1));
        let _ = letters.send(Letter {
            call: id.to_owned(),
            text: args.prompt,
            events: events.clone(),
        });
        let desk = Desk {
            busy: busy.clone(),
            mailbox: mailbox.clone(),
            owed: owed.clone(),
            sent,
            call: id.to_owned(),
            events,
        };
        let weak_outbox = outbox.downgrade();
        let worker = tokio::spawn(serve(
            run,
            inbox,
            outbox,
            desk,
            nesting.throttle.clone(),
            enrolment,
        ));
        let member = Member {
            kind: kind.clone(),
            label,
            looks,
            letters,
            replies: Arc::new(tokio::sync::Mutex::new(replies)),
            outbox: weak_outbox,
            mailbox,
            owed,
            busy,
            worker,
        };
        let mut members = self.crew.lock();
        if let Some(old) = members.insert(name.clone(), member) {
            old.worker.abort();
        }
        Ok(ToolOutput::new(format!(
            "Started the {kind} agent as {name}. Call agent_wait with name \"{name}\" for its reply; agent_send sends it more."
        )))
    }
}

/// Sends a message to a long-lived subagent.
pub struct AgentSend {
    /// What reports its work and says whether it may change files now.
    task: Arc<Task>,
    /// Where it is kept.
    crew: Arc<Crew>,
}

/// Arguments of an `agent_send` call.
#[derive(Deserialize)]
struct SendArgs {
    /// The name of the subagent.
    name: String,
    /// What it is told.
    message: String,
}

#[async_trait]
impl Tool for AgentSend {
    /// Returns `agent_send`.
    fn name(&self) -> &'static str {
        "agent_send"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "Send a follow-up message to a live long-lived subagent; it answers knowing its whole conversation, after any message it is still on. Returns at once; agent_wait gets the reply."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The subagent's name, as agent_spawn gave it." },
                "message": { "type": "string", "description": "What it is told." }
            },
            "required": ["name", "message"]
        })
    }

    /// Exclusive, since the subagent may change files.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Runs the call without an id.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Queues `message` for the subagent, its work on it reported under the call `id`, and says how many replies it owes.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an empty message, and [`ToolError::Failed`] when no live subagent has the name, or it can change files while subagents may only look.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: SendArgs = parse_args(args)?;
        let name = args.name.trim();
        if args.message.trim().is_empty() {
            return Err(ToolError::InvalidArguments("`message` is empty".to_owned()));
        }
        let members = self.crew.lock();
        let member = members.get(name).ok_or_else(|| unknown(name, &members))?;
        if !member.alive() {
            return Err(ToolError::Failed(format!(
                "{name} was stopped; agent_kill forgets it, agent_spawn starts a new one"
            )));
        }
        if self.task.only_looks() && !member.looks {
            return Err(ToolError::Failed(format!(
                "{name} can change files, which subagents may not do now"
            )));
        }
        member
            .letters
            .send(Letter {
                call: id.to_owned(),
                text: args.message,
                events: self.task.events(),
            })
            .map_err(|_| ToolError::Failed(format!("{name} was stopped")))?;
        let owed = member.owed.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(ToolOutput::new(format!(
            "Sent to {name}, which now owes {owed} {}. Call agent_wait to get {}.",
            if owed == 1 { "reply" } else { "replies" },
            if owed == 1 { "it" } else { "them in order" }
        )))
    }
}

/// Waits for the next reply of a long-lived subagent.
pub struct AgentWait {
    /// Where the subagents are kept.
    crew: Arc<Crew>,
}

/// Arguments of an `agent_wait` call.
#[derive(Deserialize)]
struct WaitArgs {
    /// The name of the subagent.
    name: String,
    /// How long to wait, in seconds.
    timeout: Option<f64>,
}

#[async_trait]
impl Tool for AgentWait {
    /// Returns `agent_wait`.
    fn name(&self) -> &'static str {
        "agent_wait"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "Wait for the next reply of a long-lived subagent, up to `timeout` seconds; replies come in the order of the messages it was sent. Returns at once when it owes none."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The subagent's name." },
                "timeout": { "type": "number", "description": "Seconds to wait at most; 300 when left out, at most 1800." }
            },
            "required": ["name"]
        })
    }

    /// Only looks: it changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Waits for and returns the next reply of the subagent; when it owes none, or the wait times out, says so instead.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for a bad timeout, and [`ToolError::Failed`] when no subagent has the name, the reply is a failure, or it was stopped before replying.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: WaitArgs = parse_args(args)?;
        let name = args.name.trim();
        let seconds = args.timeout.unwrap_or(WAIT_DEFAULT);
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(ToolError::InvalidArguments(
                "`timeout` must be a number of seconds, 0 or more".to_owned(),
            ));
        }
        let seconds = seconds.min(WAIT_MOST);
        let (replies, owed, alive) = self.crew.queue(name)?;
        if owed.load(Ordering::SeqCst) == 0 {
            let state = if alive { "is idle" } else { "was stopped" };
            return Ok(ToolOutput::new(format!(
                "{name} {state} and owes no reply; agent_send sends it a message."
            )));
        }
        let mut replies = replies.lock().await;
        match tokio::time::timeout(Duration::from_secs_f64(seconds), replies.recv()).await {
            Err(_) => Ok(ToolOutput::new(format!(
                "{name} is still working after {seconds} seconds; call agent_wait again."
            ))),
            Ok(None) => {
                owed.store(0, Ordering::SeqCst);
                Err(ToolError::Failed(format!(
                    "{name} was stopped before it replied"
                )))
            }
            Ok(Some(reply)) => {
                owed.fetch_sub(1, Ordering::SeqCst);
                reply.map(ToolOutput::new).map_err(ToolError::Failed)
            }
        }
    }
}

/// Ends a long-lived subagent.
pub struct AgentKill {
    /// Where the subagents are kept.
    crew: Arc<Crew>,
}

/// Arguments of an `agent_kill` call.
#[derive(Deserialize)]
struct KillArgs {
    /// The name of the subagent.
    name: String,
}

#[async_trait]
impl Tool for AgentKill {
    /// Returns `agent_kill`.
    fn name(&self) -> &'static str {
        "agent_kill"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "End a long-lived subagent, stopping whatever it is doing and forgetting its conversation and any reply not yet taken."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The subagent's name." }
            },
            "required": ["name"]
        })
    }

    /// Stops the subagent, forgets it, and opens again the tasks of the shared list it held.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when no subagent has the name.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: KillArgs = parse_args(args)?;
        let name = args.name.trim();
        let mut members = self.crew.lock();
        let Some(member) = members.remove(name) else {
            return Err(unknown(name, &members));
        };
        member.worker.abort();
        self.crew.board.release_all(name);
        let owed = member.owed.load(Ordering::SeqCst);
        let dropped = if owed == 0 {
            String::new()
        } else {
            format!("; {owed} replies it owed are dropped")
        };
        Ok(ToolOutput::new(format!(
            "Ended {name}, the {} agent{dropped}.",
            member.kind
        )))
    }
}

/// Lists the long-lived subagents.
pub struct AgentList {
    /// Where the subagents are kept.
    crew: Arc<Crew>,
}

#[async_trait]
impl Tool for AgentList {
    /// Returns `agent_list`.
    fn name(&self) -> &'static str {
        "agent_list"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "List the long-lived subagents: name, kind, whether working, idle or stopped, how many replies each owes, and what it was started to do."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    /// Only looks: it changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Returns one line per subagent, by name.
    async fn call(&self, _args: Value) -> Result<ToolOutput, ToolError> {
        let members = self.crew.lock();
        if members.is_empty() {
            return Ok(ToolOutput::new(
                "No long-lived subagents; agent_spawn starts one.",
            ));
        }
        let lines: Vec<String> = members
            .iter()
            .map(|(name, member)| {
                format!(
                    "- {name}: {}, {}, owes {}: {}",
                    member.kind,
                    member.state(),
                    member.owed.load(Ordering::SeqCst),
                    member.label
                )
            })
            .collect();
        Ok(ToolOutput::new(lines.join("\n")))
    }
}

/// Sends a short message from one member of a crew to another, to every other member, or to the lead.
pub(crate) struct CrewMessage {
    /// The crew; gone once the conversation ended.
    crew: Weak<Crew>,
    /// The name of the member sending.
    name: String,
    /// How many messages the member sent in its current run.
    sent: Arc<AtomicUsize>,
}

/// Arguments of a `crew_message` call.
#[derive(Deserialize)]
struct MessageArgs {
    /// Who it is for: a member's name, `all` or `lead`.
    to: String,
    /// What it says.
    message: String,
}

#[async_trait]
impl Tool for CrewMessage {
    /// Returns `crew_message`.
    fn name(&self) -> &'static str {
        "crew_message"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "Send a short message to another member of your crew by name, to every other member with `all`, or to the lead with `lead`. A member that is working reads it after its current step; one that is idle starts on it, and its reply goes to the lead, so ask it to answer you with crew_message when you need an answer. The lead reads yours among your replies. crew_members lists the crew. Returns at once; send few messages, each self-contained."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "to": { "type": "string", "description": "A member's name, `all` for every other member, or `lead`." },
                "message": { "type": "string", "description": "What it says." }
            },
            "required": ["to", "message"]
        })
    }

    /// Changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Exclusive: it sets other agents working, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes a call by who it is for.
    fn describe(&self, args: &Value) -> String {
        format!("to {}", args["to"].as_str().unwrap_or_default())
    }

    /// Delivers the message as [`Crew::post`] does, once it is checked, and counts it.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] for an empty or long message or one to the sender itself, and [`ToolError::Failed`] when the member sent [`MESSAGES_PER_RUN`] messages in this run already, the crew is gone, or the message cannot be delivered.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: MessageArgs = parse_args(args)?;
        let to = args.to.trim();
        if args.message.trim().is_empty() {
            return Err(ToolError::InvalidArguments("`message` is empty".to_owned()));
        }
        if args.message.chars().count() > MESSAGE_MOST {
            return Err(ToolError::InvalidArguments(format!(
                "`message` is longer than {MESSAGE_MOST} characters; keep it short and put long material in a file"
            )));
        }
        if to == self.name {
            return Err(ToolError::InvalidArguments(
                "you cannot send a message to yourself".to_owned(),
            ));
        }
        if self.sent.load(Ordering::SeqCst) >= MESSAGES_PER_RUN {
            return Err(ToolError::Failed(format!(
                "you sent {MESSAGES_PER_RUN} messages in this run, the most a member may; no more are sent until your next message arrives. Finish your work and put the rest in your reply."
            )));
        }
        let crew = self
            .crew
            .upgrade()
            .ok_or_else(|| ToolError::Failed("the crew is gone".to_owned()))?;
        let said = crew.post(&self.name, to, &args.message)?;
        self.sent.fetch_add(1, Ordering::SeqCst);
        Ok(ToolOutput::new(said))
    }
}

/// Lists the crew to one of its members.
pub(crate) struct CrewMembers {
    /// The crew; gone once the conversation ended.
    crew: Weak<Crew>,
    /// The name of the member asking.
    name: String,
}

#[async_trait]
impl Tool for CrewMembers {
    /// Returns `crew_members`.
    fn name(&self) -> &'static str {
        "crew_members"
    }

    /// Says what the tool does.
    fn description(&self) -> &str {
        "List the members of your crew: name, kind, whether working, idle or stopped, and what each was started to do. crew_message reaches them by name."
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    /// Only looks: it changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Returns one line per member, by name, the asking one marked, after a line naming the lead.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::Failed`] when the crew is gone.
    async fn call(&self, _args: Value) -> Result<ToolOutput, ToolError> {
        let crew = self
            .crew
            .upgrade()
            .ok_or_else(|| ToolError::Failed("the crew is gone".to_owned()))?;
        let members = crew.lock();
        let lines = members.iter().map(|(name, member)| {
            let you = if *name == self.name { " (you)" } else { "" };
            format!(
                "- {name}{you}: {}, {}: {}",
                member.kind,
                member.state(),
                member.label
            )
        });
        let text = std::iter::once(format!("- {LEAD}: the agent that started the crew"))
            .chain(lines)
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::new(text))
    }
}

/// The tools a long-lived subagent called `name` gets as a member of `crew`: `crew_message`, counting its messages in `sent`, `crew_members` and `crew_tasks`. They hold the crew weakly, since the crew holds the member.
pub(crate) fn member_tools(
    crew: &Arc<Crew>,
    name: &str,
    sent: Arc<AtomicUsize>,
) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(CrewMessage {
            crew: Arc::downgrade(crew),
            name: name.to_owned(),
            sent,
        }),
        Arc::new(CrewMembers {
            crew: Arc::downgrade(crew),
            name: name.to_owned(),
        }),
        Arc::new(CrewTasks {
            crew: Arc::downgrade(crew),
            who: Some(name.to_owned()),
        }),
    ]
}

/// The tools over `crew`: `agent_spawn` and `agent_send` start and run its subagents as `task` does, `agent_wait`, `agent_kill` and `agent_list`, and, once the crew has members, `crew_tasks`, the task list the lead shares with them.
pub fn crew_tools(task: Arc<Task>, crew: Arc<Crew>) -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(AgentSpawn {
            task: task.clone(),
            crew: crew.clone(),
        }),
        Arc::new(AgentSend {
            task,
            crew: crew.clone(),
        }),
        Arc::new(AgentWait { crew: crew.clone() }),
        Arc::new(AgentKill { crew: crew.clone() }),
        Arc::new(AgentList { crew: crew.clone() }),
    ];
    if !crew.lock().is_empty() {
        tools.push(Arc::new(CrewTasks {
            crew: Arc::downgrade(&crew),
            who: None,
        }));
    }
    tools
}
