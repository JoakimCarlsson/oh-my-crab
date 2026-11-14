//! Asking other models than the turn's own: a faster one for summaries, and the backups of a slot when its model keeps failing.

use std::sync::Arc;

use omc_providers::{ChatModel, Effort, ProviderError};

use crate::{AgentEvent, ModelPicker, Picked};

/// The other models a turn may ask besides its own.
#[derive(Clone, Default)]
pub struct Routing {
    /// The model that writes the summaries of compactions, instead of the turn's own.
    pub summarizer: Option<Picked>,
    /// The backups tried when the turn's model fails in a way another model may not.
    pub fallback: Option<Fallback>,
}

/// The backups of the slot a turn's model was chosen for.
#[derive(Clone)]
pub struct Fallback {
    /// The slot's name, such as `main` or `plan`.
    pub slot: String,
    /// What the turn's own model is called.
    pub from: String,
    /// The selectors of the backups, tried in order.
    pub backups: Vec<String>,
    /// Finds the model each backup names.
    pub picker: Arc<dyn ModelPicker>,
}

/// Which model a turn asks now: its own until a request fails for good, then each backup of its slot in turn, for the rest of the turn.
pub(crate) struct Asked<'a> {
    /// The turn's own model.
    own: &'a dyn ChatModel,
    /// How hard the turn's own model thinks.
    own_effort: Option<Effort>,
    /// The backups, when the turn has any.
    fallback: Option<&'a Fallback>,
    /// How many backups were tried.
    tried: usize,
    /// The backup asked now, when the turn moved to one.
    switched: Option<Picked>,
}

impl<'a> Asked<'a> {
    /// The turn's own `model`, thinking at `effort`, with the backups of `routing` when it has some.
    pub(crate) fn new(
        model: &'a dyn ChatModel,
        effort: Option<Effort>,
        routing: Option<&'a Routing>,
    ) -> Self {
        Self {
            own: model,
            own_effort: effort,
            fallback: routing.and_then(|routing| routing.fallback.as_ref()),
            tried: 0,
            switched: None,
        }
    }

    /// The model asked now.
    pub(crate) fn model(&self) -> &dyn ChatModel {
        self.switched
            .as_ref()
            .map_or(self.own, |picked| picked.model.as_ref())
    }

    /// How hard the model asked now thinks.
    pub(crate) fn effort(&self) -> Option<Effort> {
        self.switched
            .as_ref()
            .map_or(self.own_effort, |picked| picked.effort)
    }

    /// Moves to the next backup that can be found after a request failed for good with `error`, telling `events` with [`AgentEvent::ModelFallback`]; whether it moved. Nothing moves for an error another model would meet too, such as a sign-in problem or a request the provider will not take, or when the backups are used up; a backup that cannot be found is passed over.
    pub(crate) async fn fall_back(
        &mut self,
        error: &ProviderError,
        events: &(dyn Fn(AgentEvent) + Send + Sync),
    ) -> bool {
        let Some(fallback) = self.fallback.filter(|_| worth_switching(error)) else {
            return false;
        };
        while let Some(selector) = fallback.backups.get(self.tried) {
            self.tried += 1;
            let Ok(picked) = fallback.picker.pick(selector).await else {
                continue;
            };
            let from = self
                .switched
                .as_ref()
                .map_or_else(|| fallback.from.clone(), |picked| picked.name.clone());
            let reason = error.to_string();
            events(AgentEvent::ModelFallback {
                slot: fallback.slot.clone(),
                from,
                to: picked.name.clone(),
                reason: reason.lines().next().unwrap_or_default().to_owned(),
            });
            self.switched = Some(picked);
            return true;
        }
        false
    }
}

/// Phrases, in lower case, of a provider saying the model asked for cannot be had.
const UNAVAILABLE_PHRASES: [&str; 5] = [
    "model_not_found",
    "model not found",
    "does not exist",
    "not available",
    "unavailable",
];

/// Whether a request that failed for good with `error` is worth sending to another model: the provider is overloaded, rate limited or failing inside, or the model cannot be had. A sign-in problem, a refused or malformed request and a context overflow are not, since another model would meet them too.
pub fn worth_switching(error: &ProviderError) -> bool {
    if error.is_context_overflow() {
        return false;
    }
    match error {
        ProviderError::Status { status, body, .. } => {
            let code: u16 = status
                .split_whitespace()
                .next()
                .and_then(|code| code.parse().ok())
                .unwrap_or(0);
            let body = body.to_lowercase();
            match code {
                429 | 500..=599 | 404 => true,
                400 | 403 => {
                    body.contains("model")
                        && UNAVAILABLE_PHRASES
                            .iter()
                            .any(|phrase| body.contains(phrase))
                }
                _ => false,
            }
        }
        ProviderError::Stream(_) => error.is_transient(),
        ProviderError::Http(_)
        | ProviderError::Unfinished
        | ProviderError::Credentials(_)
        | ProviderError::NoModels => false,
    }
}

#[cfg(test)]
/// Tests of which failures move to a backup.
mod tests {
    use super::*;

    /// The error a provider gives with `status` and `body`.
    fn status(status: &str, body: &str) -> ProviderError {
        ProviderError::Status {
            status: status.to_owned(),
            body: body.to_owned(),
            retry_after: None,
        }
    }

    /// Overload, rate limits, server faults and missing models move on; sign-in problems, bad requests and overflows do not.
    #[test]
    fn only_failures_another_model_may_not_meet_move_on() {
        assert!(worth_switching(&status("529", "overloaded")));
        assert!(worth_switching(&status("429 Too Many Requests", "")));
        assert!(worth_switching(&status("503", "")));
        assert!(worth_switching(&status("404", "model_not_found")));
        assert!(worth_switching(&status(
            "400",
            "The model x does not exist"
        )));
        assert!(worth_switching(&ProviderError::Stream(
            "overloaded_error".to_owned()
        )));
        assert!(!worth_switching(&status("401", "invalid x-api-key")));
        assert!(!worth_switching(&status("403", "forbidden")));
        assert!(!worth_switching(&status("400", "messages: field required")));
        assert!(!worth_switching(&status("413", "request_too_large")));
        assert!(!worth_switching(&ProviderError::Credentials(
            "no region".to_owned()
        )));
    }
}
