//! The hooks that run around a conversation rather than inside one call: when it opens, resumes or has been compacted, when it ends, and when it waits on the reader.
//!
//! The context `SessionStart` hooks give reaches the model as a notice at the start of the next prompt, like the other context that changes during a conversation, so the system prompt and tool list stay byte for byte the same and the provider's prompt cache keeps them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use omc_hooks::{Engine, HookEvent};
use tokio::sync::watch;

/// How long crab waits after a turn ends, with nothing else to do, before it tells the `Notification` hooks it is idle.
pub(crate) const IDLE_AFTER: Duration = Duration::from_secs(60);

/// The tag the context of `SessionStart` hooks is told to the model in.
const START_TAG: &str = "session-start-hook";

/// The context of a run of `SessionStart` hooks: nothing while they run, then what they added.
pub(crate) type Starting = watch::Receiver<Option<Option<String>>>;

/// The hooks of a conversation as they stand now, and the context of the last `SessionStart` hooks that the model has not been told yet.
#[derive(Default)]
pub(crate) struct Lifecycle {
    /// The hooks that run now; they change when the project's trust or its hook files do.
    engine: Mutex<Option<Arc<Engine>>>,
    /// The last run of `SessionStart` hooks, until a turn that told the model its context ended.
    starting: Mutex<Option<Starting>>,
}

impl Lifecycle {
    /// Makes `engine` the hooks that run from now on; nothing for none.
    pub(crate) fn set_engine(&self, engine: Option<Arc<Engine>>) {
        *self.engine.lock().unwrap_or_else(|held| held.into_inner()) = engine;
    }

    /// The hooks that run now, when there are any.
    pub(crate) fn engine(&self) -> Option<Arc<Engine>> {
        self.engine
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// The hooks that run now, when any of them runs at `event`.
    fn engine_for(&self, event: HookEvent) -> Option<Arc<Engine>> {
        self.engine().filter(|engine| engine.has(event))
    }

    /// Starts the `SessionStart` hooks for `source` (`startup`, `resume` or `compact`), told the `model`, on a task of their own, and keeps their context for the next prompt in place of any earlier one; does nothing when no hook runs at that event or there is no runtime to run them on.
    pub(crate) fn begin(&self, source: &'static str, model: Option<String>) {
        let Some(engine) = self.engine_for(HookEvent::SessionStart) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (done, starting) = watch::channel(None);
        runtime.spawn(async move {
            let context = engine.session_start(source, model.as_deref()).await;
            let _ = done.send(Some(context));
        });
        *self
            .starting
            .lock()
            .unwrap_or_else(|held| held.into_inner()) = Some(starting);
    }

    /// The run of `SessionStart` hooks whose context the model has not been told yet, when there is one.
    pub(crate) fn pending(&self) -> Option<Starting> {
        self.starting
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone()
    }

    /// Forgets `told`, a run of `SessionStart` hooks whose context a turn that ended told the model, unless a later run took its place meanwhile.
    pub(crate) fn settle(&self, told: &Starting) {
        let mut starting = self
            .starting
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        if starting
            .as_ref()
            .is_some_and(|kept| kept.same_channel(told))
        {
            *starting = None;
        }
    }

    /// Forgets the run of `SessionStart` hooks waiting to be told, as when another conversation is opened.
    pub(crate) fn forget_start(&self) {
        *self
            .starting
            .lock()
            .unwrap_or_else(|held| held.into_inner()) = None;
    }

    /// Tells the `Notification` hooks, on a task of their own, that crab waits on the reader: `kind` is the kind of notification, `title` and `message` what it says. Does nothing when no hook runs at that event.
    pub(crate) fn notify(&self, kind: &'static str, title: &str, message: &str) {
        let Some(engine) = self.engine_for(HookEvent::Notification) else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (title, message) = (title.to_owned(), message.to_owned());
        runtime.spawn(async move { engine.notification(kind, &title, &message).await });
    }

    /// Runs the `SessionEnd` hooks of the hooks that run now for `reason`, and waits for them; each may run only as long as its timeout allows.
    pub(crate) async fn end(&self, reason: &str) {
        if let Some(engine) = self.engine_for(HookEvent::SessionEnd) {
            engine.session_end(reason).await;
        }
    }
}

/// The context `starting` gives, waiting for its hooks to end; nothing when they added none or could not finish.
pub(crate) async fn context_of(mut starting: Starting) -> Option<String> {
    match starting.wait_for(Option::is_some).await {
        Ok(done) => done.clone().flatten(),
        Err(_) => None,
    }
}

/// The notice that tells the model `context`, which the `SessionStart` hooks added.
pub(crate) fn start_notice(context: &str) -> String {
    format!("<{START_TAG}>\n{}\n</{START_TAG}>", context.trim())
}

#[cfg(all(test, unix))]
/// Tests for the hooks around a conversation.
mod tests {
    use std::path::PathBuf;

    use omc_hooks::{Hook, HookOrigin, Setting};

    use super::*;

    /// An engine running `command` at `event` in `folder`.
    fn engine(folder: &std::path::Path, event: HookEvent, command: &str) -> Arc<Engine> {
        Arc::new(Engine::new(
            vec![Hook {
                event,
                matcher: String::new(),
                command: command.to_owned(),
                timeout: Some(Duration::from_secs(10)),
                file: PathBuf::from("/h.json"),
                origin: HookOrigin::User,
                claude: false,
                plugin: None,
            }],
            Setting {
                session: None,
                folder: folder.to_path_buf(),
                mode: Arc::new(|| "default"),
            },
        ))
    }

    /// A scratch folder named `name` under the workspace's `build/tests`.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The context of a start waits for its hooks, stays until a turn that told it is settled, and a later start takes the place of an earlier one even when the earlier one is settled after it.
    #[tokio::test]
    async fn a_start_is_kept_until_told() {
        let folder = scratch("lifecycle-start");
        let lifecycle = Lifecycle::default();
        lifecycle.begin("startup", None);
        assert!(lifecycle.pending().is_none());
        lifecycle.set_engine(Some(engine(
            &folder,
            HookEvent::SessionStart,
            "sleep 0.2; echo the secret is kiwi",
        )));
        lifecycle.begin("startup", None);
        let first = lifecycle.pending().unwrap();
        assert_eq!(
            context_of(first.clone()).await.as_deref(),
            Some("the secret is kiwi")
        );
        assert!(lifecycle.pending().is_some());
        lifecycle.begin("compact", None);
        lifecycle.settle(&first);
        let second = lifecycle.pending().unwrap();
        assert!(!second.same_channel(&first));
        lifecycle.settle(&second);
        assert!(lifecycle.pending().is_none());
        assert_eq!(
            start_notice("  kiwi \n"),
            "<session-start-hook>\nkiwi\n</session-start-hook>"
        );
    }

    /// The end runs its hooks and waits for them; a notification runs on a task of its own.
    #[tokio::test]
    async fn an_end_and_a_notification_run() {
        let folder = scratch("lifecycle-end");
        let lifecycle = Lifecycle::default();
        lifecycle.set_engine(Some(engine(
            &folder,
            HookEvent::SessionEnd,
            "cat > end.json",
        )));
        lifecycle.end("other").await;
        let end = std::fs::read_to_string(folder.join("end.json")).unwrap();
        assert!(end.contains(r#""reason":"other""#), "{end}");
        lifecycle.set_engine(Some(engine(
            &folder,
            HookEvent::Notification,
            "cat > note.json",
        )));
        lifecycle.notify("idle_prompt", "", "crab is waiting for your input");
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let mut note = String::new();
        while !note.ends_with('}') && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
            note = std::fs::read_to_string(folder.join("note.json")).unwrap_or_default();
        }
        assert!(
            note.contains(r#""notification_type":"idle_prompt""#),
            "{note}"
        );
    }
}
