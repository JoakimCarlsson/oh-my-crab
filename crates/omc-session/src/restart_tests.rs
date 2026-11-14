//! Tests for answering the last prompt again, starting afresh, asking on the side and handing over, driven by a scripted model.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use omc_providers::{ChatModel, ProviderError, Reply, Request, Sink};
use omc_wire::Command;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use super::super::*;
use super::*;

/// What the scripted model answers a request to write a hand-off.
const HANDED: &str = "You carry on the greeting work.";

/// What the scripted model answers a question on the side.
const SIDE: &str = "You said hello.";

/// A model that answers prompts with `answer <n>`, counting them, a hand-off with [`HANDED`] and a question on the side with [`SIDE`], and keeps the last user message of every request.
#[derive(Default)]
struct Scripted {
    /// How many prompts it answered.
    answered: AtomicUsize,
    /// The last user message of every request, in order.
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl ChatModel for Scripted {
    /// Records the request and answers it as the system prompt says it is meant.
    async fn complete(
        &self,
        request: Request<'_>,
        _sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let last = request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                Message::User(text) => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        self.seen.lock().unwrap().push(last);
        let text = if request.system.contains("write the prompt that hands") {
            HANDED.to_owned()
        } else if request.system.contains("on the side") {
            SIDE.to_owned()
        } else {
            format!(
                "answer {}",
                self.answered.fetch_add(1, Ordering::SeqCst) + 1
            )
        };
        Ok(Reply {
            text,
            ..Reply::default()
        })
    }
}

/// A conversation open in a folder of its own named `name` under the build folder, asking `model`; with the events it sends and where its turns report their end.
fn opened(
    name: &str,
    model: Arc<Scripted>,
) -> (
    Session,
    UnboundedReceiver<Event>,
    UnboundedReceiver<(u64, Ended)>,
) {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-session/restart")
        .join(name);
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).unwrap();
    let locations = Locations {
        directory: directory.clone(),
        root: Some(directory.clone()),
        home: None,
        claude: None,
        codex: None,
        crab: None,
        project: true,
    };
    let assessment = assess(&locations, None, None, true, &[]);
    let folder = directory.clone();
    let setting: Arc<dyn Fn() -> Setting + Send + Sync> = Arc::new(move || Setting {
        session: None,
        folder: folder.clone(),
        mode: Arc::new(|| "default"),
    });
    let mut trust = Trust::new(assessment, setting, None, None);
    trust.vouched = true;
    let (out, events) = unbounded_channel();
    let (ended, finished) = unbounded_channel();
    let shared = Shared::new(out);
    shared.set_home(Some(directory.join("home")));
    shared.open(Provider::Claude, vec!["m".into()], "m".into(), Mode::Yolo);
    *shared.model_override.lock().unwrap() = Some(model);
    let session = Session {
        shared,
        history: Vec::new(),
        journal: None,
        open: Some(Open {
            provider: Provider::Claude,
            base: Arc::new(Registry::builtin(&directory).unwrap()),
            denied: Arc::new(Vec::new()),
            discovered: Arc::new(discover(&locations, &[])),
            locations: Arc::new(locations),
            disabled: Vec::new(),
            isolation: None,
            hooks: None,
            trust,
            hub: omc_mcp::Hub::new(Spill::disabled(), None, Arc::new(|_| {})),
            client_servers: Vec::new(),
            user_servers: Vec::new(),
            extensions: omc_extensions::Extensions::new(omc_extensions::Setting {
                session: None,
                folder: directory.clone(),
                mode: Arc::new(|| "default"),
            }),
            user_extensions: Vec::new(),
        }),
        running: None,
        turns: 0,
        resets_at_start: 0,
        ended,
        trust_tickets: 0,
        idle: None,
        snapshots: None,
        turn_id: None,
        put_back: None,
    };
    (session, events, finished)
}

/// The prompt command `text` with no files, under the turn id `turn`.
fn prompt(text: &str, turn: &str) -> Command {
    Command::Prompt {
        text: text.into(),
        files: Vec::new(),
        turn: Some(turn.into()),
        context: None,
    }
}

/// Waits for the running turn to end and takes in its end, as the conversation's loop does.
async fn settle(session: &mut Session, finished: &mut UnboundedReceiver<(u64, Ended)>) {
    let (number, how) = tokio::time::timeout(Duration::from_secs(30), finished.recv())
        .await
        .expect("the turn did not end")
        .expect("the turn's end was lost");
    session.finish(number, how);
}

/// The events sent so far.
fn drain(events: &mut UnboundedReceiver<Event>) -> Vec<Event> {
    std::iter::from_fn(|| events.try_recv().ok()).collect()
}

/// The text of the reply at `at` in `history`, empty when it is not a reply.
fn reply_at(history: &[Message], at: usize) -> String {
    match history.get(at) {
        Some(Message::Assistant(reply)) => reply.text.clone(),
        _ => String::new(),
    }
}

/// The slash commands are read with what follows them, and other text is not one of them.
#[test]
fn the_slash_commands_are_read() {
    assert_eq!(request("/retry"), Some(Asked::Retry));
    assert_eq!(request("  /fresh  "), Some(Asked::Fresh));
    assert_eq!(
        request("/btw what is left?"),
        Some(Asked::Btw("what is left?".to_owned()))
    );
    assert_eq!(request("/btw"), Some(Asked::Btw(String::new())));
    assert_eq!(request("/handoff"), Some(Asked::Handoff(None)));
    assert_eq!(
        request("/handoff  write the docs "),
        Some(Asked::Handoff(Some("write the docs".to_owned())))
    );
    assert_eq!(request("/retrying"), None);
    assert_eq!(request("retry"), None);
    assert_eq!(request("/compact"), None);
}

/// Retrying with nothing said fails; after a turn it takes the reply back out, tells the client the turns left, and answers the same prompt again, by command and by slash alike.
#[tokio::test]
async fn retry_answers_the_last_prompt_again() {
    let model = Arc::new(Scripted::default());
    let (mut session, mut events, mut finished) = opened("retry", model.clone());
    session.handle(Command::Retry).await;
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| matches!(event, Event::Failed(reason) if reason.starts_with("/retry")))
    );
    session.handle(prompt("hello", "t1")).await;
    settle(&mut session, &mut finished).await;
    assert_eq!(session.history.len(), 2);
    assert_eq!(reply_at(&session.history, 1), "answer 1");
    drain(&mut events);
    session.handle(Command::Retry).await;
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| matches!(event, Event::Resumed(_)))
    );
    settle(&mut session, &mut finished).await;
    assert_eq!(session.history.len(), 2);
    assert!(matches!(&session.history[0], Message::User(text) if text.ends_with("hello")));
    assert_eq!(reply_at(&session.history, 1), "answer 2");
    session.handle(prompt("/retry", "t2")).await;
    settle(&mut session, &mut finished).await;
    assert_eq!(session.history.len(), 2);
    assert_eq!(reply_at(&session.history, 1), "answer 3");
}

/// Starting afresh empties the conversation, keeps the model, and ends a slash prompt with `Finished`; it is refused while a turn runs.
#[tokio::test]
async fn fresh_empties_the_conversation() {
    let model = Arc::new(Scripted::default());
    let (mut session, mut events, mut finished) = opened("fresh", model.clone());
    session.handle(prompt("hello", "t1")).await;
    settle(&mut session, &mut finished).await;
    drain(&mut events);
    session.handle(prompt("/fresh", "t2")).await;
    assert!(session.history.is_empty());
    let told = drain(&mut events);
    assert!(told.iter().any(
        |event| matches!(event, Event::Resumed(conversation) if conversation.turns.is_empty())
    ));
    assert!(matches!(told.last(), Some(Event::Finished(None))));
    assert_eq!(
        session.shared.model().map(|(_, model)| model),
        Some("m".to_owned())
    );
    session.handle(prompt("again", "t3")).await;
    session.handle(Command::Fresh).await;
    assert!(
        drain(&mut events)
            .iter()
            .any(|event| matches!(event, Event::Failed(reason) if reason.starts_with("/fresh")))
    );
    settle(&mut session, &mut finished).await;
    assert_eq!(session.history.len(), 2);
    assert_eq!(reply_at(&session.history, 1), "answer 2");
}

/// A question on the side is answered with an `Aside` event from the conversation so far, and the conversation is left as it was.
#[tokio::test]
async fn btw_answers_on_the_side() {
    let model = Arc::new(Scripted::default());
    let (mut session, mut events, mut finished) = opened("btw", model.clone());
    session.handle(prompt("hello", "t1")).await;
    settle(&mut session, &mut finished).await;
    let before = session.history.clone();
    drain(&mut events);
    session.handle(prompt("/btw", "t2")).await;
    assert!(
        drain(&mut events).iter().any(
            |event| matches!(event, Event::Failed(reason) if reason.starts_with("/btw needs"))
        )
    );
    session
        .handle(Command::Btw {
            question: "what did you say?".into(),
        })
        .await;
    let aside = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match events.recv().await {
                Some(Event::Aside { question, answer }) => break (question, answer),
                Some(_) => {}
                None => panic!("the conversation hung up"),
            }
        }
    })
    .await
    .expect("no answer on the side");
    assert_eq!(aside, ("what did you say?".to_owned(), SIDE.to_owned()));
    assert_eq!(session.history, before);
    let asked = model.seen.lock().unwrap().last().cloned().unwrap();
    assert!(asked.contains("[Assistant]: answer 1"));
    assert!(asked.ends_with("<question>\nwhat did you say?\n</question>"));
}

/// Handing over has the model write a prompt toward the goal, starts the conversation afresh, tells the client the prompt and answers it as the first turn of the new conversation.
#[tokio::test]
async fn handoff_starts_a_new_conversation_from_a_written_prompt() {
    let model = Arc::new(Scripted::default());
    let (mut session, mut events, mut finished) = opened("handoff", model.clone());
    session.handle(prompt("hello", "t1")).await;
    settle(&mut session, &mut finished).await;
    drain(&mut events);
    session
        .handle(prompt("/handoff greet in French", "t2"))
        .await;
    settle(&mut session, &mut finished).await;
    let told = drain(&mut events);
    let handed = told.iter().find_map(|event| match event {
        Event::HandedOff { prompt, turn } => Some((prompt.clone(), turn.clone())),
        _ => None,
    });
    let (handed, turn) = handed.expect("the client was not told the hand-off");
    assert_eq!(handed, HANDED);
    assert!(turn.starts_with("handoff-"));
    assert!(
        model
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|asked| asked.contains("[Assistant]: answer 1")
                && asked.ends_with("toward this goal: greet in French"))
    );
    settle(&mut session, &mut finished).await;
    assert_eq!(session.history.len(), 2);
    assert!(matches!(&session.history[0], Message::User(text) if text.ends_with(HANDED)));
    assert_eq!(reply_at(&session.history, 1), "answer 2");
}
