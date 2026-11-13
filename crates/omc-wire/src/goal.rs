//! A goal the agent keeps working toward across turns, and how it stands.

/// How a goal stands.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GoalState {
    /// The agent works toward it, and the harness starts a turn of its own after each turn that ends with the goal open.
    Active,
    /// It is kept but not worked toward: the reader paused it, stopped a turn, a turn failed, or two turns in a row made no progress; `/goal resume` makes it active again.
    Paused,
    /// It used up its token budget; the agent was told to wrap up and no further turns are started for it.
    BudgetLimited,
    /// The agent said it is done.
    Complete,
    /// The reader cleared it; nothing of it is kept.
    Cleared,
}

/// A goal and how it stands, sent whenever either changes.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Goal {
    /// What the agent works toward.
    pub objective: String,
    /// How it stands.
    pub state: GoalState,
    /// The most tokens the goal may use, when it has a budget.
    #[serde(default)]
    pub budget: Option<u64>,
    /// The tokens used toward it so far: input, output and cache writes of every request made while it was active, subagents included; cache reads are not counted.
    #[serde(default)]
    pub used: u64,
    /// Why it stands as it does, when the harness has something to say: why it paused, or the agent's summary when it completed.
    #[serde(default)]
    pub note: Option<String>,
}

#[cfg(test)]
/// Tests for the goal's events.
mod tests {
    use crate::{Event, Goal, GoalState, decode, encode};

    /// A goal event survives the wire, and one without a budget, usage or note still reads.
    #[test]
    fn a_goal_event_reads_back() {
        let event = Event::Goal(Goal {
            objective: "make the tests pass".into(),
            state: GoalState::Paused,
            budget: Some(500_000),
            used: 1200,
            note: Some("no progress".into()),
        });
        assert_eq!(decode::<Event>(&encode(&event).unwrap()).unwrap(), event);
        let bare: Event = decode(r#"{"Goal":{"objective":"x","state":"Active"}}"#).unwrap();
        assert_eq!(
            bare,
            Event::Goal(Goal {
                objective: "x".into(),
                state: GoalState::Active,
                budget: None,
                used: 0,
                note: None,
            })
        );
    }
}
