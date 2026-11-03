//! Helpers shared by the unit tests.

use std::path::PathBuf;

/// Creates an empty directory named `name` under the workspace's git-ignored
/// `build/tests/omc-tools` and returns its path.
pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../build/tests/omc-tools")
        .join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

/// A [`crate::Host`] that records what it is shown and answers as it is told.
#[derive(Default)]
pub struct RecordingHost {
    /// The plans shown, in order.
    pub plans: std::sync::Mutex<Vec<Vec<omc_wire::PlanStep>>>,
    /// The forms put, in order.
    pub asked: std::sync::Mutex<Vec<Vec<omc_wire::Question>>>,
    /// What the reader answers to forms, or nothing to abandon them.
    pub replies: std::sync::Mutex<Option<Vec<omc_wire::Reply>>>,
    /// The plans put for approval, in order.
    pub plans_proposed: std::sync::Mutex<Vec<String>>,
    /// The name the agent gave the conversation.
    pub named: std::sync::Mutex<Option<String>>,
    /// Whether the reader approves plans.
    pub approve: std::sync::Mutex<bool>,
}

#[async_trait::async_trait]
impl crate::Host for RecordingHost {
    /// Records `steps`.
    fn plan(&self, steps: Vec<omc_wire::PlanStep>) {
        self.plans.lock().unwrap().push(steps);
    }

    /// Records `questions` and answers with the scripted replies.
    async fn inquire(&self, questions: Vec<omc_wire::Question>) -> Option<Vec<omc_wire::Reply>> {
        self.asked.lock().unwrap().push(questions);
        self.replies.lock().unwrap().clone()
    }

    /// Records the name when none was recorded yet.
    fn name(&self, title: &str) -> bool {
        let mut named = self.named.lock().unwrap();
        if named.is_some() {
            return false;
        }
        *named = Some(title.to_owned());
        true
    }

    /// Records `plan` and answers with the scripted verdict.
    async fn approve_plan(&self, plan: &str) -> bool {
        self.plans_proposed.lock().unwrap().push(plan.to_owned());
        *self.approve.lock().unwrap()
    }
}
