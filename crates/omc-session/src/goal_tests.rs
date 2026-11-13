//! Tests for goal mode: reading `/goal`, counting the budget, and when crab goes on, stops or wraps up.

use omc_providers::{Reply, ToolCall, ToolResult};

use super::*;

/// A reply that calls `tool` with `arguments`.
fn call(tool: &str, arguments: Value) -> Message {
    Message::Assistant(Reply {
        tool_calls: vec![ToolCall {
            id: "c".into(),
            name: tool.into(),
            arguments,
        }],
        ..Reply::default()
    })
}

/// The result of `tool`, saying `text`.
fn result(tool: &str, text: &str) -> Message {
    Message::ToolResults(vec![ToolResult {
        call_id: "c".into(),
        name: tool.into(),
        text: text.into(),
        is_error: false,
        images: Vec::new(),
    }])
}

/// What a request that cost `input` and `output` tokens, and read `cached` from the cache, counted.
fn spent(input: u64, output: u64, cached: u64) -> Tokens {
    Tokens {
        input,
        output,
        cache_read: cached,
        cache_write: 0,
    }
}

/// Counts are digits with `k` or `m` after them, or a bare number from a thousand; anything else is not a count.
#[test]
fn reads_counts_of_tokens() {
    assert_eq!(tokens("500k"), Some(500_000));
    assert_eq!(tokens("2M"), Some(2_000_000));
    assert_eq!(tokens("1.5m"), Some(1_500_000));
    assert_eq!(tokens("1200000"), Some(1_200_000));
    assert_eq!(tokens("1_000"), Some(1_000));
    assert_eq!(tokens("3"), None);
    assert_eq!(tokens("k"), None);
    assert_eq!(tokens("fix"), None);
}

/// `/goal` reads as showing, setting with or without a budget, pausing, resuming, clearing and budgeting; other commands are not goal commands.
#[test]
fn reads_goal_commands() {
    let read = |text: &str| request(text).map(|asked| asked.unwrap());
    assert_eq!(read("/goal"), Some(Asked::Show));
    assert_eq!(read("  /goal  pause "), Some(Asked::Pause));
    assert_eq!(read("/goal resume"), Some(Asked::Resume));
    assert_eq!(read("/goal clear"), Some(Asked::Clear));
    assert_eq!(read("/goal budget off"), Some(Asked::Budget(None)));
    assert_eq!(
        read("/goal budget 2m"),
        Some(Asked::Budget(Some(2_000_000)))
    );
    assert_eq!(
        read("/goal 500k make every test pass"),
        Some(Asked::Set("make every test pass".into(), Some(500_000)))
    );
    assert_eq!(
        read("/goal 3 failing tests fixed"),
        Some(Asked::Set("3 failing tests fixed".into(), None))
    );
    assert_eq!(
        read("/goal pause the build and fix it"),
        Some(Asked::Set("pause the build and fix it".into(), None))
    );
    assert!(request("/goal budget lots").unwrap().is_err());
    assert_eq!(read("/goals"), None);
    assert_eq!(read("/compact"), None);
    assert_eq!(read("goal"), None);
}

/// The same tool calls with the same results come to the same digest, a different result to another, and no calls to none.
#[test]
fn digests_follow_the_tool_calls_and_results() {
    let run = |output: &str| {
        vec![
            call("bash", json!({ "command": "cargo test" })),
            result("bash", output),
        ]
    };
    assert_eq!(digest(&run("1 failed")), digest(&run("1 failed")));
    assert_ne!(digest(&run("1 failed")), digest(&run("all passed")));
    assert_eq!(digest(&[Message::User("hi".into())]), None);
}

/// A turn's own messages are those after where the history stood, or after the last compaction when one made it shorter.
#[test]
fn finds_what_a_turn_added() {
    let before = vec![Message::User("a".into()), Message::User("b".into())];
    let mut after = before.clone();
    after.push(Message::User("c".into()));
    assert_eq!(added(2, &after).len(), 1);
    assert_eq!(added(5, &after).len(), 3);
}

/// While the goal is active crab goes on after every turn, and pauses once two turns it started in a row did the same, or nothing; the reader speaking judges the next turns afresh.
#[test]
fn goes_on_until_two_turns_make_no_progress() {
    let goals = Goals::default();
    goals.set("make the tests pass", None);
    assert!(matches!(goals.after_turn(Some(1)), Next::Continue(_)));
    assert!(matches!(goals.after_turn(Some(2)), Next::Continue(_)));
    assert!(matches!(goals.after_turn(Some(3)), Next::Continue(_)));
    let Next::Halt(goal) = goals.after_turn(Some(3)) else {
        panic!("the same calls twice should stop it");
    };
    assert_eq!(goal.state, GoalState::Paused);
    assert_eq!(goal.note.as_deref(), Some(NO_PROGRESS));
    assert_eq!(goals.after_turn(Some(4)), Next::Rest);

    goals.resume().unwrap();
    assert!(matches!(goals.after_turn(None), Next::Continue(_)));
    assert!(matches!(goals.after_turn(None), Next::Halt(_)));

    goals.resume().unwrap();
    assert!(matches!(goals.after_turn(Some(5)), Next::Continue(_)));
    goals.reader_spoke();
    assert!(matches!(goals.after_turn(Some(5)), Next::Continue(_)));
    assert!(matches!(goals.after_turn(Some(6)), Next::Continue(_)));
}

/// Only input, output and cache writes count; once the budget is used up the agent is told to wrap up once, and nothing more is started.
#[test]
fn the_budget_ends_in_one_wrap_up() {
    let goals = Goals::default();
    goals.set("ship it", Some(1_000));
    assert_eq!(goals.meter(&spent(400, 100, 90_000)), None);
    assert_eq!(goals.current().unwrap().used, 500);
    let limited = goals.meter(&spent(400, 200, 0)).unwrap();
    assert_eq!(limited.state, GoalState::BudgetLimited);
    assert_eq!(limited.used, 1_100);
    let Next::Continue(wrap) = goals.after_turn(Some(1)) else {
        panic!("a wrap-up turn should follow");
    };
    assert!(wrap.contains("Start no new work"));
    assert_eq!(goals.take_wrap_up(), None);
    assert_eq!(goals.after_turn(Some(2)), Next::Rest);
    assert!(goals.resume().is_err());
    assert_eq!(
        goals.rebudget(Some(5_000)).unwrap().state,
        GoalState::Paused
    );
    assert_eq!(goals.resume().unwrap().state, GoalState::Active);
}

/// When the budget runs out mid-turn the wrap-up is put into that turn, and no turn is started for it afterwards.
#[test]
fn a_wrap_up_put_into_the_turn_is_not_repeated() {
    let goals = Goals::default();
    goals.set("ship it", Some(1_000));
    goals.meter(&spent(2_000, 0, 0)).unwrap();
    assert!(goals.take_wrap_up().is_some());
    assert_eq!(goals.take_wrap_up(), None);
    assert_eq!(goals.after_turn(Some(1)), Next::Rest);
}

/// Only completing ends the goal; nothing is counted or started after it, and nothing is counted while it is paused.
#[test]
fn completing_ends_the_goal() {
    let goals = Goals::default();
    assert!(goals.complete("done").is_err());
    goals.set("ship it", None);
    assert!(goals.reminder().unwrap().contains("ship it"));
    let done = goals.complete("tests pass").unwrap();
    assert_eq!(done.state, GoalState::Complete);
    assert_eq!(done.note.as_deref(), Some("tests pass"));
    assert_eq!(goals.reminder(), None);
    assert_eq!(goals.after_turn(Some(1)), Next::Rest);
    assert!(goals.complete("again").is_err());
    goals.set("next", None);
    goals.pause(STOPPED).unwrap();
    goals.meter(&spent(5_000, 0, 0));
    assert_eq!(goals.current().unwrap().used, 0);
}

/// A goal saved active comes back paused, a cleared one not at all, and clearing forgets it.
#[test]
fn a_saved_goal_comes_back_paused() {
    let goals = Goals::default();
    let saved = Goal {
        objective: "ship it".into(),
        state: GoalState::Active,
        budget: None,
        used: 10,
        note: None,
    };
    let back = goals.restore(Some(saved.clone())).unwrap();
    assert_eq!(back.state, GoalState::Paused);
    assert_eq!(back.used, 10);
    assert_eq!(
        goals.restore(Some(Goal {
            state: GoalState::Cleared,
            ..saved
        })),
        None
    );
    goals.set("x", None);
    assert_eq!(goals.clear().unwrap().state, GoalState::Cleared);
    assert_eq!(goals.current(), None);
    assert_eq!(goals.clear(), None);
}

/// `/guided-goal` reads as asking the agent to work out the goal, with or without an idea to start from.
#[test]
fn reads_guided_goal_commands() {
    let read = |text: &str| request(text).map(|asked| asked.unwrap());
    assert_eq!(read("/guided-goal"), Some(Asked::Guide(String::new())));
    assert_eq!(
        read("/guided-goal  get CI green "),
        Some(Asked::Guide("get CI green".into()))
    );
    assert_eq!(read("/guided"), None);
}

/// The agent creates a goal only after the reader asked it to work one out; asking pauses a goal that was active, and creating sets the new one active, with its budget.
#[test]
fn the_agent_creates_a_goal_only_when_asked() {
    let goals = Goals::default();
    assert!(goals.create("ship it", None).is_err());
    goals.set("old", None);
    let paused = goals.begin_interview().unwrap();
    assert_eq!(paused.state, GoalState::Paused);
    assert_eq!(goals.after_turn(None), Next::Rest);
    let created = goals
        .create("## Objective\nship it", Some(800_000))
        .unwrap();
    assert_eq!(created.state, GoalState::Active);
    assert_eq!(created.budget, Some(800_000));
    assert!(matches!(goals.after_turn(None), Next::Continue(_)));
    assert!(goals.create("again", None).is_err());
}
