//! Keeping the provider's prompt cache warm while the agent waits on a long tool or on the reader.
//!
//! A cache entry lives a few minutes from the start of the last request that read or wrote it. When nothing is sent for longer, the next request writes the whole conversation again at the full write price. Shortly before the entry would expire, the warmer sends the last request again asking for no output, which reads the entry and so keeps it alive for another lifetime, as long as that is expected to cost less than the miss it prevents.

use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use omc_providers::{
    ChatModel, Effort, Message, Pricing, Provider, ProviderError, Reply, Request, Retention, Sink,
    Tokens, ToolSpec,
};
use tokio::task::AbortHandle;
use tokio::time::Instant;

use crate::shared::Shared;

/// The share of the cache lifetime after which the entry is warmed.
const WARM_AT: f64 = 0.9;

/// The least time left before the entry expires when it is warmed.
const MARGIN: Duration = Duration::from_secs(10);

/// How long after the last real request warming goes on while a turn runs.
const RUNNING_HORIZON: Duration = Duration::from_secs(60 * 60);

/// How long after the last real request warming goes on while the conversation waits for the reader.
const IDLE_HORIZON: Duration = Duration::from_secs(30 * 60);

/// The chance that the reader sends something before the entry expires while the conversation waits on them.
const IDLE_CONTINUATION: f64 = 0.15;

/// The least saving, in US dollars, that a warm request has to be expected to bring.
const MINIMUM_SAVING: f64 = 0.02;

/// The key the cost of warming is saved under with the conversation.
const SPENT_FACT: &str = "cache_warming";

/// Which model and cache a request was sent to.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Target {
    /// The provider.
    pub(crate) provider: Provider,
    /// The model.
    pub(crate) model: String,
    /// How long the cache entries the request wrote live.
    pub(crate) retention: Retention,
    /// How hard the model was asked to think.
    pub(crate) effort: Option<Effort>,
    /// Whether the agent had the checkpoint and rewind tools, which changes the tool list.
    pub(crate) checkpoints: bool,
}

/// A request as it was sent, kept to be sent again.
struct Snapshot {
    /// The model it went to.
    model: Arc<dyn ChatModel>,
    /// Where it went.
    target: Target,
    /// The system prompt.
    system: String,
    /// The conversation.
    messages: Vec<Message>,
    /// The tools.
    tools: Vec<ToolSpec>,
    /// How many tokens the prompt held, as the provider counted them.
    prompt_tokens: u64,
    /// When it was sent, which is when the lifetime of the entries it read or wrote began.
    started: Instant,
}

impl Snapshot {
    /// The request again.
    fn request(&self) -> Request<'_> {
        Request {
            system: &self.system,
            messages: &self.messages,
            tools: &self.tools,
            effort: self.target.effort,
        }
    }
}

/// What warming has cost the conversation.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Serialize)]
struct Spent {
    /// How many warm requests were answered.
    warms: u64,
    /// The tokens they were billed for.
    tokens: Tokens,
    /// What they cost, in US dollars.
    dollars: f64,
}

/// What the warmer is doing.
#[derive(Default)]
struct State {
    /// The task that warms the entries of the last request, when there is one.
    task: Option<AbortHandle>,
    /// How many turns are running.
    turns: usize,
    /// What warming has cost so far.
    spent: Spent,
}

/// Keeps the cache entries of a conversation's last request warm.
#[derive(Default)]
pub(crate) struct Warmer {
    /// What it is doing.
    state: Mutex<State>,
}

/// A turn that is running, for as long as this is kept; dropping it, as a cancelled turn does, ends it.
pub(crate) struct Running(Arc<Warmer>);

impl Drop for Running {
    /// Counts the turn as over.
    fn drop(&mut self) {
        self.0
            .with(|state| state.turns = state.turns.saturating_sub(1));
    }
}

impl Warmer {
    /// Runs `with` on the state.
    fn with<T>(&self, with: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self.state.lock().unwrap_or_else(|held| held.into_inner());
        with(&mut state)
    }

    /// Counts a turn as running until what is returned is dropped; while one runs, the next request is all but sure to come.
    pub(crate) fn running(self: &Arc<Self>) -> Running {
        self.with(|state| state.turns += 1);
        Running(self.clone())
    }

    /// Whether a turn is running.
    fn turn_running(&self) -> bool {
        self.with(|state| state.turns > 0)
    }

    /// Whether a request is being kept warm.
    #[cfg(test)]
    pub(crate) fn armed(&self) -> bool {
        self.with(|state| state.task.is_some())
    }

    /// Pretends to keep a request warm, with a task that never ends.
    #[cfg(test)]
    pub(crate) fn arm(&self) {
        let task = tokio::spawn(std::future::pending::<()>()).abort_handle();
        self.with(|state| state.task.replace(task));
    }

    /// Stops warming until the next request.
    pub(crate) fn stop(&self) {
        if let Some(task) = self.with(|state| state.task.take()) {
            task.abort();
        }
    }

    /// Keeps the entries `snapshot` read or wrote warm, for the conversation `shared`, in place of any earlier request's; they live `ttl` and cost what `pricing` says.
    fn start(&self, shared: Weak<Shared>, snapshot: Snapshot, ttl: Duration, pricing: Pricing) {
        let task = tokio::spawn(keep_warm(shared, snapshot, ttl, pricing)).abort_handle();
        if let Some(earlier) = self.with(|state| state.task.replace(task)) {
            earlier.abort();
        }
    }

    /// Adds a warm request that was billed `tokens` costing `dollars` to what warming cost, and returns the new total.
    fn spend(&self, tokens: Tokens, dollars: f64) -> Spent {
        self.with(|state| {
            let spent = &mut state.spent;
            spent.warms += 1;
            spent.tokens.input += tokens.input;
            spent.tokens.output += tokens.output;
            spent.tokens.cache_read += tokens.cache_read;
            spent.tokens.cache_write += tokens.cache_write;
            spent.dollars += dollars;
            *spent
        })
    }
}

impl Drop for Warmer {
    /// Stops warming with the conversation.
    fn drop(&mut self) {
        self.stop();
    }
}

/// When to warm an entry that lives `ttl`: at nine tenths of its life, and at least [`MARGIN`] before it ends; nothing when it lives too short for that.
fn warm_delay(ttl: Duration) -> Option<Duration> {
    let delay = ttl.mul_f64(WARM_AT).min(ttl.checked_sub(MARGIN)?);
    (!delay.is_zero()).then_some(delay)
}

/// What warming the entries of a prompt of `tokens` tokens is expected to save, in US dollars, at `pricing` for entries that live as `retention` says, when the next request comes before they expire with the chance `continuation`.
///
/// A warm request reads the whole prompt from the cache and writes no output; without it the next request writes the whole prompt again instead of reading it.
fn expected_saving(pricing: &Pricing, retention: Retention, tokens: u64, continuation: f64) -> f64 {
    let dollars = |price: f64| price * tokens as f64 / 1_000_000.0;
    let hit = dollars(pricing.cache_read);
    let miss = (dollars(pricing.cache_write_for(retention)) - hit).max(0.0);
    continuation * miss - hit
}

/// Warms the entries of `snapshot` for the conversation `shared`, which live `ttl` and cost what `pricing` says, until warming no longer pays or is no longer wanted.
///
/// Before each warm request it checks that the conversation is still there with warming on and the same model, cache lifetime, effort and checkpoint setting, that the last real request is no older than the horizon (an hour while a turn runs, half an hour while the conversation waits for the reader), that the timer did not fire so late that the entry may be gone, and that the expected saving is at least [`MINIMUM_SAVING`]. A warm request that fails ends warming; one that is answered is saved with the conversation as what warming cost.
async fn keep_warm(shared: Weak<Shared>, snapshot: Snapshot, ttl: Duration, pricing: Pricing) {
    let Some(delay) = warm_delay(ttl) else {
        return;
    };
    let slack = (ttl - delay) / 2;
    let target = &snapshot.target;
    let mut last = snapshot.started;
    loop {
        let due = last + delay;
        tokio::time::sleep_until(due).await;
        let Some(held) = shared.upgrade() else {
            return;
        };
        let running = held.warmer.turn_running();
        let horizon = if running {
            RUNNING_HORIZON
        } else {
            IDLE_HORIZON
        };
        let current = held.warming()
            && held.retention() == target.retention
            && held.model() == Some((target.provider, target.model.clone()))
            && held.effort_now() == target.effort
            && held.checkpoints() == target.checkpoints;
        if !current || due > snapshot.started + horizon || Instant::now() > due + slack {
            return;
        }
        let continuation = if running { 1.0 } else { IDLE_CONTINUATION };
        let saving = expected_saving(
            &pricing,
            target.retention,
            snapshot.prompt_tokens,
            continuation,
        );
        if saving < MINIMUM_SAVING {
            return;
        }
        drop(held);
        let sent = Instant::now();
        let Ok(Some(tokens)) = snapshot.model.warm(snapshot.request()).await else {
            return;
        };
        last = sent;
        let Some(held) = shared.upgrade() else {
            return;
        };
        let spent = held
            .warmer
            .spend(tokens, pricing.cost(&tokens, target.retention));
        held.save_fact(SPENT_FACT, &serde_json::to_value(spent).unwrap_or_default());
    }
}

/// The model a turn's main loop asks: it passes every request on to the model it wraps, and keeps the last one it sent warm.
///
/// Summaries, which write nothing to the cache, and subagents, which ask the wrapped model themselves, are not kept warm.
pub(crate) struct Recorder {
    /// The model requests go to.
    pub(crate) inner: Arc<dyn ChatModel>,
    /// The conversation the turn is in.
    pub(crate) shared: Arc<Shared>,
    /// The provider and model the requests go to.
    pub(crate) provider: Provider,
    /// The model id.
    pub(crate) model: String,
    /// Whether the turn gave the agent the checkpoint and rewind tools.
    pub(crate) checkpoints: bool,
}

impl Recorder {
    /// Keeps the entries of `request`, sent at `started` and answered with a prompt of `prompt_tokens` tokens, warm when warming is on and pays for this model; stops warming the earlier request's otherwise.
    fn keep(&self, request: &Request<'_>, started: Instant, prompt_tokens: u64) {
        let warmer = &self.shared.warmer;
        let retention = self.shared.retention();
        let costed = omc_providers::pricing(self.provider, &self.model).zip(
            omc_providers::cache_ttl(self.provider, &self.model, retention),
        );
        let wanted = self.shared.warming()
            && omc_providers::warmable(self.provider, &self.model, request.effort);
        let Some((pricing, ttl)) = costed.filter(|_| wanted && prompt_tokens > 0) else {
            warmer.stop();
            return;
        };
        let snapshot = Snapshot {
            model: self.inner.clone(),
            target: Target {
                provider: self.provider,
                model: self.model.clone(),
                retention,
                effort: request.effort,
                checkpoints: self.checkpoints,
            },
            system: request.system.to_owned(),
            messages: request.messages.to_vec(),
            tools: request.tools.to_vec(),
            prompt_tokens,
            started,
        };
        warmer.start(Arc::downgrade(&self.shared), snapshot, ttl, pricing);
    }
}

#[async_trait]
impl ChatModel for Recorder {
    /// Sends `request` and, once it is answered, keeps it warm; the earlier request is no longer warmed from the moment this one is sent, since this one reads its entries anyway.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.shared.warmer.stop();
        let started = Instant::now();
        let reply = self.inner.complete(request, sink).await?;
        let prompt_tokens = reply.tokens.map_or(0, |tokens| tokens.context());
        self.keep(&request, started, prompt_tokens);
        Ok(reply)
    }

    /// Sends `request` without writing to the cache, and does not keep it warm.
    async fn complete_uncached(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        self.inner.complete_uncached(request, sink).await
    }

    /// Warms `request` with the wrapped model.
    async fn warm(&self, request: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
        self.inner.warm(request).await
    }
}

#[cfg(test)]
/// Tests for warming the cache.
mod tests {
    use super::*;
    use crate::settings::Mode;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The prices of Claude Opus 5.5.
    fn opus() -> Pricing {
        omc_providers::pricing(Provider::Claude, "claude-opus-5-5").unwrap()
    }

    /// Entries are warmed at nine tenths of their life, keeping a margin before short ones expire.
    #[test]
    fn warms_before_expiry() {
        assert_eq!(
            warm_delay(Duration::from_secs(300)),
            Some(Duration::from_secs(270))
        );
        assert_eq!(
            warm_delay(Duration::from_secs(50)),
            Some(Duration::from_secs(40))
        );
        assert_eq!(warm_delay(Duration::from_secs(5)), None);
    }

    /// While a turn runs, a large prompt is worth warming; while idle, only a larger one is, and a small one never.
    #[test]
    fn saving_weighs_the_chance_of_a_next_request() {
        let price = opus();
        let running = expected_saving(&price, Retention::Short, 100_000, 1.0);
        assert!((running - (0.5 - 0.02 - 0.02)).abs() < 1e-9);
        assert!(
            expected_saving(&price, Retention::Short, 100_000, IDLE_CONTINUATION) > MINIMUM_SAVING
        );
        assert!(
            expected_saving(&price, Retention::Short, 20_000, IDLE_CONTINUATION) < MINIMUM_SAVING
        );
        assert!(
            expected_saving(&price, Retention::Long, 100_000, 1.0)
                > expected_saving(&price, Retention::Short, 100_000, 1.0)
        );
    }

    /// A model that answers with a prompt of a fixed size and counts the warm requests it gets.
    struct Counting {
        /// How many tokens each prompt holds.
        tokens: u64,
        /// How many warm requests came.
        warms: AtomicUsize,
    }

    #[async_trait]
    impl ChatModel for Counting {
        /// Answers with nothing but usage.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            Ok(Reply {
                tokens: Some(Tokens {
                    cache_read: self.tokens,
                    ..Tokens::default()
                }),
                ..Reply::default()
            })
        }

        /// Counts the warm request and reports it as a cache read.
        async fn warm(&self, _: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
            self.warms.fetch_add(1, Ordering::SeqCst);
            Ok(Some(Tokens {
                cache_read: self.tokens,
                ..Tokens::default()
            }))
        }
    }

    /// A sink that drops everything.
    struct Nowhere;

    impl Sink for Nowhere {
        /// Ignores the start of a block.
        fn start(&mut self, _: omc_providers::Part) {}

        /// Ignores a chunk of text.
        fn text(&mut self, _: &str) {}

        /// Ignores a chunk of thinking.
        fn thinking(&mut self, _: &str) {}
    }

    /// Sends one request of a prompt of `tokens` tokens through a recorder on Claude Opus 5.5, with a turn running when `running`, lets `minutes` pass, and returns how many warm requests were sent and the conversation.
    async fn after(tokens: u64, running: bool, minutes: u64) -> (usize, Arc<Shared>) {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(
            Provider::Claude,
            Vec::new(),
            "claude-opus-5-5".into(),
            Mode::Yolo,
        );
        let model = Arc::new(Counting {
            tokens,
            warms: AtomicUsize::new(0),
        });
        let recorder = Recorder {
            inner: model.clone(),
            shared: shared.clone(),
            provider: Provider::Claude,
            model: "claude-opus-5-5".into(),
            checkpoints: false,
        };
        let turn = running.then(|| shared.warmer.running());
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "s",
            messages: &messages,
            tools: &[],
            effort: shared.effort_now(),
        };
        recorder.complete(request, &mut Nowhere).await.unwrap();
        tokio::time::sleep(Duration::from_secs(minutes * 60)).await;
        drop(turn);
        (model.warms.load(Ordering::SeqCst), shared)
    }

    /// While a turn runs, a large prompt is warmed every four and a half minutes, for an hour at most.
    #[tokio::test(start_paused = true)]
    async fn running_turns_are_warmed_for_an_hour() {
        let (warms, _) = after(100_000, true, 20).await;
        assert_eq!(warms, 4);
        let (warms, shared) = after(100_000, true, 120).await;
        assert_eq!(warms, 13);
        let spent = shared.warmer.with(|state| state.spent);
        assert_eq!(spent.warms, 13);
        assert_eq!(spent.tokens.cache_read, 13 * 100_000);
    }

    /// While idle, a large prompt is warmed for half an hour at most, and a small one not at all.
    #[tokio::test(start_paused = true)]
    async fn idle_warming_is_shorter_and_needs_a_larger_prompt() {
        assert_eq!(after(100_000, false, 120).await.0, 6);
        assert_eq!(after(20_000, false, 120).await.0, 0);
    }

    /// Turning warming off, or another model, stops it.
    #[tokio::test(start_paused = true)]
    async fn warming_follows_the_settings() {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(
            Provider::Claude,
            Vec::new(),
            "claude-opus-5-5".into(),
            Mode::Yolo,
        );
        shared.set_warming(false);
        let model = Arc::new(Counting {
            tokens: 100_000,
            warms: AtomicUsize::new(0),
        });
        let recorder = Recorder {
            inner: model.clone(),
            shared: shared.clone(),
            provider: Provider::Claude,
            model: "claude-opus-5-5".into(),
            checkpoints: false,
        };
        let _turn = shared.warmer.running();
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "s",
            messages: &messages,
            tools: &[],
            effort: shared.effort_now(),
        };
        recorder.complete(request, &mut Nowhere).await.unwrap();
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(model.warms.load(Ordering::SeqCst), 0);
        shared.set_warming(true);
        recorder.complete(request, &mut Nowhere).await.unwrap();
        tokio::time::sleep(Duration::from_secs(300)).await;
        assert_eq!(model.warms.load(Ordering::SeqCst), 1);
        shared.set_model("claude-sonnet-5-5");
        tokio::time::sleep(Duration::from_secs(600)).await;
        assert_eq!(model.warms.load(Ordering::SeqCst), 1);
    }

    /// A conversation on Claude Opus 5.5 with a turn running, a recorder for it and the model behind that, which answers with a prompt of 100,000 tokens.
    fn warming_turn() -> (Arc<Shared>, Running, Recorder, Arc<Counting>) {
        let (out, _events) = tokio::sync::mpsc::unbounded_channel();
        let shared = Shared::new(out);
        shared.open(
            Provider::Claude,
            Vec::new(),
            "claude-opus-5-5".into(),
            Mode::Yolo,
        );
        let model = Arc::new(Counting {
            tokens: 100_000,
            warms: AtomicUsize::new(0),
        });
        let recorder = Recorder {
            inner: model.clone(),
            shared: shared.clone(),
            provider: Provider::Claude,
            model: "claude-opus-5-5".into(),
            checkpoints: false,
        };
        let turn = shared.warmer.running();
        (shared, turn, recorder, model)
    }

    /// Another effort, or turning the checkpoint tools on or off, stops warming a request sent with the earlier setting, even when the warmer is not told.
    #[tokio::test(start_paused = true)]
    async fn warming_follows_effort_and_checkpoints() {
        for change in ["effort", "checkpoints"] {
            let (shared, _turn, recorder, model) = warming_turn();
            let messages = [Message::User("hi".into())];
            let request = Request {
                system: "s",
                messages: &messages,
                tools: &[],
                effort: shared.effort_now(),
            };
            recorder.complete(request, &mut Nowhere).await.unwrap();
            let other = omc_providers::efforts(Provider::Claude, "claude-opus-5-5")
                .into_iter()
                .find(|effort| Some(*effort) != shared.effort_now());
            shared.change_quietly(|effort, checkpoints| match change {
                "effort" => *effort = other,
                _ => *checkpoints = true,
            });
            tokio::time::sleep(Duration::from_secs(600)).await;
            assert_eq!(model.warms.load(Ordering::SeqCst), 0, "{change}");
        }
    }

    /// A model that answers its first request at once and takes ten minutes over each later one, counting the warm requests it gets meanwhile.
    struct Slow {
        /// How many requests came.
        asked: AtomicUsize,
        /// How many warm requests came.
        warms: AtomicUsize,
    }

    #[async_trait]
    impl ChatModel for Slow {
        /// Answers with usage only, at once the first time and after ten minutes afterwards.
        async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
            if self.asked.fetch_add(1, Ordering::SeqCst) > 0 {
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
            Ok(Reply {
                tokens: Some(Tokens {
                    cache_read: 100_000,
                    ..Tokens::default()
                }),
                ..Reply::default()
            })
        }

        /// Counts the warm request.
        async fn warm(&self, _: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
            self.warms.fetch_add(1, Ordering::SeqCst);
            Ok(None)
        }
    }

    /// No warm request is sent while a later request is on its way, since that request reads the entries itself.
    #[tokio::test(start_paused = true)]
    async fn nothing_is_warmed_while_a_request_is_on_its_way() {
        let (shared, _turn, _, _) = warming_turn();
        let model = Arc::new(Slow {
            asked: AtomicUsize::new(0),
            warms: AtomicUsize::new(0),
        });
        let recorder = Recorder {
            inner: model.clone(),
            shared: shared.clone(),
            provider: Provider::Claude,
            model: "claude-opus-5-5".into(),
            checkpoints: false,
        };
        let messages = [Message::User("hi".into())];
        let request = Request {
            system: "s",
            messages: &messages,
            tools: &[],
            effort: shared.effort_now(),
        };
        recorder.complete(request, &mut Nowhere).await.unwrap();
        recorder.complete(request, &mut Nowhere).await.unwrap();
        assert_eq!(model.warms.load(Ordering::SeqCst), 0);
    }
}
