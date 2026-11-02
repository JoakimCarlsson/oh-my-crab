//! Background tasks that report back to the event loop.

use std::collections::HashMap;

use omc_auth::Provider;
use tokio::sync::mpsc::UnboundedSender;

use crate::message::Msg;

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

/// Sends `prompt` to the logged-in provider and streams the reply back.
///
/// `models` holds the user's model choices; a provider without one gets its
/// default, which is reported back with [`Msg::ModelChosen`].
pub fn start_chat(tx: UnboundedSender<Msg>, prompt: String, models: HashMap<Provider, String>) {
    tokio::spawn(async move {
        let result = stream_reply(&tx, &prompt, &models).await;
        let msg = match result {
            Ok(()) => Msg::Done,
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

/// Refreshes credentials if needed, then forwards each reply chunk to `tx`.
///
/// # Errors
///
/// Returns the auth or provider error that stopped the reply.
async fn stream_reply(
    tx: &UnboundedSender<Msg>,
    prompt: &str,
    models: &HashMap<Provider, String>,
) -> anyhow::Result<()> {
    let (provider, credentials) = omc_auth::active_credentials().await?;
    let model = match models.get(&provider) {
        Some(model) => model.clone(),
        None => {
            let model = omc_providers::default_model(provider, &credentials).await?;
            let _ = tx.send(Msg::ModelChosen(provider, model.clone()));
            model
        }
    };
    omc_providers::send(provider, &credentials, &model, prompt, |delta| {
        let _ = tx.send(Msg::Delta(delta.to_string()));
    })
    .await?;
    Ok(())
}
