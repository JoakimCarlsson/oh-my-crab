//! A small in-memory job queue with leases, retries and a dead-letter list.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

/// How many times a job is tried before it is given up on.
const MAX_ATTEMPTS: u32 = 5;

/// How many finished jobs are remembered.
const HISTORY: usize = 100;

/// What a job is doing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    /// It waits to be taken.
    Waiting,
    /// A worker holds it.
    Leased,
    /// It finished.
    Done,
    /// It failed for good.
    Dead,
}

/// One unit of work.
#[derive(Clone, Debug)]
pub struct Job {
    /// The job's number, unique in its queue.
    pub id: u64,
    /// What the job is about.
    pub payload: String,
    /// How many times it has been tried.
    pub attempts: u32,
    /// What it is doing.
    pub state: State,
    /// When it may next be taken.
    pub ready_at: Instant,
    /// The higher, the sooner it is taken.
    pub priority: u8,
}

/// A lease on a job: who holds it and until when.
#[derive(Clone, Debug)]
pub struct Lease {
    /// The job that is held.
    pub job: u64,
    /// The worker that holds it.
    pub worker: String,
    /// When the lease runs out.
    pub expires: Instant,
}

/// The queue itself.
#[derive(Debug, Default)]
pub struct Queue {
    /// The next job number to give out.
    next: u64,
    /// The jobs that wait, in the order they came.
    waiting: VecDeque<Job>,
    /// The jobs that are held, by job number.
    leased: BTreeMap<u64, (Job, Lease)>,
    /// The jobs that finished, newest last.
    done: VecDeque<Job>,
    /// The jobs that failed for good.
    dead: Vec<Job>,
}

/// How long a worker may hold a job before it is taken back.
pub fn lease_time(priority: u8) -> Duration {
    if priority > 200 {
        Duration::from_secs(10)
    } else {
        Duration::from_secs(30)
    }
}

/// How long to wait before a job that failed `attempts` times is tried again: one second, doubled for each attempt, at most 60 seconds.
pub fn retry_delay(attempts: u32) -> Duration {
    let doubled = Duration::from_secs(1) * 2u32.saturating_pow(attempts.min(16));
    doubled.min(Duration::from_secs(60))
}

/// Whether a job that was tried `attempts` times may be tried again.
pub fn may_retry(attempts: u32) -> bool {
    attempts < MAX_ATTEMPTS
}

impl Queue {
    /// An empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a job with `payload` at `priority` and gives its number.
    pub fn push(&mut self, payload: impl Into<String>, priority: u8) -> u64 {
        self.next += 1;
        self.waiting.push_back(Job {
            id: self.next,
            payload: payload.into(),
            attempts: 0,
            state: State::Waiting,
            ready_at: Instant::now(),
            priority,
        });
        self.next
    }

    /// How many jobs wait.
    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    /// How many jobs are held.
    pub fn leased(&self) -> usize {
        self.leased.len()
    }

    /// How many jobs failed for good.
    pub fn dead(&self) -> usize {
        self.dead.len()
    }

    /// The place in the waiting line of the job to take at `now`: the highest priority among those that are ready, the oldest of them first.
    fn pick(&self, now: Instant) -> Option<usize> {
        let mut best: Option<(usize, u8)> = None;
        for (place, job) in self.waiting.iter().enumerate() {
            if job.ready_at > now {
                continue;
            }
            match best {
                Some((_, priority)) if priority >= job.priority => {}
                _ => best = Some((place, job.priority)),
            }
        }
        best.map(|(place, _)| place)
    }

    /// Gives `worker` the next job that is ready at `now`, when there is one.
    pub fn take(&mut self, worker: &str, now: Instant) -> Option<Job> {
        let place = self.pick(now)?;
        let mut job = self.waiting.remove(place)?;
        job.state = State::Leased;
        job.attempts += 1;
        let lease = Lease {
            job: job.id,
            worker: worker.to_owned(),
            expires: now + lease_time(job.priority),
        };
        self.leased.insert(job.id, (job.clone(), lease));
        Some(job)
    }

    /// Marks the job `id` as finished, when `worker` holds it.
    pub fn finish(&mut self, id: u64, worker: &str) -> bool {
        match self.leased.get(&id) {
            Some((_, lease)) if lease.worker == worker => {}
            _ => return false,
        }
        let Some((mut job, _)) = self.leased.remove(&id) else {
            return false;
        };
        job.state = State::Done;
        self.done.push_back(job);
        while self.done.len() > HISTORY {
            self.done.pop_front();
        }
        true
    }

    /// Puts the job `id` back after it failed at `now`, to be tried again later, or gives up on it when it was tried too often.
    pub fn fail(&mut self, id: u64, now: Instant) -> Option<State> {
        let (mut job, _) = self.leased.remove(&id)?;
        if may_retry(job.attempts) {
            job.state = State::Waiting;
            job.ready_at = now + retry_delay(job.attempts);
            self.waiting.push_back(job);
            Some(State::Waiting)
        } else {
            job.state = State::Dead;
            self.dead.push(job);
            Some(State::Dead)
        }
    }

    /// Takes back every job whose lease ran out by `now`, as if it had failed, and gives how many there were.
    pub fn reap(&mut self, now: Instant) -> usize {
        let expired: Vec<u64> = self
            .leased
            .iter()
            .filter(|(_, (_, lease))| lease.expires <= now)
            .map(|(id, _)| *id)
            .collect();
        for id in &expired {
            self.fail(*id, now);
        }
        expired.len()
    }

    /// Gives the worker that holds the job `id` more time, from `now`.
    pub fn renew(&mut self, id: u64, worker: &str, now: Instant) -> bool {
        match self.leased.get_mut(&id) {
            Some((job, lease)) if lease.worker == worker => {
                lease.expires = now + lease_time(job.priority);
                true
            }
            _ => false,
        }
    }

    /// Puts every job that failed for good back in the waiting line, with its attempts forgotten, and gives how many there were.
    pub fn revive(&mut self, now: Instant) -> usize {
        let revived = self.dead.len();
        for mut job in self.dead.drain(..) {
            job.state = State::Waiting;
            job.attempts = 0;
            job.ready_at = now;
            self.waiting.push_back(job);
        }
        revived
    }

    /// The payloads of the jobs that finished, oldest first.
    pub fn history(&self) -> Vec<&str> {
        self.done.iter().map(|job| job.payload.as_str()).collect()
    }

    /// Drops every waiting job whose payload `unwanted` says yes to, and gives how many were dropped.
    pub fn purge(&mut self, unwanted: impl Fn(&str) -> bool) -> usize {
        let before = self.waiting.len();
        self.waiting.retain(|job| !unwanted(&job.payload));
        before - self.waiting.len()
    }

    /// A line for each state with how many jobs are in it.
    pub fn summary(&self) -> String {
        format!(
            "waiting: {}\nleased: {}\ndone: {}\ndead: {}\n",
            self.waiting.len(),
            self.leased.len(),
            self.done.len(),
            self.dead.len()
        )
    }
}

#[cfg(test)]
/// Tests for the queue.
mod tests {
    use super::*;

    /// The delay doubles with each attempt and stops growing at the cap.
    #[test]
    fn the_delay_doubles_up_to_the_cap() {
        assert_eq!(retry_delay(0), Duration::from_secs(1));
        assert_eq!(retry_delay(3), Duration::from_secs(8));
        assert!(retry_delay(20) >= retry_delay(10));
    }

    /// A higher priority is taken first, and the older of two equals.
    #[test]
    fn the_highest_priority_is_taken_first() {
        let mut queue = Queue::new();
        let low = queue.push("low", 1);
        let high = queue.push("high", 9);
        let now = Instant::now();
        assert_eq!(queue.take("w", now).map(|job| job.id), Some(high));
        assert_eq!(queue.take("w", now).map(|job| job.id), Some(low));
    }

    /// A job that keeps failing ends up dead.
    #[test]
    fn a_job_that_keeps_failing_dies() {
        let mut queue = Queue::new();
        let id = queue.push("flaky", 1);
        let mut now = Instant::now();
        for _ in 0..MAX_ATTEMPTS {
            now += Duration::from_secs(120);
            assert!(queue.take("w", now).is_some());
            queue.fail(id, now);
        }
        assert_eq!(queue.dead(), 1);
    }
}
