//! Questions the agent puts to the client, and the answers that come back.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_agent::{ApprovalRequest, Decision, Prompter};
use omc_wire::{Choice, Event, Inquiry, Permission, Question, Reply, Verdict};
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::lifecycle::Lifecycle;

/// The id of the choice that lets a call run this once.
const ALLOW: &str = "allow";

/// The id of the choice that lets a tool run for the rest of the session.
const ALWAYS: &str = "always";

/// The id of the choice that refuses a call.
const REJECT: &str = "reject";

/// The kind of notification the hooks are told when crab asks the reader to allow something.
const PERMISSION: &str = "permission_prompt";

/// The kind of notification the hooks are told when crab puts a form of questions to the reader.
const ELICITATION: &str = "elicitation_dialog";

/// A [`Prompter`] that sends each question to the client and waits for its answer.
pub(crate) struct Questions {
    /// Where the questions go.
    out: UnboundedSender<Event>,
    /// The questions still waiting, by ticket.
    pending: Mutex<HashMap<u64, oneshot::Sender<Decision>>>,
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

    /// Answers the question under `ticket` with the choice `option`, or declines it; a ticket nobody waits on is ignored.
    pub(crate) fn answer(&self, ticket: u64, option: Option<&str>) {
        let waiting = self.pending.lock().ok().and_then(|mut p| p.remove(&ticket));
        if let Some(waiting) = waiting {
            let _ = waiting.send(match option {
                Some(ALLOW) => Decision::AllowOnce,
                Some(ALWAYS) => Decision::AllowAlways,
                _ => Decision::Deny,
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

    /// Puts `plan` to the client for approval and waits; whether it was approved.
    pub(crate) async fn propose(&self, plan: &str) -> bool {
        let ticket = self.next.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        match self.pending.lock() {
            Ok(mut pending) => pending.insert(ticket, reply),
            Err(_) => return false,
        };
        let asked = Event::Ask(Permission {
            ticket,
            title: "Plan ready for review".to_owned(),
            command: None,
            plan: Some(plan.to_owned()),
            choices: vec![
                choice(ALLOW, "Approve and start", Verdict::Allow),
                choice(REJECT, "Keep planning", Verdict::Reject),
            ],
        });
        if self.out.send(asked).is_err() {
            return false;
        }
        self.notify(
            PERMISSION,
            "Plan ready for review",
            "crab has a plan ready for your review",
        );
        matches!(
            answer.await,
            Ok(Decision::AllowOnce | Decision::AllowAlways)
        )
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
        answer.await.unwrap_or(Decision::Deny)
    }
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

/// The answers a permission question about `request` offers; "always" names
/// its scope when that is narrower than the whole tool.
fn choices(request: &ApprovalRequest) -> Vec<Choice> {
    let always = if request.scope == request.tool {
        "Always allow".to_owned()
    } else {
        format!("Always allow {}", request.scope)
    };
    vec![
        choice(ALLOW, "Allow once", Verdict::Allow),
        choice(ALWAYS, &always, Verdict::Always),
        choice(REJECT, "Reject", Verdict::Reject),
    ]
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
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        questions.answer(asked.ticket, Some(ALWAYS));
        assert_eq!(asking.await.unwrap(), Decision::AllowAlways);
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
                    })
                    .await
            })
        };
        let Some(Event::Ask(asked)) = seen.recv().await else {
            panic!("no question");
        };
        assert_eq!(asked.title, "ast_edit: f($X) → g($X)");
        assert_eq!(asked.command.as_deref(), Some(summary));
        questions.answer(asked.ticket, None);
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

    /// A plan is shown in a permission question and the choice decides.
    #[tokio::test]
    async fn plans_are_approved_or_rejected() {
        let (out, mut seen) = tokio::sync::mpsc::unbounded_channel();
        let questions = std::sync::Arc::new(Questions::new(out));
        for (option, approved) in [(ALLOW, true), (REJECT, false)] {
            let asking = {
                let questions = questions.clone();
                tokio::spawn(async move { questions.propose("the plan").await })
            };
            let Some(Event::Ask(asked)) = seen.recv().await else {
                panic!("no question");
            };
            assert_eq!(asked.plan.as_deref(), Some("the plan"));
            assert_eq!(asked.choices.len(), 2);
            questions.answer(asked.ticket, Some(option));
            assert_eq!(asking.await.unwrap(), approved);
        }
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
        questions.answer(asked.ticket, Some(REJECT));
        assert_eq!(asking.await.unwrap(), Decision::Deny);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
