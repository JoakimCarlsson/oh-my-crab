//! `crab exec`: one prompt in, the agent's answer out, with nobody there to ask, for scripts, CI and sandboxes.

use std::collections::HashSet;
use std::io::Read;
use std::path::PathBuf;
use std::time::Duration;

use omc_wire::{Command, Event, LanguageAnswer, Opening, Verdict};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

/// What `crab exec` was asked to do, read from its arguments.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Exec {
    /// The provider to open the conversation with; crab's default when not given.
    provider: Option<String>,
    /// The model to use; the provider's default when not given.
    model: Option<String>,
    /// The folder the agent works in; the current one when not given.
    folder: Option<PathBuf>,
    /// Whether the project's own hooks, instruction files, skills and agents are trusted and used.
    trust: bool,
    /// Whether the result is printed as one JSON object rather than as the answer's text.
    json: bool,
    /// How long the turn may take before it is stopped and counted as failed.
    timeout: Option<Duration>,
    /// The prompt; read from standard input when not given.
    prompt: Option<String>,
}

/// How a run ended, as `--json` prints it.
#[derive(Debug, Default, PartialEq, serde::Serialize)]
pub(crate) struct Outcome {
    /// What the agent said after it last used a tool: its answer.
    result: String,
    /// Whether the turn failed.
    is_error: bool,
    /// Why the turn failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    /// How many requests the model was sent.
    requests: u64,
    /// The tokens sent to the model, summed over every request.
    input_tokens: u64,
    /// The tokens the model wrote, reasoning included, summed over every request.
    output_tokens: u64,
    /// How many tool calls the agent made.
    tool_calls: u64,
}

/// An [`Outcome`] being gathered from a turn's events, with the tool calls already counted.
#[derive(Debug, Default)]
struct Tally {
    /// What has been gathered so far.
    outcome: Outcome,
    /// The ids of the tool calls seen, so that each is counted once however often it is reported.
    tools: HashSet<String>,
    /// Whether the turn has got under way, after which a [`Event::Failed`] is a warning rather than the end.
    active: bool,
}

/// Reads the arguments that follow `exec`; the words that are not options make up the prompt.
///
/// # Errors
///
/// Fails on an unknown option, an option missing its value or a timeout that is not a whole number of seconds.
pub(crate) fn parse(arguments: impl IntoIterator<Item = String>) -> anyhow::Result<Exec> {
    let mut exec = Exec::default();
    let mut words = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let mut value = |name: &str| {
            arguments
                .next()
                .ok_or_else(|| anyhow::anyhow!("{name} needs a value"))
        };
        match argument.as_str() {
            "--provider" => exec.provider = Some(value("--provider")?),
            "--model" => exec.model = Some(value("--model")?),
            "--folder" => exec.folder = Some(PathBuf::from(value("--folder")?)),
            "--timeout" => {
                let seconds = value("--timeout")?;
                let seconds: u64 = seconds
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--timeout takes seconds, not {seconds}"))?;
                exec.timeout = Some(Duration::from_secs(seconds));
            }
            "--trust" => exec.trust = true,
            "--json" => exec.json = true,
            "--" => words.extend(arguments.by_ref()),
            option if option.starts_with("--") => {
                return Err(anyhow::anyhow!("exec does not know {option}"));
            }
            word => words.push(word.to_owned()),
        }
    }
    if !words.is_empty() {
        exec.prompt = Some(words.join(" "));
    }
    Ok(exec)
}

/// Runs `exec`: opens a conversation in its folder with nothing asked before a tool runs, sends the prompt, answers what the harness would ask a person, and prints the answer, or the [`Outcome`] as JSON, when the turn ends.
///
/// Gives whether the turn succeeded.
///
/// # Errors
///
/// Fails when the prompt is empty or cannot be read, the folder cannot be resolved or the result cannot be written.
pub(crate) async fn run(exec: Exec) -> anyhow::Result<bool> {
    let prompt = match exec.prompt.clone() {
        Some(prompt) => prompt,
        None => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            text
        }
    };
    if prompt.trim().is_empty() {
        return Err(anyhow::anyhow!(
            "exec needs a prompt, as words or on standard input"
        ));
    }
    let folder = match &exec.folder {
        Some(folder) => std::fs::canonicalize(folder)?,
        None => std::env::current_dir()?,
    };
    let (commands, received) = unbounded_channel();
    let (sent, events) = unbounded_channel();
    let session = tokio::spawn(omc_session::run(received, sent));
    let _ = commands.send(Command::Open(opening(&exec, folder)));
    let _ = commands.send(Command::Prompt {
        text: prompt,
        files: Vec::new(),
        turn: None,
        context: None,
    });
    let mut tally = Tally::default();
    let followed = converse(&commands, events, exec.trust, &mut tally);
    match exec.timeout {
        Some(limit) => {
            if tokio::time::timeout(limit, followed).await.is_err() {
                tally.outcome.is_error = true;
                tally.outcome.error = Some(format!("timed out after {}s", limit.as_secs()));
            }
        }
        None => followed.await,
    }
    let _ = commands.send(Command::Close);
    session.abort();
    report(&exec, &tally.outcome)?;
    Ok(!tally.outcome.is_error)
}

/// How the conversation is opened: in `folder`, with the provider and model asked for, every tool run without asking, nothing kept from an earlier conversation, and crab's own language services.
fn opening(exec: &Exec, folder: PathBuf) -> Opening {
    Opening {
        directory: folder,
        home: None,
        provider: exec.provider.clone().unwrap_or_default(),
        model: exec.model.clone(),
        yolo: true,
        tools: Vec::new(),
        denied: Vec::new(),
        disabled: Vec::new(),
        conversation: None,
        history: None,
        keep: None,
        defaults: Vec::new(),
        before: None,
        languages: false,
        navigation: false,
    }
}

/// Follows the turn's events into `tally` until the turn ends, answering what waits on an answer through `commands`.
async fn converse(
    commands: &UnboundedSender<Command>,
    mut events: UnboundedReceiver<Event>,
    trust: bool,
    tally: &mut Tally,
) {
    while let Some(event) = events.recv().await {
        if let Some(reason) = ending(&event, tally.active) {
            tally.outcome.is_error = reason.is_some();
            tally.outcome.error = reason;
            return;
        }
        take(&event, tally);
        if let Some(answer) = answer(&event, trust) {
            let _ = commands.send(answer);
        }
    }
    tally.outcome.is_error = true;
    tally.outcome.error = Some("the harness stopped before the turn ended".to_owned());
}

/// Whether `event` ends the run, and if so with what failure: `Some(None)` for a turn that finished well, `Some(Some(reason))` for one that failed, and `None` while it goes on. A failure reported once the turn is `active` is the harness going on after a problem, not the end.
fn ending(event: &Event, active: bool) -> Option<Option<String>> {
    match event {
        Event::Finished(reason) => Some(reason.clone()),
        Event::SignedOut { reason, .. } => Some(Some(format!("signed out: {reason}"))),
        Event::Failed(reason) if !active => Some(Some(reason.clone())),
        _ => None,
    }
}

/// Counts `event` into `tally`, keeping only what the agent said since its last tool call as the answer.
fn take(event: &Event, tally: &mut Tally) {
    let outcome = &mut tally.outcome;
    match event {
        Event::Used(usage) => {
            outcome.requests += 1;
            outcome.input_tokens += usage.used;
            outcome.output_tokens += usage.written;
        }
        Event::Said(text) => outcome.result.push_str(text),
        Event::Tool(tool) => {
            if tally.tools.insert(tool.id.clone()) {
                outcome.tool_calls += 1;
            }
            outcome.result.clear();
        }
        Event::Started(_) | Event::Thought(_) => {}
        _ => return,
    }
    tally.active = true;
}

/// What a run answers when the harness waits on `event`, since nobody is there to decide: no to every approval and question, the project trusted only with `--trust`, and no help from language services, which a run does not offer; nothing for an event that waits on no answer.
fn answer(event: &Event, trust: bool) -> Option<Command> {
    Some(match event {
        Event::Language(request) => Command::Language {
            ticket: request.ticket,
            answer: LanguageAnswer::Unsupported {
                reason: "crab exec answers no language questions".to_owned(),
            },
        },
        Event::Ask(permission) => Command::Choose {
            ticket: permission.ticket,
            option: None,
            note: None,
        },
        Event::Inquire(inquiry) => Command::Answer {
            ticket: inquiry.ticket,
            replies: None,
        },
        Event::Trust(request) => Command::Trust {
            ticket: request.ticket,
            verdict: if trust {
                Verdict::Allow
            } else {
                Verdict::Reject
            },
        },
        _ => return None,
    })
}

/// Prints `outcome`: as one JSON object with `--json`, otherwise the answer on standard output and the reason of a failure on standard error.
///
/// # Errors
///
/// Fails when the outcome cannot be encoded.
fn report(exec: &Exec, outcome: &Outcome) -> anyhow::Result<()> {
    if exec.json {
        println!("{}", serde_json::to_string(outcome)?);
        return Ok(());
    }
    if !outcome.result.is_empty() {
        println!("{}", outcome.result.trim_end());
    }
    if let Some(error) = &outcome.error {
        eprintln!("crab exec: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omc_wire::Usage;

    #[test]
    fn options_are_read_and_the_rest_is_the_prompt() {
        let exec = parse(
            [
                "--provider",
                "openrouter",
                "--model",
                "a/b",
                "--json",
                "--trust",
                "--timeout",
                "60",
                "fix",
                "the",
                "bug",
            ]
            .map(String::from),
        )
        .unwrap();
        assert_eq!(exec.provider.as_deref(), Some("openrouter"));
        assert_eq!(exec.model.as_deref(), Some("a/b"));
        assert!(exec.json && exec.trust);
        assert_eq!(exec.timeout, Some(Duration::from_secs(60)));
        assert_eq!(exec.prompt.as_deref(), Some("fix the bug"));
        assert!(parse(["--nope".to_owned()]).is_err());
        assert!(parse(["--timeout".to_owned(), "soon".to_owned()]).is_err());
        assert_eq!(parse(Vec::new()).unwrap().prompt, None);
    }

    #[test]
    fn the_answer_is_what_was_said_after_the_last_tool() {
        let mut tally = Tally::default();
        let tool = |id: &str| {
            Event::Tool(serde_json::from_value(serde_json::json!({ "id": id })).unwrap())
        };
        for event in [
            Event::Said("Let me look.".to_owned()),
            tool("1"),
            tool("1"),
            Event::Used(Usage {
                used: 100,
                size: 1000,
                written: 10,
            }),
            Event::Said("Fixed ".to_owned()),
            Event::Said("it.".to_owned()),
            Event::Used(Usage {
                used: 150,
                size: 1000,
                written: 5,
            }),
        ] {
            take(&event, &mut tally);
        }
        assert_eq!(tally.outcome.result, "Fixed it.");
        assert_eq!(tally.outcome.tool_calls, 1);
        assert_eq!(tally.outcome.requests, 2);
        assert_eq!(tally.outcome.input_tokens, 250);
        assert_eq!(tally.outcome.output_tokens, 15);
        assert!(tally.active);
    }

    #[test]
    fn only_finishing_or_failing_before_the_turn_starts_ends_a_run() {
        assert_eq!(ending(&Event::Finished(None), true), Some(None));
        assert_eq!(
            ending(&Event::Failed("no key".to_owned()), false),
            Some(Some("no key".to_owned()))
        );
        assert_eq!(ending(&Event::Failed("retrying".to_owned()), true), None);
        assert_eq!(ending(&Event::Said("hi".to_owned()), false), None);
    }

    #[test]
    fn trust_is_given_only_when_asked_for() {
        let event = Event::Trust(omc_wire::TrustRequest {
            ticket: 7,
            root: PathBuf::from("/p"),
            standing: omc_wire::Standing::Unknown,
            skipped: Vec::new(),
        });
        let verdict = |trust| match answer(&event, trust) {
            Some(Command::Trust { verdict, .. }) => Some(verdict),
            _ => None,
        };
        assert_eq!(verdict(true), Some(Verdict::Allow));
        assert_eq!(verdict(false), Some(Verdict::Reject));
    }
}
