//! Background tasks that report back to the event loop.

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
pub fn start_chat(tx: UnboundedSender<Msg>, prompt: String) {
    tokio::spawn(async move {
        let result = stream_reply(&tx, &prompt).await;
        let msg = match result {
            Ok(()) => Msg::Done,
            Err(e) => Msg::Failed(format!("{e:#}")),
        };
        let _ = tx.send(msg);
    });
}

/// Refreshes credentials if needed, then forwards each reply chunk to `tx`.
///
/// # Errors
///
/// Returns the auth or provider error that stopped the reply.
async fn stream_reply(tx: &UnboundedSender<Msg>, prompt: &str) -> anyhow::Result<()> {
    let (provider, credentials) = omc_auth::active_credentials().await?;
    omc_providers::send(provider, &credentials, prompt, |delta| {
        let _ = tx.send(Msg::Delta(delta.to_string()));
    })
    .await?;
    Ok(())
}
