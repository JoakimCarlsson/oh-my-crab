//! Asking the user before tools that change things run.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

/// A tool call waiting for the user's decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    /// Name of the tool.
    pub tool: String,
    /// One-line description of the call.
    pub summary: String,
}

/// The user's answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Run this call only.
    AllowOnce,
    /// Run this call and every later call of the same tool this session.
    AllowAlways,
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

/// The approval policy of a session.
pub struct Approvals {
    /// Whether to allow everything without asking.
    yolo: bool,
    /// Tools the user allowed for the rest of the session.
    always: Mutex<HashSet<String>>,
    /// Where questions go.
    prompter: Arc<dyn Prompter>,
}

impl Approvals {
    /// Creates a policy that asks `prompter`, or never asks when `yolo` is set.
    pub fn new(prompter: Arc<dyn Prompter>, yolo: bool) -> Self {
        Self {
            yolo,
            always: Mutex::new(HashSet::new()),
            prompter,
        }
    }

    /// Returns whether the call described by `request` may run.
    ///
    /// Skips the question in yolo mode and for tools already allowed for the
    /// session; remembers [`Decision::AllowAlways`] answers.
    pub async fn allows(&self, request: &ApprovalRequest) -> bool {
        if self.yolo || self.always.lock().unwrap().contains(&request.tool) {
            return true;
        }
        match self.prompter.ask(request).await {
            Decision::AllowOnce => true,
            Decision::AllowAlways => {
                self.always.lock().unwrap().insert(request.tool.clone());
                true
            }
            Decision::Deny => false,
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

    /// Builds a request for `tool`.
    fn request(tool: &str) -> ApprovalRequest {
        ApprovalRequest {
            tool: tool.into(),
            summary: String::new(),
        }
    }

    /// Allow-once asks every time and deny blocks.
    #[tokio::test]
    async fn once_asks_each_time_and_deny_blocks() {
        let prompter = Scripted::new(&[Decision::AllowOnce, Decision::Deny]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&request("bash")).await);
        assert!(!approvals.allows(&request("bash")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 2);
    }

    /// Allow-always is remembered per tool for the session.
    #[tokio::test]
    async fn always_is_remembered_per_tool() {
        let prompter = Scripted::new(&[Decision::AllowAlways]);
        let approvals = Approvals::new(prompter.clone(), false);
        assert!(approvals.allows(&request("bash")).await);
        assert!(approvals.allows(&request("bash")).await);
        assert_eq!(*prompter.asked.lock().unwrap(), 1);
        assert!(!approvals.allows(&request("edit")).await);
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
