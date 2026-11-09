//! Asking the user before tools that change things run, and the permission rules that decide for them.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_tools::Subject;

use crate::permissions::{Judgement, Rule, RuleBook};

/// A tool call waiting for the user's decision.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// Name of the tool.
    pub tool: String,
    /// What approving the call is about, such as the tool's name or a site, as the tool names it.
    pub scope: String,
    /// One-line description of the call.
    pub summary: String,
    /// What permission rules look at in the call: its command line, files, folder or site.
    pub subject: Subject,
    /// The allow rules an "always allow" answer adds, for this conversation or this project; empty when no rule can cover the call, and then "always allow" is not offered.
    pub rules: Vec<String>,
    /// The same rules as they are kept for every project, with paths written from the root of the file system.
    pub everywhere: Vec<String>,
    /// Whether rules can be kept beyond the conversation, for the project or for every project.
    pub keepable: bool,
}

/// Where an "always allow" answer's rules are kept beyond the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Place {
    /// For every conversation in the project.
    Project,
    /// For every conversation of the user, in any project.
    User,
}

/// The user's answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run this call only.
    AllowOnce,
    /// Run this call, and allow its rules for the rest of the conversation.
    AllowAlways,
    /// Run this call, and allow its rules for the rest of the conversation and keep them in this place.
    AllowAndKeep(Place),
    /// Do not run the call.
    Deny,
}

/// Something that can ask the user, such as the TUI.
#[async_trait]
pub trait Prompter: Send + Sync {
    /// Asks the user about `request` and returns their decision.
    ///
    /// Implementations that cannot reach the user must answer
    /// [`Decision::Deny`].
    async fn ask(&self, request: &ApprovalRequest) -> Decision;
}

/// Something that keeps allow rules beyond the conversation, such as crab's permissions file.
pub trait Keeper: Send + Sync {
    /// Keeps `rules`, in Claude Code's syntax, in `place`; reports failures itself.
    fn keep(&self, rules: &[String], place: Place);
}

/// Where a call stands before the reader would be asked about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    /// No question is needed: the call may run, or is refused for the reason given.
    Settled(Result<(), String>),
    /// The reader is to be asked this.
    Open(ApprovalRequest),
}

/// The approval policy of a session.
pub struct Approvals {
    /// Whether to allow everything without asking; deny rules still apply.
    yolo: AtomicBool,
    /// The allow rules the user added for the rest of the conversation.
    session: Mutex<Vec<Rule>>,
    /// The rules read from the settings files, with the folders they are read against.
    book: Mutex<Arc<RuleBook>>,
    /// Where rules kept beyond the conversation go, when they can be kept.
    keeper: Mutex<Option<Arc<dyn Keeper>>>,
    /// Where questions go.
    prompter: Arc<dyn Prompter>,
}

impl Approvals {
    /// Creates a policy that asks `prompter`, or never asks when `yolo` is set, with no rules.
    pub fn new(prompter: Arc<dyn Prompter>, yolo: bool) -> Self {
        Self {
            yolo: AtomicBool::new(yolo),
            session: Mutex::new(Vec::new()),
            book: Mutex::new(Arc::new(RuleBook::default())),
            keeper: Mutex::new(None),
            prompter,
        }
    }

    /// Whether everything is allowed without asking.
    pub fn yolo(&self) -> bool {
        self.yolo.load(Ordering::Relaxed)
    }

    /// Allows everything without asking when `yolo` is set, and asks again when it is not.
    pub fn set_yolo(&self, yolo: bool) {
        self.yolo.store(yolo, Ordering::Relaxed);
    }

    /// Puts the rules of `book` in force in place of those before; the rules the user added for the conversation stay.
    pub fn set_rules(&self, book: RuleBook) {
        *self.book.lock().unwrap_or_else(|held| held.into_inner()) = Arc::new(book);
    }

    /// The rules from the settings files in force now.
    pub fn rules(&self) -> Arc<RuleBook> {
        self.book
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// Forgets the allow rules the user added for the conversation, as when another conversation is opened.
    pub fn forget_session(&self) {
        self.session
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clear();
    }

    /// The allow rules the user added for the conversation.
    pub fn session_rules(&self) -> Vec<Rule> {
        self.session
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// Keeps the rules of "always allow" answers beyond the conversation through `keeper`, or only for the conversation when there is none.
    pub fn set_keeper(&self, keeper: Option<Arc<dyn Keeper>>) {
        *self.keeper.lock().unwrap_or_else(|held| held.into_inner()) = keeper;
    }

    /// Whether the permission rules let the agent see what the file at `path`, an absolute path, holds without asking: no deny rule of `read` or `grep` touches it, nor, outside yolo mode, an ask rule, since a tool that finds the file itself cannot ask about each one.
    pub fn may_read(&self, path: &std::path::Path) -> bool {
        let book = self.rules();
        let subject = Subject::Files(vec![path.to_path_buf()]);
        ["read", "grep"]
            .iter()
            .all(|tool| match book.judge(tool, &subject, &[]) {
                Judgement::Denied(_) => false,
                Judgement::Asked(_) => self.yolo(),
                Judgement::Allowed | Judgement::Open => true,
            })
    }

    /// Returns whether the call described by `request`, a call that needs approval, may run.
    ///
    /// See [`Approvals::check`]; this is it for a call that needs approval and that no hook allowed.
    pub async fn allows(&self, request: &ApprovalRequest) -> bool {
        self.check(request, true, false).await.is_ok()
    }

    /// Decides whether the call described by `request` may run: `needed` says whether the tool asks before it runs, and `hooked` whether a hook allowed it without asking.
    ///
    /// A deny rule stops the call whatever else holds, in yolo mode too. Then a call that is not `needed` runs unless an ask rule matches it, a `hooked` call runs, and in yolo mode every call runs. An ask rule makes the reader be asked, without an "always allow"; otherwise allow rules from the files and from earlier answers that cover the whole call let it run, and the reader is asked about the rest, with the rules an "always allow" would add.
    ///
    /// # Errors
    ///
    /// Returns the reason for the model when a deny rule stops the call or the reader refuses it.
    pub async fn check(
        &self,
        request: &ApprovalRequest,
        needed: bool,
        hooked: bool,
    ) -> Result<(), String> {
        match self.standing(request, needed, hooked, false) {
            Standing::Settled(settled) => settled,
            Standing::Open(open) => self.ask(open).await,
        }
    }

    /// Where the call described by `request` stands before the reader is asked: settled by a deny rule, by needing no approval, by `hooked` (a hook allowed it), by yolo mode or by an allow rule, or open, with the request the reader would be asked, the rules an "always allow" answer adds filled in. `needed` says whether the tool asks before it runs; `asked` that a hook wants the reader asked, which counts as an ask rule matching.
    pub fn standing(
        &self,
        request: &ApprovalRequest,
        needed: bool,
        hooked: bool,
        asked: bool,
    ) -> Standing {
        let book = self.rules();
        let session = self.session_rules();
        let judged = book.judge(&request.tool, &request.subject, &session);
        if let Judgement::Denied(rule) = &judged {
            return Standing::Settled(Err(format!(
                "a permission rule denies this tool call: {rule}; do not try to get around it, ask the user instead"
            )));
        }
        let asked = asked || matches!(judged, Judgement::Asked(_));
        if (!needed && !asked) || hooked || self.yolo() {
            return Standing::Settled(Ok(()));
        }
        if judged == Judgement::Allowed && !asked {
            return Standing::Settled(Ok(()));
        }
        let mut request = request.clone();
        if asked {
            request.rules.clear();
            request.everywhere.clear();
        } else {
            request.rules = book.propose(&request.tool, &request.subject, &session, false);
            request.everywhere = book.propose(&request.tool, &request.subject, &session, true);
        }
        Standing::Open(request)
    }

    /// Asks the reader about `request`, a call [`Approvals::standing`] left open, and remembers or keeps the rules of an "always allow" answer.
    ///
    /// # Errors
    ///
    /// Returns the reason for the model when the reader refuses the call.
    pub async fn ask(&self, mut request: ApprovalRequest) -> Result<(), String> {
        let book = self.rules();
        let keeper = self
            .keeper
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone();
        request.keepable = keeper.is_some() && !request.rules.is_empty();
        let decision = self.prompter.ask(&request).await;
        let kept = match decision {
            Decision::AllowOnce => return Ok(()),
            Decision::Deny => return Err("the user denied this tool call".to_owned()),
            Decision::AllowAlways => None,
            Decision::AllowAndKeep(place) => Some(place),
        };
        self.remember(&request.rules, &book);
        if let (Some(place), Some(keeper)) = (kept, keeper) {
            let rules = match place {
                Place::Project => &request.rules,
                Place::User => &request.everywhere,
            };
            if !rules.is_empty() {
                keeper.keep(rules, place);
            }
        }
        Ok(())
    }

    /// Adds `rules`, in Claude Code's syntax, to the allow rules of the conversation, as a hook asked; a rule that cannot be read is skipped.
    pub fn allow_for_session(&self, rules: &[String]) {
        self.remember(rules, &self.rules());
    }

    /// Adds `rules` to the allow rules of the conversation, with `/path` paths read from the project root of `book`.
    fn remember(&self, rules: &[String], book: &RuleBook) {
        let mut session = self.session.lock().unwrap_or_else(|held| held.into_inner());
        for text in rules {
            if let Ok(rule) = Rule::parse(text) {
                let rule = rule.based(book.places.root.clone());
                if !session.contains(&rule) {
                    session.push(rule);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    /// A prompter that gives scripted answers and counts the questions.
    struct Scripted {
        /// Answers still to give.
        answers: Mutex<VecDeque<Decision>>,
        /// Questions asked so far.
        asked: Mutex<usize>,
    }

    impl Scripted {
        /// Creates a prompter that answers with `answers` in order.
        fn new(answers: &[Decision]) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.iter().copied().collect()),
                asked: Mutex::new(0),
            })
        }
    }

    #[async_trait]
    impl Prompter for Scripted {
        /// Counts the question and returns the next answer, denying when out.
        async fn ask(&self, _request: &ApprovalRequest) -> Decision {
            *self.asked.lock().unwrap() += 1;
            self.answers
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Decision::Deny)
        }
    }

    /// Builds a request for `tool`, about the whole tool.
    fn request(tool: &str) -> ApprovalRequest {
        scoped(tool, tool)
    }

    /// Builds a request for `tool` with the approval scope `scope`, about the whole tool.
    fn scoped(tool: &str, scope: &str) -> ApprovalRequest {
        ApprovalRequest {
            tool: tool.into(),
            scope: scope.into(),
            ..ApprovalRequest::default()
        }
    }

    /// Builds a request for `tool` about `subject`.
    fn about(tool: &str, subject: Subject) -> ApprovalRequest {
        ApprovalRequest {
            tool: tool.into(),
            scope: tool.into(),
            subject,
            ..ApprovalRequest::default()
        }
    }

    /// Builds a `bash` request for the command line `line`.
    fn bash(line: &str) -> ApprovalRequest {
        about("bash", Subject::Command(line.into()))
    }

    /// The rules written as `allow`, `ask` and `deny`, read against the project `/w`.
    fn book(allow: &[&str], ask: &[&str], deny: &[&str]) -> RuleBook {
        let parsed = |texts: &[&str]| {
            texts
                .iter()
                .map(|text| Rule::parse(text).unwrap())
                .collect()
        };
        RuleBook {
            allow: parsed(allow),
            ask: parsed(ask),
            deny: parsed(deny),
            places: crate::permissions::Places {
                directory: "/w".into(),
                root: "/w".into(),
                home: Some("/home/u".into()),
            },
        }
    }

    /// A keeper that records what it was given to keep.
    #[derive(Default)]
    struct Recording {
        /// The rules kept, with where.
        kept: Mutex<Vec<(Vec<String>, Place)>>,
    }

    impl Keeper for Recording {
        /// Records `rules` and `place`.
        fn keep(&self, rules: &[String], place: Place) {
            self.kept.lock().unwrap().push((rules.to_vec(), place));
        }
    }

    /// Allow-always on a site covers that site and not another.
    #[tokio::test]
    async fn always_is_remembered_per_site() {
        let prompter = Scripted::new(&[Decision::AllowAlways, Decision::Deny]);
        let approvals = Approvals::new(prompter.clone(), false);
        let site = |host: &str| about("read", Subject::Site(host.into()));
        assert!(approvals.allows(&site("a.example")).await);
        assert!(approvals.allows(&site("a.example")).await);
        assert!(!approvals.allows(&site("localhost")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 2);
    }

    /// A file a deny rule of `read` or `grep` touches may not be read, nor one an ask rule touches outside yolo mode; others may.
    #[test]
    fn may_read_follows_deny_and_ask_rules() {
        let approvals = Approvals::new(Scripted::new(&[]), false);
        approvals.set_rules(book(
            &[],
            &["Read(/secrets/**)"],
            &["Read(.env)", "Grep(*.pem)"],
        ));
        assert!(!approvals.may_read(std::path::Path::new("/w/.env")));
        assert!(!approvals.may_read(std::path::Path::new("/w/sub/.env")));
        assert!(!approvals.may_read(std::path::Path::new("/w/certs/a.pem")));
        assert!(!approvals.may_read(std::path::Path::new("/w/secrets/x.txt")));
        assert!(approvals.may_read(std::path::Path::new("/w/src/main.rs")));
        approvals.set_yolo(true);
        assert!(approvals.may_read(std::path::Path::new("/w/secrets/x.txt")));
        assert!(!approvals.may_read(std::path::Path::new("/w/.env")));
    }

    /// Allow-once asks every time and deny blocks.
    #[tokio::test]
    async fn once_asks_each_time_and_deny_blocks() {
        let prompter = Scripted::new(&[Decision::AllowOnce, Decision::Deny]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&bash("ls")).await);
        assert!(!approvals.allows(&bash("ls")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 2);
    }

    /// Allow-always is remembered per tool for a tool whose calls differ in nothing.
    #[tokio::test]
    async fn always_is_remembered_per_tool() {
        let prompter = Scripted::new(&[Decision::AllowAlways]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&request("web_search")).await);
        assert!(approvals.allows(&scoped("web_search", "web_search")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 1);
        assert!(!approvals.allows(&request("debug")).await);
    }

    /// Allow-always on a command allows its prefix, never every command, and never a compound that sneaks another command in.
    #[tokio::test]
    async fn always_on_a_command_allows_its_prefix_only() {
        let prompter = Scripted::new(&[Decision::AllowAlways]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&bash("cargo test -p foo")).await);
        assert!(approvals.allows(&bash("cargo test --workspace")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 1);
        assert!(!approvals.allows(&bash("cargo test; rm -rf ~")).await);
        assert!(!approvals.allows(&bash("cargo build")).await);
        assert!(!approvals.allows(&bash("cargo test $(rm -rf ~)")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 4);
        assert_eq!(
            approvals.session_rules(),
            [Rule::parse("Bash(cargo test:*)").unwrap().based("")]
        );
    }

    /// The question names the rules an "always" adds: those for the commands not yet allowed.
    #[tokio::test]
    async fn the_question_proposes_rules_for_what_is_not_allowed_yet() {
        let recorder = Arc::new(Recorder::default());
        let approvals = Approvals::new(recorder.clone(), false);
        approvals.set_rules(book(&["Bash(git status:*)"], &[], &[]));
        approvals
            .allows(&bash("git status && cargo test -p x | head -5"))
            .await;
        let asked = recorder.asked.lock().unwrap().clone();
        assert_eq!(asked[0].rules, ["Bash(cargo test:*)", "Bash(head:*)"]);
        assert!(!asked[0].keepable);
    }

    /// An answer to keep the rules hands them to the keeper, for the project as proposed and for every project with absolute paths, and allows them at once.
    #[tokio::test]
    async fn kept_rules_reach_the_keeper() {
        let prompter = Scripted::new(&[
            Decision::AllowAndKeep(Place::Project),
            Decision::AllowAndKeep(Place::User),
        ]);
        let approvals = Approvals::new(prompter.clone(), false);
        approvals.set_rules(book(&[], &[], &[]));
        let keeper = Arc::new(Recording::default());
        approvals.set_keeper(Some(keeper.clone()));
        assert!(approvals.allows(&bash("git status")).await);
        assert!(approvals.allows(&bash("git status -s")).await);
        let edit = about("edit", Subject::Files(vec!["/w/src/a.rs".into()]));
        assert!(approvals.allows(&edit).await);
        assert!(
            approvals
                .allows(&about("write", Subject::Files(vec!["/w/src/b.rs".into()])))
                .await
        );
        assert_eq!(*prompter.asked.lock().unwrap(), 2);
        assert_eq!(
            *keeper.kept.lock().unwrap(),
            [
                (vec!["Bash(git status:*)".to_owned()], Place::Project),
                (vec!["Edit(//w/src/**)".to_owned()], Place::User),
            ]
        );
    }

    /// A deny rule stops a call in yolo mode too, and stops a call that needs no approval.
    #[tokio::test]
    async fn deny_beats_yolo() {
        let prompter = Scripted::new(&[]);
        let approvals = Approvals::new(prompter.clone(), true);
        approvals.set_rules(book(&["Bash"], &[], &["Bash(rm:*)", "Read(./.env)"]));
        let denied = approvals.check(&bash("ls && rm -rf /"), true, false).await;
        assert!(denied.unwrap_err().contains("Bash(rm:*)"));
        assert!(approvals.check(&bash("ls"), true, false).await.is_ok());
        let secret = about("read", Subject::Files(vec!["/w/.env".into()]));
        assert!(approvals.check(&secret, false, false).await.is_err());
        let fine = about("read", Subject::Files(vec!["/w/src/a.rs".into()]));
        assert!(approvals.check(&fine, false, false).await.is_ok());
        assert!(approvals.check(&bash("rm x"), true, true).await.is_err());
        assert_eq!(*prompter.asked.lock().unwrap(), 0);
    }

    /// An ask rule asks even when an allow rule covers the call, without offering "always", and asks about a call that needs no approval; yolo mode and hooks still skip it.
    #[tokio::test]
    async fn ask_beats_allow() {
        let recorder = Arc::new(Recorder::default());
        let approvals = Approvals::new(recorder.clone(), false);
        approvals.set_rules(book(
            &["Bash(git:*)"],
            &["Bash(git push:*)", "Read(secrets/**)"],
            &[],
        ));
        assert!(
            approvals
                .check(&bash("git status"), true, false)
                .await
                .is_ok()
        );
        assert!(
            approvals
                .check(&bash("git push origin"), true, false)
                .await
                .is_ok()
        );
        let secret = about("read", Subject::Files(vec!["/w/secrets/k".into()]));
        assert!(approvals.check(&secret, false, false).await.is_ok());
        assert!(approvals.check(&bash("git push"), true, true).await.is_ok());
        approvals.set_yolo(true);
        assert!(
            approvals
                .check(&bash("git push"), true, false)
                .await
                .is_ok()
        );
        let asked = recorder.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 2);
        assert!(asked.iter().all(|request| request.rules.is_empty()));
    }

    /// A command line the splitter does not take apart offers no "always", and an answer of "always" adds nothing.
    #[tokio::test]
    async fn opaque_commands_are_never_remembered() {
        let recorder = Arc::new(Recorder::default());
        let approvals = Approvals::new(recorder.clone(), false);
        approvals.allows(&bash("for f in *; do rm $f; done")).await;
        assert!(recorder.asked.lock().unwrap()[0].rules.is_empty());
        let prompter = Scripted::new(&[Decision::AllowAlways, Decision::Deny]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&bash("if true; then ls; fi")).await);
        assert!(!approvals.allows(&bash("if true; then ls; fi")).await);
    }

    /// Forgetting the conversation's rules asks again.
    #[tokio::test]
    async fn forgetting_the_session_asks_again() {
        let prompter = Scripted::new(&[Decision::AllowAlways, Decision::Deny]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&bash("make")).await);
        approvals.forget_session();
        assert!(!approvals.allows(&bash("make")).await);
    }

    /// A prompter that records each request and allows it once.
    #[derive(Default)]
    struct Recorder {
        /// The requests asked about.
        asked: Mutex<Vec<ApprovalRequest>>,
    }

    #[async_trait]
    impl Prompter for Recorder {
        /// Records `request` and allows it once.
        async fn ask(&self, request: &ApprovalRequest) -> Decision {
            self.asked.lock().unwrap().push(request.clone());
            Decision::AllowOnce
        }
    }

    /// Yolo mode allows everything without asking.
    #[tokio::test]
    async fn yolo_never_asks() {
        let prompter = Scripted::new(&[]);
        let approvals = Approvals::new(prompter.clone(), true);
        assert!(approvals.allows(&request("bash")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 0);
    }
}
