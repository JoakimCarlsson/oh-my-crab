//! Finding the model a subagent asks by the name its definition or a `task` call gives.
//!
//! A name is looked up among the models the conversation's provider listed: an exact id first, then an alias such as `haiku`, which picks the first listed id holding it, the provider listing its best models first. A name that is not found there and starts with the id of another provider and a `/` picks a model of that provider, when the reader is signed in to it.

use std::sync::Arc;

use async_trait::async_trait;
use omc_agent::{ModelPicker, Picked};
use omc_auth::AuthError;
use omc_providers::{Effort, Provider, RemoteModel, Retention};

use crate::settings::default_effort;
use crate::shared::Shared;

/// How many model ids an error lists at most.
const LISTED: usize = 20;

/// Finds the models of a conversation's subagents.
pub(crate) struct SessionPicker {
    /// What the conversation shares: its logins, the models its provider listed and how long the prompt cache is kept.
    pub(crate) shared: Arc<Shared>,
    /// The provider of the conversation.
    pub(crate) provider: Provider,
    /// How hard the conversation's model thinks, which a subagent's model keeps when it takes it.
    pub(crate) effort: Option<Effort>,
}

#[async_trait]
impl ModelPicker for SessionPicker {
    /// The model `wanted` names, among the conversation provider's models or, as `provider/model`, those of another signed-in provider, with the effort it thinks at: the conversation's, when it takes it, else its own default.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when no model of the provider matches, the other provider is not signed in, or its models cannot be listed.
    async fn pick(&self, wanted: &str) -> Result<Picked, String> {
        self.locate(wanted).await.map(|(_, _, picked)| picked)
    }
}

impl SessionPicker {
    /// The model `wanted` names, as [`ModelPicker::pick`] finds it, with the provider that serves it and its id there.
    ///
    /// # Errors
    ///
    /// Same as [`ModelPicker::pick`].
    pub(crate) async fn locate(&self, wanted: &str) -> Result<(Provider, String, Picked), String> {
        let wanted = wanted.trim();
        let listed = self.shared.models_listed();
        let exact = listed.iter().any(|id| id.eq_ignore_ascii_case(wanted));
        let (provider, id) = match provider_named(wanted).filter(|_| !exact) {
            Some((named, rest)) if named == self.provider => {
                (named, resolve(rest, named, &listed)?)
            }
            Some((other, rest)) => (other, self.resolve_with(other, rest).await?),
            None => (self.provider, resolve(wanted, self.provider, &listed)?),
        };
        let credentials = self
            .shared
            .with_home(omc_providers::credentials_for(provider))
            .await
            .map_err(|error| signed_out(provider, &error))?;
        let same = provider == self.provider;
        let efforts = omc_providers::efforts(provider, &id);
        let effort = self
            .effort
            .filter(|effort| efforts.contains(effort))
            .or_else(|| default_effort(&efforts));
        let name = if same {
            id.clone()
        } else {
            format!("{}/{id}", provider.def().id)
        };
        let picked = Picked {
            model: Arc::new(RemoteModel::new(
                provider,
                credentials,
                id.clone(),
                if same {
                    self.shared.retention()
                } else {
                    Retention::default()
                },
                self.shared.home(),
            )),
            name,
            effort,
        };
        Ok((provider, id, picked))
    }

    /// The id `rest` names among the models of `provider`, another provider than the conversation's, which are listed for it.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when the reader is not signed in to `provider`, its models cannot be listed, or none matches.
    async fn resolve_with(&self, provider: Provider, rest: &str) -> Result<String, String> {
        let credentials = self
            .shared
            .with_home(omc_providers::credentials_for(provider))
            .await
            .map_err(|error| signed_out(provider, &error))?;
        let listed = omc_providers::list_models(provider, &credentials)
            .await
            .map_err(|error| {
                format!(
                    "the models of {} cannot be listed: {error}",
                    provider.def().id
                )
            })?;
        resolve(rest, provider, &listed)
    }
}

/// Why the models of `provider` cannot be used, from the `error` its credentials failed with.
fn signed_out(provider: Provider, error: &AuthError) -> String {
    match error {
        AuthError::NotLoggedIn => format!(
            "not signed in to {}; sign in to it first, or name a model of the conversation's provider",
            provider.def().id
        ),
        other => format!("the login of {} cannot be used: {other}", provider.def().id),
    }
}

/// The provider `wanted` names before its first `/`, and what follows, when it names one and something follows.
fn provider_named(wanted: &str) -> Option<(Provider, &str)> {
    let (prefix, rest) = wanted.split_once('/')?;
    let provider = Provider::from_id(prefix.trim())?;
    (!rest.trim().is_empty()).then_some((provider, rest.trim()))
}

/// The id among `listed`, the models of `provider` best first, that `wanted` names: the one equal to it in any case, else the first holding it, so that an alias such as `haiku` picks the provider's best Haiku. When nothing was listed, `wanted` is taken as it is.
///
/// # Errors
///
/// Fails, listing some of the models there are, when none matches.
pub(crate) fn resolve(
    wanted: &str,
    provider: Provider,
    listed: &[String],
) -> Result<String, String> {
    let wanted = wanted.trim();
    if wanted.is_empty() {
        return Err("no model was named".to_owned());
    }
    if listed.is_empty() {
        return Ok(wanted.to_owned());
    }
    let lower = wanted.to_ascii_lowercase();
    listed
        .iter()
        .find(|id| id.eq_ignore_ascii_case(wanted))
        .or_else(|| {
            listed
                .iter()
                .find(|id| id.to_ascii_lowercase().contains(&lower))
        })
        .cloned()
        .ok_or_else(|| {
            let shown: Vec<&str> = listed.iter().take(LISTED).map(String::as_str).collect();
            let more = if listed.len() > LISTED { ", ..." } else { "" };
            format!(
                "{} has no model matching {wanted}; its models are: {}{more}. Give an alias such as haiku, sonnet or opus, an id from the list, or provider/model for another signed-in provider",
                provider.def().id,
                shown.join(", ")
            )
        })
}

#[cfg(test)]
/// Tests for finding subagent models by name.
mod tests {
    use super::*;

    /// The models a Claude account might list, best first.
    fn listed() -> Vec<String> {
        [
            "claude-opus-4-5-20251101",
            "claude-sonnet-4-5-20250929",
            "claude-haiku-4-5-20251001",
            "claude-3-5-haiku-20241022",
        ]
        .map(str::to_owned)
        .to_vec()
    }

    /// An alias picks the first listed model holding it, in any case.
    #[test]
    fn aliases_pick_the_best_listed() {
        assert_eq!(
            resolve("haiku", Provider::Claude, &listed()).unwrap(),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(
            resolve("Sonnet", Provider::Claude, &listed()).unwrap(),
            "claude-sonnet-4-5-20250929"
        );
    }

    /// An exact id wins over a model that merely holds it.
    #[test]
    fn exact_ids_win() {
        assert_eq!(
            resolve("claude-3-5-haiku-20241022", Provider::Claude, &listed()).unwrap(),
            "claude-3-5-haiku-20241022"
        );
    }

    /// An unknown name fails with the models there are; nothing listed takes the name as it is.
    #[test]
    fn unknown_names_list_the_models() {
        let error = resolve("gpt-9", Provider::Claude, &listed()).unwrap_err();
        assert!(error.contains("no model matching gpt-9"), "{error}");
        assert!(error.contains("claude-haiku-4-5-20251001"), "{error}");
        assert_eq!(resolve("x-1", Provider::Claude, &[]).unwrap(), "x-1");
        assert!(resolve(" ", Provider::Claude, &listed()).is_err());
    }

    /// A known provider's id before a `/` names that provider; an unknown one, or nothing after the `/`, does not.
    #[test]
    fn providers_are_named_before_a_slash() {
        assert_eq!(
            provider_named("codex/gpt-5"),
            Some((Provider::Codex, "gpt-5"))
        );
        assert_eq!(
            provider_named("claude/haiku"),
            Some((Provider::Claude, "haiku"))
        );
        assert_eq!(provider_named("acme/model"), None);
        assert_eq!(provider_named("codex/"), None);
        assert_eq!(provider_named("haiku"), None);
    }
}
