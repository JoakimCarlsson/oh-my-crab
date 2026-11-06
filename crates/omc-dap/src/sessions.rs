//! The debugging sessions of one conversation, which outlive the tool calls that use them.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::time::Instant;

use crate::session::Session;

/// The most sessions a conversation has open at once.
pub(crate) const MOST_SESSIONS: usize = 4;

/// How long closing every session may take in all, before those left are killed.
const CLOSE_WAIT: Duration = Duration::from_secs(3);

/// A session, locked while an action runs on it so that calls made at once take turns.
pub(crate) type Shared = Arc<tokio::sync::Mutex<Session>>;

/// The open sessions and how the next one is named.
#[derive(Default)]
struct Store {
    /// The number the next session's handle gets.
    next: u64,
    /// The sessions, by handle.
    open: BTreeMap<String, Shared>,
    /// The handle of the session started or used last, which an action naming none is about.
    last: Option<String>,
    /// How many sessions are being started.
    starting: usize,
}

/// The debugging sessions of a conversation.
///
/// Sessions belong to the conversation, not to the turn that started them: they go on when a turn is cancelled, so that the model can go on debugging in the next, and they are ended when the conversation closes or another is opened, or when this is dropped.
#[derive(Default)]
pub struct Sessions {
    /// The open sessions.
    store: Mutex<Store>,
}

impl Sessions {
    /// A conversation's sessions, none open.
    pub fn new() -> Self {
        Self::default()
    }

    /// Locks the store, going on with it when a panic poisoned it.
    fn lock(&self) -> MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Reserves a handle for a session about to start.
    ///
    /// # Errors
    ///
    /// Returns why when as many sessions as allowed are open.
    pub(crate) fn reserve(&self) -> Result<String, String> {
        let mut store = self.lock();
        if store.open.len() + store.starting >= MOST_SESSIONS {
            let open: Vec<&String> = store.open.keys().collect();
            return Err(format!(
                "{MOST_SESSIONS} debugging sessions are open ({}); disconnect one first",
                open.iter()
                    .map(|id| id.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        store.next += 1;
        store.starting += 1;
        Ok(format!("dbg-{}", store.next))
    }

    /// Records the session started under a reserved handle, or that it failed to start when there is none.
    pub(crate) fn admit(&self, session: Option<Session>) {
        let mut store = self.lock();
        store.starting = store.starting.saturating_sub(1);
        if let Some(session) = session {
            let id = session.id.clone();
            store
                .open
                .insert(id.clone(), Arc::new(tokio::sync::Mutex::new(session)));
            store.last = Some(id);
        }
    }

    /// The session called `id`, or the one used last when `id` is not given; it becomes the one used last.
    ///
    /// # Errors
    ///
    /// Returns why when there is no such session, naming the open ones.
    pub(crate) fn get(&self, id: Option<&str>) -> Result<Shared, String> {
        let mut store = self.lock();
        let wanted = match id {
            Some(id) => id.to_owned(),
            None => store
                .last
                .clone()
                .filter(|last| store.open.contains_key(last))
                .or_else(|| store.open.keys().last().cloned())
                .ok_or_else(|| {
                    "no debugging session is open; start one with action launch or attach"
                        .to_owned()
                })?,
        };
        let session = store.open.get(&wanted).cloned().ok_or_else(|| {
            let open: Vec<&str> = store.open.keys().map(String::as_str).collect();
            if open.is_empty() {
                format!("there is no session {wanted}; none is open")
            } else {
                format!("there is no session {wanted}; open: {}", open.join(", "))
            }
        })?;
        store.last = Some(wanted);
        Ok(session)
    }

    /// Takes the session called `id` out, so that it ends once the last action on it is done.
    pub(crate) fn remove(&self, id: &str) {
        let mut store = self.lock();
        store.open.remove(id);
        if store.last.as_deref() == Some(id) {
            store.last = None;
        }
    }

    /// The open sessions.
    pub(crate) fn all(&self) -> Vec<Shared> {
        self.lock().open.values().cloned().collect()
    }

    /// How many sessions are open.
    pub fn len(&self) -> usize {
        self.lock().open.len()
    }

    /// Whether no session is open.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Ends every session at once: each adapter and every program a session launched is killed, without telling the adapters.
    pub fn stop_all(&self) {
        let open = std::mem::take(&mut self.lock().open);
        drop(open);
    }

    /// Ends every session cleanly where it can: each adapter is told to disconnect, which ends the programs the sessions launched and leaves the ones they attached to running, and whatever has not ended within a few seconds is killed.
    pub async fn close(&self) {
        let open = std::mem::take(&mut self.lock().open);
        let deadline = Instant::now() + CLOSE_WAIT;
        for session in open.into_values() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = tokio::time::timeout(left, async {
                session.lock().await.disconnect(None, left).await;
            })
            .await;
        }
    }
}
