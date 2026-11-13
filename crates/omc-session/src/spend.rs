//! What the conversation costs: every model request is priced as it is answered, added up for each model, saved with the conversation and told to the client.
//!
//! The requests of the main loop, of the plan model, of subagents, of summaries and of cache warming all count. A model whose prices are not known is counted in tokens only. Prices are the list prices [`omc_providers::pricing`] gives, so a conversation paid for by a subscription is shown what it would have cost at them.
//!
//! A cache miss is a request of the main loop that reads from the prompt cache less of the conversation than the request before it sent, as happens when the entry expired while the conversation waited, or when a compaction rewrote the conversation. What the part that was not read cost beyond reading it is kept as what the miss cost.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use omc_providers::{
    ChatModel, Pricing, Provider, ProviderError, Reply, Request, Retention, Sink, Tokens,
};
use omc_wire::{Event, ModelSpend, Spent, micros};

use crate::shared::Shared;

/// The key what the conversation cost is saved under with it.
pub(crate) const SPENT_FACT: &str = "spent";

/// How many tokens of the conversation a request may leave unread from the cache before it counts as a miss: the end of a prompt that lies behind the last cached block, and the steps in which a provider that caches by itself counts, are not misses.
const MISS_SLACK: u64 = 2048;

/// How many tokens a price is for.
const PER_MILLION: f64 = 1_000_000.0;

/// What the requests sent to one model cost.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Account {
    /// The model, as the client is told it.
    model: String,
    /// How many requests it answered.
    requests: u64,
    /// The tokens they were billed for.
    tokens: Tokens,
    /// Whether the model's prices are known, so that `dollars` says what it cost.
    priced: bool,
    /// What they cost, in US dollars.
    dollars: f64,
    /// How much of that was paid for cache misses, in US dollars.
    missed: f64,
}

impl Account {
    /// The account as the client is told it, with dollars as whole millionths.
    fn told(&self) -> ModelSpend {
        ModelSpend {
            model: self.model.clone(),
            requests: self.requests,
            input: self.tokens.input,
            output: self.tokens.output,
            cache_read: self.tokens.cache_read,
            cache_write: self.tokens.cache_write,
            cost: self.priced.then(|| micros(self.dollars)),
            missed: micros(self.missed),
        }
    }
}

/// The last request of the main loop: what the next one is expected to read from the cache.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Lane {
    /// The model it went to.
    model: String,
    /// How many tokens its prompt held.
    prompt: u64,
}

/// Everything that is kept of what the conversation cost, as it is saved with it.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct Books {
    /// What each model cost, in the order they were first asked.
    accounts: Vec<Account>,
    /// The last request of the main loop, when there was one.
    lane: Option<Lane>,
}

impl Books {
    /// The books `saved` with a conversation, or empty ones when none were or they cannot be read.
    fn read(saved: Option<serde_json::Value>) -> Self {
        saved
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or_default()
    }

    /// The books as the client is told them.
    fn told(&self) -> Spent {
        Spent {
            models: self.accounts.iter().map(Account::told).collect(),
        }
    }

    /// The books as the client is told them, when they hold something.
    fn told_if_any(&self) -> Option<Spent> {
        (!self.accounts.is_empty()).then(|| self.told())
    }
}

/// What the books `saved` with a conversation say it cost, as a client is told it; nothing when it cost nothing.
pub(crate) fn told(saved: Option<serde_json::Value>) -> Option<Spent> {
    Books::read(saved).told_if_any()
}

/// One answered request to add to the books.
pub(crate) struct Bill<'a> {
    /// The model that answered, as the client is told it.
    pub(crate) model: &'a str,
    /// The tokens it was billed for.
    pub(crate) tokens: Tokens,
    /// What the model charges, when that is known.
    pub(crate) pricing: Option<Pricing>,
    /// How long the cache entries the request wrote live.
    pub(crate) retention: Retention,
    /// Whether it is a request of the main loop, whose prompt the next such request is expected to read from the cache.
    pub(crate) main: bool,
}

/// What the part of a prompt that an earlier request of `earlier` tokens had cached, and the request billed `tokens` did not read from the cache, cost beyond reading it, in US dollars at `pricing` for entries that live as `retention` says; nothing when all but [`MISS_SLACK`] tokens of it were read.
fn miss_cost(pricing: &Pricing, retention: Retention, earlier: u64, tokens: &Tokens) -> f64 {
    let unread = earlier
        .min(tokens.context())
        .saturating_sub(tokens.cache_read);
    if unread <= MISS_SLACK {
        return 0.0;
    }
    let written = unread.min(tokens.cache_write);
    let beyond =
        |count: u64, price: f64| count as f64 * (price - pricing.cache_read).max(0.0) / PER_MILLION;
    beyond(written, pricing.cache_write_for(retention)) + beyond(unread - written, pricing.input)
}

/// What the conversation cost so far.
#[derive(Default)]
pub(crate) struct Ledger {
    /// The books.
    books: Mutex<Books>,
}

impl Ledger {
    /// Runs `with` on the books.
    fn with<T>(&self, with: impl FnOnce(&mut Books) -> T) -> T {
        let mut books = self.books.lock().unwrap_or_else(|held| held.into_inner());
        with(&mut books)
    }

    /// Starts from the books `saved` with a conversation, or from nothing when none were or they cannot be read, and returns what the client is told when they hold something.
    pub(crate) fn restore(&self, saved: Option<serde_json::Value>) -> Option<Spent> {
        let books = Books::read(saved);
        self.with(|held| {
            *held = books;
            held.told_if_any()
        })
    }

    /// What the conversation cost so far, as the client is told it; nothing when it cost nothing.
    pub(crate) fn told(&self) -> Option<Spent> {
        self.with(|books| books.told_if_any())
    }

    /// Adds `bill` to the books and returns them as the client is told them and as they are saved.
    fn charge(&self, bill: &Bill<'_>) -> (Spent, serde_json::Value) {
        self.with(|books| {
            let earlier = books
                .lane
                .as_ref()
                .filter(|lane| bill.main && lane.model == bill.model)
                .map(|lane| lane.prompt);
            if bill.main {
                books.lane = Some(Lane {
                    model: bill.model.to_owned(),
                    prompt: bill.tokens.context(),
                });
            }
            let at = books
                .accounts
                .iter()
                .position(|account| account.model == bill.model)
                .unwrap_or_else(|| {
                    books.accounts.push(Account {
                        model: bill.model.to_owned(),
                        ..Account::default()
                    });
                    books.accounts.len() - 1
                });
            let account = &mut books.accounts[at];
            account.requests += 1;
            account.tokens.input += bill.tokens.input;
            account.tokens.output += bill.tokens.output;
            account.tokens.cache_read += bill.tokens.cache_read;
            account.tokens.cache_write += bill.tokens.cache_write;
            if let Some(pricing) = &bill.pricing {
                account.priced = true;
                account.dollars += pricing.cost(&bill.tokens, bill.retention);
                if let Some(earlier) = earlier {
                    account.missed += miss_cost(pricing, bill.retention, earlier, &bill.tokens);
                }
            }
            (
                books.told(),
                serde_json::to_value(&*books).unwrap_or_default(),
            )
        })
    }
}

impl Shared {
    /// Adds the answered request `bill` to what the conversation cost, saves that with the conversation and tells the client.
    pub(crate) fn charge(&self, bill: &Bill<'_>) {
        let (spent, saved) = self.ledger.charge(bill);
        self.save_fact(SPENT_FACT, &saved);
        self.say(Event::Spent(spent));
    }
}

/// A model whose requests are counted toward what the conversation cost: it passes every request on to the model it wraps and charges what the answer was billed.
pub(crate) struct Priced {
    /// The model requests go to.
    inner: Arc<dyn ChatModel>,
    /// The conversation that pays.
    shared: Arc<Shared>,
    /// The model, as the client is told it.
    name: String,
    /// What the model charges, when that is known.
    pricing: Option<Pricing>,
    /// How long the cache entries its requests write live.
    retention: Retention,
    /// Whether its cached requests are those of the main loop.
    main: bool,
}

impl Priced {
    /// Counts the requests `inner` answers as the model `id` of `provider` toward what the conversation `shared` cost; `main` says the requests that write to the cache are those of the main loop, which are watched for cache misses.
    ///
    /// A model of another provider than the conversation's is named `provider/id` and writes cache entries with the default lifetime, as the conversation's setting applies to its own provider only.
    pub(crate) fn of(
        inner: Arc<dyn ChatModel>,
        shared: Arc<Shared>,
        provider: Provider,
        id: &str,
        main: bool,
    ) -> Self {
        let same = shared.model().is_none_or(|(held, _)| held == provider);
        Self {
            inner,
            name: if same {
                id.to_owned()
            } else {
                format!("{}/{id}", provider.def().id)
            },
            pricing: omc_providers::pricing(provider, id),
            retention: if same {
                shared.retention()
            } else {
                Retention::default()
            },
            shared,
            main,
        }
    }

    /// Charges the conversation for `tokens`, when the provider said what a request was billed; `main` says it was a request of the main loop.
    fn charge(&self, tokens: Option<Tokens>, main: bool) {
        if let Some(tokens) = tokens {
            self.shared.charge(&Bill {
                model: &self.name,
                tokens,
                pricing: self.pricing,
                retention: self.retention,
                main,
            });
        }
    }
}

#[async_trait]
impl ChatModel for Priced {
    /// Sends `request` and charges what it cost.
    async fn complete(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let reply = self.inner.complete(request, sink).await?;
        self.charge(reply.tokens, self.main);
        Ok(reply)
    }

    /// Sends `request` without writing to the cache, and charges what it cost; it is no request of the main loop, since the next one does not read what it sent.
    async fn complete_uncached(
        &self,
        request: Request<'_>,
        sink: &mut dyn Sink,
    ) -> Result<Reply, ProviderError> {
        let reply = self.inner.complete_uncached(request, sink).await?;
        self.charge(reply.tokens, false);
        Ok(reply)
    }

    /// Warms `request` with the wrapped model and charges what that cost.
    async fn warm(&self, request: Request<'_>) -> Result<Option<Tokens>, ProviderError> {
        let tokens = self.inner.warm(request).await?;
        self.charge(tokens, false);
        Ok(tokens)
    }
}

/// Tests for adding up what a conversation cost.
#[cfg(test)]
mod tests {
    use super::*;

    /// Prices of five dollars for input, twenty-five for output and half a dollar for cache reads per million tokens, with cache writes a quarter more than input.
    fn prices() -> Pricing {
        Pricing {
            input: 5.0,
            output: 25.0,
            cache_read: 0.5,
            cache_write: 6.25,
            cache_write_long: 10.0,
        }
    }

    /// A bill for `tokens` answered by `model`, at [`prices`] when `priced`.
    fn bill(model: &str, tokens: Tokens, priced: bool, main: bool) -> Bill<'_> {
        Bill {
            model,
            tokens,
            pricing: priced.then(prices),
            retention: Retention::Short,
            main,
        }
    }

    /// Requests add up for each model in the order the models were first asked, and a model without prices is counted in tokens only.
    #[test]
    fn requests_add_up_for_each_model() {
        let ledger = Ledger::default();
        let tokens = Tokens {
            input: 1_000_000,
            output: 100_000,
            cache_read: 2_000_000,
            cache_write: 0,
        };
        ledger.charge(&bill("opus", tokens, true, false));
        ledger.charge(&bill("local", tokens, false, false));
        let (spent, _) = ledger.charge(&bill("opus", tokens, true, false));
        assert_eq!(spent.models.len(), 2);
        let opus = &spent.models[0];
        assert_eq!((opus.model.as_str(), opus.requests), ("opus", 2));
        assert_eq!(opus.cost, Some(17_000_000));
        assert_eq!(opus.cache_read, 4_000_000);
        assert_eq!(opus.missed, 0);
        let local = &spent.models[1];
        assert_eq!((local.cost, local.output), (None, 100_000));
        assert_eq!(spent.cost(), Some(17_000_000));
        assert!(spent.partial());
    }

    /// A request of the main loop that reads the earlier prompt from the cache misses nothing; one that writes it again is charged the difference to reading it as a miss, and requests off the main loop are not watched.
    #[test]
    fn a_prompt_written_again_is_a_cache_miss() {
        let ledger = Ledger::default();
        let first = Tokens {
            input: 10,
            output: 50,
            cache_read: 0,
            cache_write: 100_000,
        };
        let hit = Tokens {
            input: 10,
            output: 50,
            cache_read: 100_000,
            cache_write: 3_990,
        };
        let expired = Tokens {
            input: 10,
            output: 50,
            cache_read: 0,
            cache_write: 110_000,
        };
        ledger.charge(&bill("opus", first, true, true));
        let (spent, _) = ledger.charge(&bill("opus", hit, true, true));
        assert_eq!(spent.missed(), 0);
        let (spent, _) = ledger.charge(&bill("opus", expired, true, false));
        assert_eq!(spent.missed(), 0);
        let (spent, _) = ledger.charge(&bill("opus", expired, true, true));
        assert_eq!(spent.missed(), 598_000);
        let (spent, _) = ledger.charge(&bill("haiku", expired, true, true));
        assert_eq!(spent.models[1].missed, 0);
    }

    /// The books saved with a conversation are taken up again, cents and the last prompt included, and opening a conversation without any starts from nothing.
    #[test]
    fn the_books_survive_being_saved() {
        let ledger = Ledger::default();
        let tokens = Tokens {
            input: 3,
            output: 7,
            cache_read: 0,
            cache_write: 50_000,
        };
        let (spent, saved) = ledger.charge(&bill("opus", tokens, true, true));
        let reopened = Ledger::default();
        assert_eq!(reopened.restore(Some(saved)), Some(spent));
        let (again, _) = reopened.charge(&bill("opus", tokens, true, true));
        assert_eq!(again.models[0].requests, 2);
        assert!(again.missed() > 0);
        assert_eq!(reopened.restore(None), None);
        assert_eq!(reopened.restore(Some(serde_json::json!("nonsense"))), None);
    }
}
