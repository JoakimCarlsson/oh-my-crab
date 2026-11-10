//! One run: a model given one task in a fresh copy of its files, and what it left there.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};
use std::time::{Duration, Instant};

use omc_client::{Harness, Heard};
use omc_wire::{Command, Event, LanguageAnswer, LanguageAsk, Opening, Verdict};

use crate::args::{Contender, Model};
use crate::tally::{Tally, Uses, edits};
use crate::task::{Task, commit, copy, differing, identical, init};

/// How long the harness is listened to before the time left is looked at again.
const POLL: Duration = Duration::from_secs(1);

/// The folder under a run's folder that the model works in.
const WORK: &str = "work";

/// What is listed among the files that differ when a task's check command did not end successfully.
const CHECK_FAILED: &str = "(the check failed)";

/// The file under a run's folder that the harness's events are kept in, one a line.
const TRANSCRIPT: &str = "events.jsonl";

/// What every run of a benchmark shares.
#[derive(Clone, Debug)]
pub(crate) struct Setup {
    /// The crab that is measured.
    pub(crate) crab: PathBuf,
    /// The names of the tools the model is given, as crab calls them.
    pub(crate) tools: Vec<String>,
    /// The names of every tool crab offers, from which the ones a task does not give the model are denied; empty when crab is not measured.
    pub(crate) offered: Vec<String>,
    /// The conversation settings every run starts with, as ids and values.
    pub(crate) settings: Vec<(String, String)>,
    /// How long one run may take.
    pub(crate) timeout: Duration,
    /// A folder with nothing in it, which stands in for the user's own folders so that their instructions, skills, hooks and rules stay out of the measurement.
    pub(crate) blank: PathBuf,
}

/// What one run came to.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct Outcome {
    /// What was measured: the harness and its model, as the report names them.
    pub(crate) model: String,
    /// The name of the task.
    pub(crate) task: String,
    /// Which run of the task this was, from 1.
    pub(crate) run: usize,
    /// Whether the files the model left are the ones the task asks for.
    pub(crate) passed: bool,
    /// Whether it passed without a single edit call that did not work.
    pub(crate) clean: bool,
    /// Whether it passed and every file is, byte for byte, what the task asks for, blank lines and the white space at the end of lines included; a task scored by a command alone is exact when it passed.
    #[serde(default)]
    pub(crate) exact: bool,
    /// The files that are not what the task asks for.
    pub(crate) differing: Vec<String>,
    /// Why the run ended badly, when it did: the turn failed, the time ran out or the harness stopped.
    pub(crate) error: Option<String>,
    /// How often each tool was called, by its name.
    pub(crate) tools: BTreeMap<String, Uses>,
    /// The most tokens the conversation held at once.
    pub(crate) context: u64,
    /// How many requests the model was sent.
    pub(crate) requests: u32,
    /// The tokens the model was sent, added up over the requests; what it read from a cache counts too.
    pub(crate) tokens: u64,
    /// The tokens the model wrote, added up over its replies.
    #[serde(default)]
    pub(crate) written: u64,
    /// How long the run took, in seconds.
    pub(crate) seconds: f64,
}

/// Runs `task` with `contender` in a fresh copy of its files under `folder`, where the transcript is kept too, as run `number`.
///
/// Nothing is returned as an error: a run that cannot be set up, fails or runs out of time is an outcome that did not pass, with the reason in it.
pub(crate) fn run(
    setup: &Setup,
    contender: &Contender,
    task: &Task,
    number: usize,
    folder: &Path,
) -> Outcome {
    let started = Instant::now();
    let work = folder.join(WORK);
    let transcript = folder.join(TRANSCRIPT);
    let (tally, error) = match (prepare(task, &work), contender) {
        (Err(reason), _) => (Tally::default(), Some(reason)),
        (Ok(()), Contender::Crab(model)) => converse(setup, model, task, &work, &transcript),
        (Ok(()), Contender::ClaudeCode(model)) => {
            crate::baseline::claude_code(setup, model, task, &work, &transcript)
        }
        (Ok(()), Contender::Codex(model)) => {
            crate::baseline::codex(setup, model, task, &work, &transcript)
        }
        (Ok(()), Contender::Command { words, .. }) => {
            crate::baseline::command(setup, words, task, &work, &transcript)
        }
    };
    let (mut differing, error) = match task.after.as_ref().map(|after| differing(after, &work)) {
        None => (Vec::new(), error),
        Some(Ok(differing)) => (differing, error),
        Some(Err(reason)) => (Vec::new(), error.or(Some(reason))),
    };
    if let Some(check) = &task.check
        && !checks(check, &work)
    {
        differing.push(CHECK_FAILED.to_owned());
    }
    let (context, requests, tokens) = (tally.context(), tally.requests(), tally.sent());
    let written = tally.written();
    let tools = tally.into_tools();
    let passed = error.is_none() && differing.is_empty();
    let exact = passed
        && task
            .after
            .as_ref()
            .is_none_or(|after| identical(after, &work));
    Outcome {
        model: contender.label(),
        task: task.name.clone(),
        run: number,
        passed,
        clean: passed && edits(&tools).failed == 0,
        exact,
        differing,
        error,
        tools,
        context,
        requests,
        tokens,
        written,
        seconds: started.elapsed().as_secs_f64(),
    }
}

/// Makes `work` a fresh copy of the files `task` starts from, committed in a git repository of its own, so that the project the harness sees ends there and starts clean.
///
/// # Errors
///
/// Fails when the files cannot be copied, or git cannot make the repository or the commit.
fn prepare(task: &Task, work: &Path) -> Result<(), String> {
    let _ = std::fs::remove_dir_all(work);
    copy(&task.before, work)?;
    init(work)?;
    commit(work)
}

/// Whether the command `check`, run by `bash` in `work`, ends successfully; it does not when `bash` cannot be started.
fn checks(check: &str, work: &Path) -> bool {
    Process::new("bash")
        .args(["-c", check])
        .current_dir(work)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The command that starts the measured crab in `work`, with the user's own folders and files replaced by empty ones, and memory, tool servers, extensions and the catalog download off.
fn harness(setup: &Setup, work: &Path) -> Process {
    let mut process = Process::new(&setup.crab);
    process.arg("serve").current_dir(work);
    for folder in ["OMC_CONFIG_DIR", "CLAUDE_CONFIG_DIR", "CODEX_HOME"] {
        process.env(folder, &setup.blank);
    }
    for (file, name) in [
        ("OMC_HOOKS_FILE", "hooks.json"),
        ("OMC_PERMISSIONS_FILE", "permissions.json"),
        ("OMC_TRUST_FILE", "trust.json"),
    ] {
        process.env(file, setup.blank.join(name));
    }
    for switch in [
        "OMC_MCP_CONFIG",
        "OMC_EXTENSIONS",
        "OMC_MEMORY",
        "OMC_MODELS_REFRESH",
    ] {
        process.env(switch, "0");
    }
    process
}

/// How the conversation of a run of `task` is opened: `model` working in `work` with the task's tools and no others, nothing asked before a tool runs, nothing saved, and the language services answered by the benchmark itself so that no language server is started.
fn opening(setup: &Setup, model: &Model, task: &Task, work: &Path) -> Opening {
    let given = task.tools(&setup.tools);
    Opening {
        directory: work.to_path_buf(),
        home: None,
        provider: model.provider.clone(),
        model: model.model.clone(),
        yolo: true,
        tools: Vec::new(),
        denied: setup
            .offered
            .iter()
            .filter(|tool| !given.contains(tool))
            .cloned()
            .collect(),
        disabled: Vec::new(),
        conversation: None,
        history: None,
        keep: None,
        defaults: setup.settings.clone(),
        before: None,
        languages: true,
        navigation: false,
    }
}

/// Holds the conversation of one run: gives `model` the prompt of `task` in `work`, answers what the harness asks, keeps every event in the file `transcript`, and stops when the turn ends or the time is up.
///
/// Gives the count of what the model did and, when the run ended badly, why.
fn converse(
    setup: &Setup,
    model: &Model,
    task: &Task,
    work: &Path,
    transcript: &Path,
) -> (Tally, Option<String>) {
    let mut tally = Tally::default();
    let mut harness = match Harness::start(harness(setup, work)) {
        Ok(harness) => harness,
        Err(reason) => return (tally, Some(reason)),
    };
    let mut kept = std::fs::File::create(transcript).ok();
    let asked = harness
        .send(&Command::Open(opening(setup, model, task, work)))
        .and_then(|()| {
            harness.send(&Command::Prompt {
                text: task.prompt.clone(),
                files: Vec::new(),
                turn: None,
                context: None,
            })
        });
    if let Err(reason) = asked {
        return (tally, Some(reason));
    }
    let deadline = Instant::now() + setup.timeout;
    let error = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break Some(format!("timed out after {}s", setup.timeout.as_secs()));
        }
        let event = match harness.listen(left.min(POLL)) {
            Heard::Silence => continue,
            Heard::Closed(said) => break Some(said),
            Heard::Event(event) => event,
        };
        if let (Some(file), Ok(line)) = (&mut kept, omc_wire::encode(&event)) {
            let _ = file.write_all(line.as_bytes());
        }
        tally.take(&event);
        match event {
            Event::Finished(reason) => break reason,
            Event::SignedOut { reason, .. } => break Some(format!("signed out: {reason}")),
            Event::Failed(reason) if !tally.active() => break Some(reason),
            other => {
                if let Some(answer) = answer(&other)
                    && let Err(reason) = harness.send(&answer)
                {
                    break Some(reason);
                }
            }
        }
    };
    let _ = harness.send(&Command::Close);
    (tally, error)
}

/// What the benchmark answers when the harness waits on `event`: no formatting and no diagnostics from the language services, and a refusal of every question, approval and request for trust, since nobody is there to decide; nothing for an event that waits on no answer.
fn answer(event: &Event) -> Option<Command> {
    Some(match event {
        Event::Language(request) => Command::Language {
            ticket: request.ticket,
            answer: match &request.ask {
                LanguageAsk::Format { .. } => LanguageAnswer::Formatted { text: None },
                LanguageAsk::Diagnostics { .. } => LanguageAnswer::Diagnostics {
                    diagnostics: Vec::new(),
                },
                _ => LanguageAnswer::Unsupported {
                    reason: "no language services are available here".to_owned(),
                },
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
            verdict: Verdict::Reject,
        },
        _ => return None,
    })
}

#[cfg(test)]
/// Tests for what the benchmark answers the harness.
mod tests {
    use super::*;
    use omc_wire::{Inquiry, LanguageRequest, Permission};

    /// A question, an approval and a status request are each turned down under their own ticket, and an event that asks nothing gets no answer.
    #[test]
    fn what_waits_is_turned_down() {
        assert_eq!(
            answer(&Event::Inquire(Inquiry {
                ticket: 4,
                questions: Vec::new()
            })),
            Some(Command::Answer {
                ticket: 4,
                replies: None
            })
        );
        assert_eq!(
            answer(&Event::Ask(Permission {
                ticket: 5,
                title: "edit".to_owned(),
                command: None,
                plan: None,
                choices: Vec::new()
            })),
            Some(Command::Choose {
                ticket: 5,
                option: None,
                note: None
            })
        );
        assert!(matches!(
            answer(&Event::Language(LanguageRequest {
                ticket: 6,
                ask: LanguageAsk::Status {}
            })),
            Some(Command::Language {
                ticket: 6,
                answer: LanguageAnswer::Unsupported { .. }
            })
        ));
        assert_eq!(answer(&Event::Said("done".to_owned())), None);
    }
}
