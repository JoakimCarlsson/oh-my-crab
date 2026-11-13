//! How deep subagents may start subagents of their own, and how many run at once across every level.
//!
//! A subagent below the deepest level allowed gets a `task` tool of its own, which starts its subagents one level further down. However deep they go, they all take a seat from one [`Throttle`], shared by the conversation, so the number running at once stays bounded instead of multiplying with each level. A subagent waiting for its own subagents gives its seat up while they run and takes one again when they end, so a tree of waiting subagents can never hold every seat and stall.

use std::sync::{Arc, Mutex};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::Roster;

/// The environment variable that sets how many levels of subagents may run below the main agent.
pub const DEPTH_VARIABLE: &str = "OMC_SUBAGENT_DEPTH";

/// How many levels of subagents may run below the main agent when [`DEPTH_VARIABLE`] does not say: the main agent's subagents and theirs.
pub const DEFAULT_DEPTH: usize = 2;

/// The most levels [`DEPTH_VARIABLE`] may ask for; a larger number is cut to it.
pub const MOST_DEPTH: usize = 8;

/// How many levels of subagents may run below the main agent, as [`DEPTH_VARIABLE`] says: a whole number from 1, where 1 lets no subagent start subagents, to [`MOST_DEPTH`]; anything else, or nothing, gives [`DEFAULT_DEPTH`].
pub fn max_depth() -> usize {
    depth_from(std::env::var(DEPTH_VARIABLE).ok().as_deref())
}

/// The depth `value`, the text of [`DEPTH_VARIABLE`], asks for, as [`max_depth`] reads it.
pub(crate) fn depth_from(value: Option<&str>) -> usize {
    value
        .and_then(|text| text.trim().parse::<usize>().ok())
        .filter(|depth| *depth >= 1)
        .map_or(DEFAULT_DEPTH, |depth| depth.min(MOST_DEPTH))
}

/// The seats subagents take to run, shared by every level of a conversation, so that only so many run at once.
pub struct Throttle {
    /// One permit per seat.
    seats: Arc<Semaphore>,
}

impl Throttle {
    /// A throttle letting `seats` subagents run at once; at least one.
    pub fn new(seats: usize) -> Self {
        Self {
            seats: Arc::new(Semaphore::new(seats.max(1))),
        }
    }

    /// How many seats are free now.
    pub fn free(&self) -> usize {
        self.seats.available_permits()
    }

    /// Waits for a free seat and takes it; nothing only when the throttle was closed, which it never is.
    pub(crate) async fn take(&self) -> Option<OwnedSemaphorePermit> {
        self.seats.clone().acquire_owned().await.ok()
    }
}

impl Default for Throttle {
    /// A throttle with [`crate::MAX_CONCURRENT`] seats.
    fn default() -> Self {
        Self::new(crate::MAX_CONCURRENT)
    }
}

/// The seat a running subagent holds, which it gives up while its own subagents run.
#[derive(Default)]
pub(crate) struct Seat {
    /// The permit, while the subagent holds one.
    held: Mutex<Option<OwnedSemaphorePermit>>,
}

impl Seat {
    /// Holds `permit` as the subagent's seat.
    pub(crate) fn fill(&self, permit: Option<OwnedSemaphorePermit>) {
        *self.held.lock().unwrap_or_else(|held| held.into_inner()) = permit;
    }

    /// Gives the seat up, freeing it for others.
    pub(crate) fn vacate(&self) {
        self.fill(None);
    }
}

/// Where a `task` tool stands among the levels of subagents.
#[derive(Clone)]
pub struct Nesting {
    /// The level the subagents this tool starts run at: 1 for the main agent's tool, 2 for that of its subagents, and so on.
    pub depth: usize,
    /// How many levels of subagents may run below the main agent; subagents at this level get no `task` tool.
    pub most: usize,
    /// The seats every subagent of the conversation takes to run.
    pub throttle: Arc<Throttle>,
    /// The seat of the subagent this tool belongs to, given up while the subagents it starts run; nothing for the main agent's tool.
    pub(crate) seat: Option<Arc<Seat>>,
    /// The id of the subagent this tool belongs to, which the ids of the subagents it starts are put under; empty for the main agent's tool.
    pub(crate) scope: String,
    /// The handle of the subagent this tool belongs to, which the finished subagents it started are kept under for follow-ups; empty for the main agent's tool.
    pub(crate) keeper: String,
    /// The subagents of the conversation, at every level, by which the reader stops a running one and a follow-up reaches a finished one.
    pub roster: Arc<Roster>,
}

impl Nesting {
    /// Where the main agent's `task` tool stands: its subagents at level 1, `most` levels allowed, seats taken from `throttle`, and a roster of its own.
    pub fn new(most: usize, throttle: Arc<Throttle>) -> Self {
        Self {
            depth: 1,
            most: most.max(1),
            throttle,
            seat: None,
            scope: String::new(),
            keeper: String::new(),
            roster: Arc::new(Roster::default()),
        }
    }

    /// The same, with its subagents put on `roster`, which the conversation shares.
    #[must_use]
    pub fn with_roster(self, roster: Arc<Roster>) -> Self {
        Self { roster, ..self }
    }

    /// Whether the subagents this tool starts may start subagents of their own.
    pub(crate) fn nests(&self) -> bool {
        self.depth < self.most
    }

    /// Where the `task` tool of a subagent this tool starts stands: one level further down, holding `seat`, its subagents put under `id` and, once they finished, kept under `handle`.
    pub(crate) fn below(&self, seat: Arc<Seat>, id: &str, handle: &str) -> Self {
        Self {
            depth: self.depth + 1,
            most: self.most,
            throttle: self.throttle.clone(),
            seat: Some(seat),
            scope: id.to_owned(),
            keeper: handle.to_owned(),
            roster: self.roster.clone(),
        }
    }

    /// The id of the subagent started by the call `call`, at `place` in its list when it is one of several, under the subagent this tool belongs to.
    pub(crate) fn id_of(&self, call: &str, place: Option<usize>) -> String {
        let own = match place {
            Some(place) => format!("{call}/{place}"),
            None => call.to_owned(),
        };
        if self.scope.is_empty() {
            own
        } else {
            format!("{}:{own}", self.scope)
        }
    }

    /// Runs `work`, the subagents this tool started, with the seat of the subagent this tool belongs to given up meanwhile, and takes a seat again once it is done.
    pub(crate) async fn lending<F: std::future::Future>(&self, work: F) -> F::Output {
        let Some(seat) = &self.seat else {
            return work.await;
        };
        seat.vacate();
        let output = work.await;
        seat.fill(self.throttle.take().await);
        output
    }
}

impl Default for Nesting {
    /// Where a main agent's `task` tool stands with [`DEFAULT_DEPTH`] levels and a throttle of its own.
    fn default() -> Self {
        Self::new(DEFAULT_DEPTH, Arc::new(Throttle::default()))
    }
}

#[cfg(test)]
/// Tests for the depth and the seats of subagents.
mod tests {
    use super::*;

    /// The variable gives a depth from 1 up, cut at the most; anything else gives the default.
    #[test]
    fn reads_the_depth() {
        assert_eq!(depth_from(None), DEFAULT_DEPTH);
        assert_eq!(depth_from(Some("3")), 3);
        assert_eq!(depth_from(Some(" 1 ")), 1);
        assert_eq!(depth_from(Some("0")), DEFAULT_DEPTH);
        assert_eq!(depth_from(Some("lots")), DEFAULT_DEPTH);
        assert_eq!(depth_from(Some("99")), MOST_DEPTH);
    }

    /// Each level below allows nesting until the deepest, and ids are put under the subagent the tool belongs to.
    #[test]
    fn levels_go_down_to_the_most() {
        let top = Nesting::new(2, Arc::new(Throttle::new(2)));
        assert!(top.nests());
        assert_eq!(top.id_of("t1", Some(2)), "t1/2");
        let below = top.below(Arc::new(Seat::default()), "t1/2", "sub-1");
        assert_eq!(below.depth, 2);
        assert!(!below.nests());
        assert_eq!(below.id_of("s1", None), "t1/2:s1");
    }

    /// A subagent waiting for its own subagents gives its seat up meanwhile and takes one back afterwards.
    #[tokio::test]
    async fn a_waiting_subagent_lends_its_seat() {
        let throttle = Arc::new(Throttle::new(1));
        let seat = Arc::new(Seat::default());
        seat.fill(throttle.take().await);
        assert_eq!(throttle.free(), 0);
        let nesting = Nesting::new(2, throttle.clone()).below(seat, "t1", "sub-1");
        let inside = nesting
            .lending(async {
                let child = throttle.take().await;
                let free = throttle.free();
                drop(child);
                free
            })
            .await;
        assert_eq!(inside, 0);
        assert_eq!(throttle.free(), 0);
    }
}
