//! Background tasks that report back to the event loop.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use omc_agent::{AgentEvent, ApprovalRequest, Approvals, Decision, Prompter};
use omc_auth::Provider;
use omc_providers::{Message, RemoteModel};
use omc_tools::Registry;
use tokio::sync::mpsc::UnboundedSender;
use tokio::sync::oneshot;

use crate::message::Msg;

/// A [`Prompter`] that shows the question in the TUI and waits for the answer.
pub struct UiPrompter {
    /// Channel to the event loop.
    tx: UnboundedSender<Msg>,
}

impl UiPrompter {
    /// Creates a prompter that asks through `tx`.
    pub fn new(tx: UnboundedSender<Msg>) -> Self {
        Self { tx }
    }
}

#[async_trait]
impl Prompter for UiPrompter {
    /// Sends the request to the event loop and waits; denies when the UI is gone.
    async fn ask(&self, request: &ApprovalRequest) -> Decision {
        let (reply, answer) = oneshot::channel();
        let msg = Msg::Approval {
            request: request.clone(),
            reply,
        };
        if self.tx.send(msg).is_err() {
            return Decision::Deny;
        }
        answer.await.unwrap_or(Decision::Deny)
    }
}

/// Starts the browser login for `provider` in the background.
pub fn start_login(tx: UnboundedSender<Msg>, provider: Provider) {
    tokio::spawn(async move {
        let notify = {
            let tx = tx.clone();
            move |url: String| {
                let _ = tx.send(Msg::LoginUrl(url));
            }
        };
        let msg = match omc_auth::login(provider, notify).await {
            Ok(()) => Msg::LoggedIn(provider),
            Err(e) => Msg::LoginFailed(e.to_string()),
        };
        let _ = tx.send(msg);
    });
}

/// Runs one turn of the agent loop for `prompt` and streams its events back.
///
/// `models` holds the user's model choices; a provider without one gets its
/// default, which is reported back with [`Msg::ModelChosen`]. `history` is the
/// conversation so far; the extended conversation comes back in [`Msg::Done`].
pub fn start_chat(
    tx: UnboundedSender<Msg>,
    prompt: String,
    models: HashMap<Provider, String>,
    history: Vec<Message>,
    registry: Arc<Registry>,
    approvals: Arc<Approvals>,
) {
    tokio::spawn(async move {
        let result = run_agent(&tx, prompt, &models, history, &registry, &approvals).await;
        let msg = match result {
            Ok(history) => Msg::Done(history),
            Err(e) => Msg::Failed(format!("{e:#}")),
        };
        let _ = tx.send(msg);
    });
}

/// Fetches the models the active provider offers and reports them back.
pub fn start_model_list(tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let msg = match fetch_models().await {
            Ok((provider, list)) => Msg::ModelList(provider, list),
            Err(e) => Msg::ModelListFailed(format!("{e:#}")),
        };
        let _ = tx.send(msg);
    });
}

/// Refreshes credentials if needed, then lists the active provider's models.
///
/// # Errors
///
/// Returns the auth or provider error that stopped the lookup.
async fn fetch_models() -> anyhow::Result<(Provider, Vec<String>)> {
    let (provider, credentials) = omc_auth::active_credentials().await?;
    let list = omc_providers::list_models(provider, &credentials).await?;
    Ok((provider, list))
}

/// Refreshes credentials if needed, then runs the agent loop, forwarding its
/// events to `tx`.
///
/// # Errors
///
/// Returns the auth or provider error that stopped the turn.
async fn run_agent(
    tx: &UnboundedSender<Msg>,
    prompt: String,
    models: &HashMap<Provider, String>,
    mut history: Vec<Message>,
    registry: &Registry,
    approvals: &Approvals,
) -> anyhow::Result<Vec<Message>> {
    let (provider, credentials) = omc_auth::active_credentials().await?;
    let model = match models.get(&provider) {
        Some(model) => model.clone(),
        None => {
            let model = omc_providers::default_model(provider, &credentials).await?;
            let _ = tx.send(Msg::ModelChosen(provider, model.clone()));
            model
        }
    };
    let model = RemoteModel {
        provider,
        credentials,
        model,
    };
    let events = |event: AgentEvent| {
        let _ = tx.send(Msg::Agent(event));
    };
    omc_agent::run_turn(&model, registry, approvals, &mut history, prompt, &events).await?;
    Ok(history)
}
