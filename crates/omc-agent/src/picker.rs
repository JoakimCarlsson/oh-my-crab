//! The models subagents ask: the one of the agent that starts them, or another one a definition or a call names.

use std::sync::Arc;

use async_trait::async_trait;
use omc_providers::{ChatModel, Effort};

/// A model found by the name a definition or a call gave.
#[derive(Clone)]
pub struct Picked {
    /// The model to ask.
    pub model: Arc<dyn ChatModel>,
    /// What it is called, as subagents using it are shown: its id, after the provider and a `/` when another provider serves it.
    pub name: String,
    /// How hard it is asked to think; nothing when it does not think.
    pub effort: Option<Effort>,
}

/// Finds a model by the name an agent definition or a `task` call gives it.
#[async_trait]
pub trait ModelPicker: Send + Sync {
    /// The model `wanted` names: an alias such as `haiku`, a model id, or a provider and a model id as `provider/model`.
    ///
    /// # Errors
    ///
    /// Fails with a reason the model can act on, such as the models there are, when `wanted` names none that can be used.
    async fn pick(&self, wanted: &str) -> Result<Picked, String>;
}

/// Where the models of subagents come from.
#[derive(Clone, Default)]
pub struct Models {
    /// What the model subagents ask when nothing names another is called, shown with each subagent that asks it; empty when it is not known.
    pub current: String,
    /// Finds another model by name; nothing when subagents can only ask the default one, which refuses every call or definition naming a model.
    pub picker: Option<Arc<dyn ModelPicker>>,
    /// The name [`Models::picker`] is asked for when neither the call nor the kind's definition names a model, such as `@subagent`; nothing leaves such subagents on the default one.
    pub fallback: Option<String>,
    /// The names tried in order when the model [`Models::fallback`] names fails in a way another model may not.
    pub backups: Vec<String>,
}

impl Models {
    /// The same models, with `current` as the name of the default one.
    pub(crate) fn named(&self, current: &str) -> Self {
        Self {
            current: current.to_owned(),
            picker: self.picker.clone(),
            fallback: self.fallback.clone(),
            backups: self.backups.clone(),
        }
    }
}
