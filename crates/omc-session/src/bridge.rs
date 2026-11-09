//! Questions the agent puts to the client, and the answers that come back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_agent::{ApprovalRequest, Decision, Place, Prompter};
use omc_tools::PlanVerdict;
use omc_wire::{Choice, Event, Inquiry, Permission, Question, Reply, Verdict};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::lifecycle::Lifecycle;

/// The id of the choice that lets a call run this once.
const ALLOW: &str = "allow";

/// The id of the choice that lets a call run and allows its rules for the rest of the conversation.
const ALWAYS: &str = "always";

/// The id of the choice that lets a call run and keeps its rules for every conversation in the project.
const ALWAYS_PROJECT: &str = "always-project";

/// The id of the choice that lets a call run and keeps its rules for every conversation in every project.
const ALWAYS_USER: &str = "always-user";

/// The id of the choice that refuses a call; for a plan, the one that keeps the agent planning, minding the note sent with it.
const REJECT: &str = "reject";

/// The id of the choice that approves a plan to be carried out in a fresh context window that starts from the plan.
const FRESH: &str = "fresh";

/// The id of the choice that rejects a plan outright, which has the agent stop and wait.
const DISCARD: &str = "discard";

/// What the reader picked in answer to a question: the id of the choice, or nothing when they declined, and what they wrote along with it.
#[derive(Debug, Default)]
struct Picked {
    /// The id of the choice, or nothing when the question was declined.
    option: Option<String>,
    /// What the reader wrote along with it, when they wrote anything.
    note: Option<String>,
}

impl Picked {
    /// The decision about a permission the choice amounts to: refusing for any choice besides the allowing ones.
    fn decision(&self) -> Decision {
        match self.option.as_deref() {
            Some(ALLOW) => Decision::AllowOnce,
            Some(ALWAYS) => Decision::AllowAlways,
            Some(ALWAYS_PROJECT) => Decision::AllowAndKeep(Place::Project),
            Some(ALWAYS_USER) => Decision::AllowAndKeep(Place::User),
            _ => Decision::Deny,
        }
    }

    /// The verdict about a plan the choice amounts to, with the note: any allowing choice approves it in the window it was made in, as a client that only knows `allow` and `reject` expects; a declined question or an unknown choice keeps the agent planning.
    fn verdict(self) -> PlanVerdict {
        let allowed = self.decision() != Decision::Deny;
        let note = self.note.filter(|note| !note.trim().is_empty());
        match self.option.as_deref() {
            Some(FRESH) => PlanVerdict::Approved { fresh: true, note },
            Some(DISCARD) => PlanVerdict::Rejected(note),
            _ if allowed => PlanVerdict::Approved { fresh: false, note },
            _ => PlanVerdict::Refine(note),
        }
    }
}

/// The kind of notification the hooks are told when crab asks the reader to allow something.
const PERMISSION: &str = "permission_prompt";

/// The kind of notification the hooks are told when crab puts a form of questions to the reader.
const ELICITATION: &str = "elicitation_dialog";

/// A [`Prompter`] that sends each question to the client and waits for its answer.
pub(crate) struct Questions {
    /// Where the questions go.
    out: UnboundedSender<Event>,
    /// The questions still waiting, by ticket.
    pending: Mutex<HashMap<u64, oneshot::Sender<Picked>>>,
    /// The forms still waiting, by ticket.
    forms: Mutex<HashMap<u64, oneshot::Sender<Vec<Reply>>>>,
    /// The ticket the next question is put under.
    next: AtomicU64,
    /// The hooks told that crab waits on the reader whenever a question is put, when there are any.
    lifecycle: Option<Arc<Lifecycle>>,
}

impl Questions {
    /// Questions that are sent to the client through `out`.
    pub(crate) fn new(out: UnboundedSender<Event>) -> Self {
        Self {
            out,
            pending: Mutex::new(HashMap::new()),
            forms: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            lifecycle: None,
        }
    }

    /// The same questions, telling the `Notification` hooks of `lifecycle` whenever one is put to the client.
    pub(crate) fn heeded_by(self, lifecycle: Arc<Lifecycle>) -> Self {
        Self {
            lifecycle: Some(lifecycle),
            ..self
        }
    }

    /// Tells the `Notification` hooks, when there are any, that crab waits on the reader: `kind` is the kind of notification, `title` and `message` what it says.
    fn notify(&self, kind: &'static str, title: &str, message: &str) {
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.notify(kind, title, message);
        }
    }

    /// Answers the question under `ticket` with the choice `option`, or declines it, with `note`, what the reader wrote along with it, which only a plan passes on to the agent; a ticket nobody waits on is ignored.
    pub(crate) fn answer(&self, ticket: u64, option: Option<&str>, note: Option<&str>) {
        let waiting = self.pending.lock().ok().and_then(|mut p| p.remove(&ticket));
        if let Some(waiting) = waiting {
            let _ = waiting.send(Picked {
                option: option.map(str::to_owned),
                note: note.map(str::to_owned),
            });
        }
    }

    /// Answers the form under `ticket` with `replies`, or abandons it; a ticket nobody waits on is ignored.
    pub(crate) fn reply(&self, ticket: u64, replies: Option<Vec<Reply>>) {
        let waiting = self.forms.lock().ok().and_then(|mut f| f.remove(&ticket));
        if let (Some(waiting), Some(replies)) = (waiting, replies) {
            let _ = waiting.send(replies);
        }
    }

    /// Puts `questions` to the client as a form and waits for the replies; nothing when the client abandons it or is gone.
    pub(crate) async fn inquire(&self, questions: Vec<Question>) -> Option<Vec<Reply>> {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.forms.lock().ok()?.insert(ticket, reply);
        self.out
            .send(Event::Inquire(Inquiry { ticket, questions }))
            .ok()?;
        self.notify(
            ELICITATION,
            "Question for you",
            "crab has questions for you",
        );
        answer.await.ok()
    }

    /// Puts `plan` to the client for approval and waits; what the reader decided, with what they wrote along with it. A question that cannot be put, or that is declined, keeps the agent planning.
    pub(crate) async fn propose(&self, plan: &str) -> PlanVerdict {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        match self.pending.lock() {
            Ok(mut pending) => pending.insert(ticket, reply),
            Err(_) => return PlanVerdict::Refine(None),
        };
        let asked = Event::Ask(Permission {
            ticket,
            title: "Plan ready for review".to_owned(),
            command: None,
            plan: Some(plan.to_owned()),
            choices: plan_choices(),
        });
        if self.out.send(asked).is_err() {
            return PlanVerdict::Refine(None);
        }
        self.notify(
            PERMISSION,
            "Plan ready for review",
            "crab has a plan ready for your review",
        );
        answer.await.unwrap_or_default().verdict()
    }

    /// Declines every question and abandons every form still waiting.
    pub(crate) fn decline_all(&self) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.clear();
        }
        if let Ok(mut forms) = self.forms.lock() {
            forms.clear();
        }
    }
}

#[async_trait]
impl Prompter for Questions {
    /// Puts `request` to the client and waits; declines when the client is gone.
    async fn ask(&self, request: &ApprovalRequest) -> Decision {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        match self.pending.lock() {
            Ok(mut pending) => pending.insert(ticket, reply),
            Err(_) => return Decision::Deny,
        };
        let asked = Event::Ask(Permission {
            ticket,
            title: format!("{}: {}", request.tool, first_line(&request.summary)),
            command: (request.tool == "bash" || request.summary.lines().nth(1).is_some())
                .then(|| request.summary.clone()),
            plan: None,
            choices: choices(request),
        });
        if self.out.send(asked).is_err() {
            return Decision::Deny;
        }
        self.notify(
            PERMISSION,
            "Permission needed",
            &format!("crab needs your permission to use {}", request.tool),
        );
        answer
            .await
            .map_or(Decision::Deny, |picked| picked.decision())
    }
}

/// The answers a plan offers: carrying it out in the window it was made in, which a client that only knows `allow` takes, carrying it out in a fresh context window, sending it back to be revised with the note the reader writes, which a client that only knows `reject` takes, and rejecting it outright.
fn plan_choices() -> Vec<Choice> {
    vec![
        choice(ALLOW, "Approve and execute", Verdict::Allow),
        choice(
            FRESH,
            "Approve and execute in a fresh context",
            Verdict::Allow,
        ),
        choice(REJECT, "Keep planning", Verdict::Reject),
        choice(DISCARD, "Reject the plan", Verdict::Reject),
    ]
}

/// The first line of `text`.
fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

/// The choice `id`, shown as `name`, that amounts to `verdict`.
fn choice(id: &str, name: &str, verdict: Verdict) -> Choice {
    Choice {
        id: id.to_owned(),
        name: name.to_owned(),
        verdict,
    }
}

/// The answers a permission question about `request` offers: allowing it once, allowing it always, naming the rules that adds, for the conversation and, when rules can be kept, for the project and for every project, and refusing it. "Always" is left out when no rule can cover the call.
fn choices(request: &ApprovalRequest) -> Vec<Choice> {
    let mut offered = vec![choice(ALLOW, "Allow once", Verdict::Allow)];
    if !request.rules.is_empty() {
        let named = |rules: &[String]| match rules {
            [only] if *only == request.tool => String::new(),
            rules => format!(" {}", rules.join(", ")),
        };
        let rules = named(&request.rules);
        offered.push(choice(
            ALWAYS,
            &format!("Always allow{rules}"),
            Verdict::Always,
        ));
        if request.keepable {
            offered.push(choice(
                ALWAYS_PROJECT,
                &format!("Always allow{rules} in this project"),
                Verdict::Always,
            ));
            if !request.everywhere.is_empty() {
                offered.push(choice(
                    ALWAYS_USER,
                    &format!(
                        "Always allow{} in every project",
                        named(&request.everywhere)
                    ),
                    Verdict::Always,
                ));
            }
        }
    }
    offered.push(choice(REJECT, "Reject", Verdict::Reject));
    offered
}

#[cfg(test)]
/// Tests for the question bridge.
mod tests {
    use super::*;

    /// An answer reaches the call that waits for it.
    #[tokio::test]
    async fn answers_reach_the_waiting_call() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(&ApprovalRequest {
                        tool: "bash".into(),
                        scope: "bash".into(),
                        summary: "ls".into(),
                        rules: vec!["Bash(ls:*)".into()],
                        ..ApprovalRequest::default()
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        questions.answer(asked.ticket, Some(ALWAYS), None);
        assert_eq!(asking.await.unwrap(), Decision::AllowAlways);
    }

    /// "Always" names the rules it adds, offers keeping them for the project and every project when they can be kept, and each choice reaches the waiting call as its decision.
    #[tokio::test]
    async fn always_names_its_rules_and_offers_where_to_keep_them() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        for (option, decision) in [
            (ALWAYS, Decision::AllowAlways),
            (ALWAYS_PROJECT, Decision::AllowAndKeep(Place::Project)),
            (ALWAYS_USER, Decision::AllowAndKeep(Place::User)),
        ] {
            let asking = {
                let questions = questions.clone();
                tokio::spawn(async move {
                    questions
                        .ask(&ApprovalRequest {
                            tool: "edit".into(),
                            scope: "edit".into(),
                            summary: "src/a.rs".into(),
                            rules: vec!["Edit(/src/**)".into()],
                            everywhere: vec!["Edit(//w/src/**)".into()],
                            keepable: true,
                            ..ApprovalRequest::default()
                        })
                        .await
                })
            };
            let Some(Event::Ask(asked)) = seen.recv().await else {
                panic!("no question");
            };
            let names: Vec<(&str, &str)> = asked
                .choices
                .iter()
                .map(|choice| (choice.id.as_str(), choice.name.as_str()))
                .collect();
            assert_eq!(
                names,
                [
                    (ALLOW, "Allow once"),
                    (ALWAYS, "Always allow Edit(/src/**)"),
                    (ALWAYS_PROJECT, "Always allow Edit(/src/**) in this project"),
                    (
                        ALWAYS_USER,
                        "Always allow Edit(//w/src/**) in every project"
                    ),
                    (REJECT, "Reject"),
                ]
            );
            questions.answer(asked.ticket, Some(option), None);
            assert_eq!(asking.await.unwrap(), decision);
        }
    }

    /// A call no rule can cover is offered no "always", and one whose rules cannot be kept only the conversation's.
    #[tokio::test]
    async fn always_is_offered_only_where_it_means_something() {
        let uncovered = ApprovalRequest {
            tool: "bash".into(),
            ..ApprovalRequest::default()
        };
        let ids: Vec<String> = choices(&uncovered).into_iter().map(|c| c.id).collect();
        assert_eq!(ids, [ALLOW, REJECT]);
        let unkept = ApprovalRequest {
            tool: "web_search".into(),
            rules: vec!["WebSearch".into()],
            ..ApprovalRequest::default()
        };
        let offered = choices(&unkept);
        let ids: Vec<&str> = offered.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, [ALLOW, ALWAYS, REJECT]);
        assert_eq!(offered[1].name, "Always allow WebSearch");
    }

    /// A summary of several lines is shown whole, not only the first line in the title.
    #[tokio::test]
    async fn long_summaries_are_shown_whole() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let summary = "f($X) → g($X)\n- f($X)\n+ g($X)\n+ evil()";
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(&ApprovalRequest {
                        tool: "ast_edit".into(),
                        scope: "ast_edit".into(),
                        summary: summary.into(),
                        ..ApprovalRequest::default()
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        assert_eq!(asked.title, "ast_edit: f($X) → g($X)");
        assert_eq!(asked.command.as_deref(), Some(summary));
        questions.answer(asked.ticket, None, None);
        assert_eq!(asking.await.unwrap(), Decision::Deny);
    }

    /// A question nobody answers is declined when the session clears them.
    #[tokio::test]
    async fn cleared_questions_are_declined() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(&ApprovalRequest {
                        tool: "edit".into(),
                        scope: "edit".into(),
                        summary: "x".into(),
                        ..ApprovalRequest::default()
                    })
                    .await
            })
        };
        seen.recv().await;
        questions.decline_all();
        assert_eq!(asking.await.unwrap(), Decision::Deny);
    }

    /// A form is shown, answered, and its replies reach the asker; an abandoned one gives nothing.
    #[tokio::test]
    async fn forms_are_answered_or_abandoned() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move { questions.inquire(Vec::new()).await })
        };
        let Some(Event::Inquire(form)) = seen.recv().await else {
            panic!("no form");
        };
        questions.reply(form.ticket, Some(vec![Reply::default()]));
        assert_eq!(asking.await.unwrap(), Some(vec![Reply::default()]));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move { questions.inquire(Vec::new()).await })
        };
        let Some(Event::Inquire(form)) = seen.recv().await else {
            panic!("no form");
        };
        questions.reply(form.ticket, None);
        assert_eq!(asking.await.unwrap(), None);
    }

    /// A plan is shown in a permission question offering four choices, each of which, with the note sent along, decides the verdict; `allow` and `reject` mean what they meant before there were more, and a declined question keeps the agent planning.
    #[tokio::test]
    async fn plans_are_approved_sent_back_or_rejected() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let note = || Some("mind the tests".to_owned());
        for (option, sent, verdict) in [
            (
                Some(ALLOW),
                None,
                PlanVerdict::Approved {
                    fresh: false,
                    note: None,
                },
            ),
            (
                Some(FRESH),
                Some("mind the tests"),
                PlanVerdict::Approved {
                    fresh: true,
                    note: note(),
                },
            ),
            (
                Some(REJECT),
                Some("mind the tests"),
                PlanVerdict::Refine(note()),
            ),
            (Some(REJECT), Some("  "), PlanVerdict::Refine(None)),
            (Some(DISCARD), None, PlanVerdict::Rejected(None)),
            (None, None, PlanVerdict::Refine(None)),
            (
                Some(ALWAYS),
                None,
                PlanVerdict::Approved {
                    fresh: false,
                    note: None,
                },
            ),
        ] {
            let asking = {
                let questions = questions.clone();
                tokio::spawn(async move { questions.propose("the plan").await })
            };
            let Some(Event::Ask(asked)) = seen.recv().await else {
                panic!("no question");
            };
            assert_eq!(asked.plan.as_deref(), Some("the plan"));
            let offered: Vec<(&str, Verdict)> = asked
                .choices
                .iter()
                .map(|choice| (choice.id.as_str(), choice.verdict))
                .collect();
            assert_eq!(
                offered,
                [
                    (ALLOW, Verdict::Allow),
                    (FRESH, Verdict::Allow),
                    (REJECT, Verdict::Reject),
                    (DISCARD, Verdict::Reject),
                ]
            );
            questions.answer(asked.ticket, option, sent);
            assert_eq!(asking.await.unwrap(), verdict);
        }
    }

    /// A note sent with an answer to a permission question changes nothing about the decision.
    #[tokio::test]
    async fn a_note_leaves_a_permission_alone() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(&ApprovalRequest {
                        tool: "bash".into(),
                        scope: "bash".into(),
                        summary: "ls".into(),
                        ..ApprovalRequest::default()
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        questions.answer(asked.ticket, Some(FRESH), Some("why"));
        assert_eq!(asking.await.unwrap(), Decision::Deny);
    }

    /// A permission question tells the `Notification` hooks that crab needs the reader's permission, naming the tool.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_question_notifies_the_hooks() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/bridge-notify");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let lifecycle = Arc::new(Lifecycle::default());
        lifecycle.set_engine(Some(Arc::new(omc_hooks::Engine::new(
            vec![omc_hooks::Hook {
                event: omc_hooks::HookEvent::Notification,
                matcher: PERMISSION.to_owned(),
                command: "cat > note.json".to_owned(),
                timeout: None,
                file: dir.join("hooks.json"),
                origin: omc_hooks::HookOrigin::User,
                claude: false,
                plugin: None,
            }],
            omc_hooks::Setting {
                session: None,
                folder: dir.clone(),
                mode: Arc::new(|| "default"),
            },
        ))));
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = Arc::new(Questions::new(out).heeded_by(lifecycle));
        let asking = {
            let questions = questions.clone();
            tokio::spawn(async move {
                questions
                    .ask(&ApprovalRequest {
                        tool: "bash".into(),
                        scope: "bash".into(),
                        summary: "ls".into(),
                        ..ApprovalRequest::default()
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut note = String::new();
        while !note.ends_with('}') && std::time::Instant::now() < deadline {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            note = std::fs::read_to_string(dir.join("note.json")).unwrap_or_default();
        }
        let note: serde_json::Value = serde_json::from_str(&note).unwrap();
        assert_eq!(note["notification_type"], "permission_prompt");
        assert_eq!(note["message"], "crab needs your permission to use bash");
        questions.answer(asked.ticket, Some(REJECT), None);
        assert_eq!(asking.await.unwrap(), Decision::Deny);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
