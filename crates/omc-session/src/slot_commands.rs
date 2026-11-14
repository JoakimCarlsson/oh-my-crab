//! The commands that read and change the model slots and presets, and the conversation's model named by a slot.

use omc_providers::{Effort, Provider};
use omc_wire::{Event, SlotScope};

use super::Session;
use crate::settings::{EFFORT, MODEL};
use crate::slots::{self, Layers, Named};

/// The model `value` names for a conversation of `provider` whose models are `listed` and whose model is `current`, when there is one, and the effort it asks for: the model of the slot `@name` names, which is `current` for a slot left to it, or `value` as it is when it names no slot. `@main` names the model the files set for `main`, which a conversation opened without a model starts with.
///
/// A slot's selector naming `provider` before a `/` is taken without it, unless `listed` holds it whole.
///
/// # Errors
///
/// Fails, saying why, when the slot does not exist or its selector is not valid, when it is left to a model there is not yet, or when it names a model of another provider.
pub(crate) fn conversation_model(
    layers: &Layers,
    provider: Provider,
    listed: &[String],
    current: Option<&str>,
    value: &str,
) -> Result<(String, Option<Effort>), String> {
    let value = value.trim();
    if !value.starts_with('@') {
        return Ok((value.to_owned(), None));
    }
    let Named::Slot(name) = slots::parse_named(value)? else {
        return Ok((value.to_owned(), None));
    };
    let selector = if name == slots::MAIN {
        layers.default_model()?
    } else {
        layers.model_of(&name)?
    };
    let Some(selector) = selector else {
        return current
            .map(|current| (current.to_owned(), None))
            .ok_or_else(|| format!("@{name} sets no model, and the conversation has none yet"));
    };
    if listed
        .iter()
        .any(|id| id.eq_ignore_ascii_case(&selector.model))
    {
        return Ok((selector.model, selector.effort));
    }
    match selector.model.split_once('/') {
        Some((prefix, rest)) if Provider::from_id(prefix) == Some(provider) => {
            Ok((rest.to_owned(), selector.effort))
        }
        Some((prefix, _)) if Provider::from_id(prefix).is_some() => Err(format!(
            "@{name} names {}, a model of {prefix}; the conversation's model must be one of {}",
            selector.model,
            provider.def().id
        )),
        _ => Ok((selector.model, selector.effort)),
    }
}

impl Session {
    /// The slots as they stand, with the model each resolves to for the conversation's provider, each provider's assignment, and a suggestion for each the reader is signed in to that the slot gives no model.
    pub(super) fn slots_event(&self) -> Event {
        let layers = self.shared.slot_layers();
        let main = self.shared.model();
        let home = self.shared.home();
        let signed_in: Vec<Provider> = Provider::all()
            .into_iter()
            .filter(|other| {
                let id = other.def().id;
                crate::providers::signed_in(id, home.clone())
                    || crate::providers::key_source(id, home.clone()).is_some()
            })
            .collect();
        let ids: Vec<String> = signed_in
            .iter()
            .map(|provider| provider.def().id.to_owned())
            .collect();
        let suggest = |slot: &str, id: &str| {
            if slot != slots::FAST && slot != slots::VISION {
                return None;
            }
            let provider = Provider::all()
                .into_iter()
                .find(|provider| provider.def().id == id)?;
            let model = match &main {
                Some((current, model)) if *current == provider => model.as_str(),
                _ => "",
            };
            slots::suggest(
                slot,
                provider,
                model,
                &signed_in,
                &omc_providers::catalog_models,
            )
        };
        let main_name = main.as_ref().map(|(_, model)| model.as_str());
        Event::Slots(layers.infos(main_name, &ids, &suggest))
    }

    /// The presets as they stand.
    pub(super) fn presets_event(&self) -> Event {
        Event::Presets(slots::preset_infos(&self.shared.slot_layers().presets()))
    }

    /// Tells the client the slots after `changed`, the outcome of a change to them, or why it failed.
    fn after_slots(&self, changed: Result<(), String>) {
        self.say(match changed {
            Ok(()) => self.slots_event(),
            Err(reason) => Event::Failed(reason),
        });
    }

    /// Tells the client the presets after `changed`, the outcome of a change to them, or why it failed.
    fn after_presets(&self, changed: Result<(), String>) {
        self.say(match changed {
            Ok(()) => self.presets_event(),
            Err(reason) => Event::Failed(reason),
        });
    }

    /// Sets the assignment of `provider`, or the any-provider one when it is nothing, of the slot `name` in the file of `scope` to `model` and `backups`, and tells the client the slots.
    pub(super) fn set_slot(
        &self,
        name: &str,
        scope: SlotScope,
        provider: Option<&str>,
        model: Option<String>,
        backups: Vec<String>,
    ) {
        let provider = provider.map(str::trim).filter(|id| !id.is_empty());
        let changed =
            self.shared
                .slot_files()
                .set_slot(scope, name.trim(), provider, model, backups);
        self.after_slots(changed);
    }

    /// Takes the assignment of `provider`, or the any-provider one when it is nothing, of the slot `name` out of the file of `scope`, and tells the client the slots.
    pub(super) fn clear_slot(&self, name: &str, scope: SlotScope, provider: Option<&str>) {
        let provider = provider.map(str::trim).filter(|id| !id.is_empty());
        let changed = self
            .shared
            .slot_files()
            .clear_slot(scope, name.trim(), provider);
        self.after_slots(changed);
    }

    /// Saves every slot as it stands, with `effort` or else the conversation's, as the preset `name`, and tells the client the presets.
    pub(super) fn save_preset(&self, name: &str, effort: Option<String>) {
        let effort = effort
            .map(|effort| effort.trim().to_owned())
            .filter(|effort| !effort.is_empty())
            .or_else(|| {
                self.shared
                    .effort_now()
                    .map(|effort| effort.as_str().to_owned())
            });
        let preset = self.shared.slot_layers().snapshot(effort);
        let changed = self.shared.slot_files().save_preset(name.trim(), preset);
        self.after_presets(changed);
    }

    /// Takes the preset `name` out of the user's file, and tells the client the presets.
    pub(super) fn delete_preset(&self, name: &str) {
        let changed = self.shared.slot_files().delete_preset(name.trim());
        self.after_presets(changed);
    }

    /// Sets every slot of the file of `scope` to those of the preset `name`, then the conversation's effort to the preset's, and its model to the preset's `main` slot when it sets one, while a conversation is open; tells the client the slots.
    pub(super) async fn apply_preset(&mut self, name: &str, scope: SlotScope) {
        let name = name.trim();
        let Some(preset) = self.shared.slot_layers().presets().remove(name) else {
            return self.say(Event::Failed(format!("there is no preset {name}")));
        };
        if let Err(reason) = self.shared.slot_files().apply_preset(scope, &preset) {
            return self.say(Event::Failed(reason));
        }
        self.say(self.slots_event());
        if self.open.is_none() {
            return;
        }
        if preset.slots.get(slots::MAIN).is_some_and(|main| {
            main.any.model.is_some()
                || main
                    .providers
                    .values()
                    .any(|assignment| assignment.model.is_some())
        }) {
            self.set(MODEL, &format!("@{}", slots::MAIN)).await;
        }
        if let Some(effort) = &preset.effort {
            self.set(EFFORT, effort).await;
        }
    }

    /// The model `value` names for the open conversation and the effort it asks for, as [`conversation_model`] finds it.
    ///
    /// # Errors
    ///
    /// Same as [`conversation_model`]; also fails when no conversation is open and `value` names a slot.
    pub(super) fn conversation_model(
        &self,
        value: &str,
    ) -> Result<(String, Option<Effort>), String> {
        let Some((provider, current)) = self.shared.model() else {
            return Ok((value.trim().to_owned(), None));
        };
        conversation_model(
            &self
                .shared
                .slot_layers()
                .for_provider(Some(provider.def().id)),
            provider,
            &self.shared.models_listed(),
            Some(&current),
            value,
        )
    }
}

#[cfg(test)]
/// Tests of naming the conversation's model by a slot.
mod tests {
    use super::*;
    use crate::slots::{Assignment, SlotEntry, SlotsFile};

    /// Layers whose user file sets `fast` to `model`.
    fn fast(model: &str) -> Layers {
        let mut user = SlotsFile::default();
        user.slots.insert(
            slots::FAST.to_owned(),
            SlotEntry::any(Assignment {
                model: Some(model.to_owned()),
                backups: Vec::new(),
            }),
        );
        Layers {
            user,
            ..Layers::default()
        }
    }

    /// A slot's model of the conversation's provider is taken without the provider, with its effort; a plain model is kept as it is; a slot left to main keeps the model in use; another provider's model and an unknown slot fail.
    #[test]
    fn slots_name_the_conversation_model() {
        let layers = fast("claude/haiku:low");
        assert_eq!(
            conversation_model(&layers, Provider::Claude, &[], Some("opus"), "@fast").unwrap(),
            ("haiku".to_owned(), Some(Effort::Low))
        );
        assert_eq!(
            conversation_model(&layers, Provider::Claude, &[], Some("opus"), "sonnet").unwrap(),
            ("sonnet".to_owned(), None)
        );
        assert_eq!(
            conversation_model(&layers, Provider::Claude, &[], Some("opus"), "@plan").unwrap(),
            ("opus".to_owned(), None)
        );
        assert!(conversation_model(&layers, Provider::Claude, &[], None, "@plan").is_err());
        let error =
            conversation_model(&layers, Provider::Codex, &[], Some("gpt"), "@fast").unwrap_err();
        assert!(error.contains("a model of claude"), "{error}");
        let error =
            conversation_model(&layers, Provider::Claude, &[], Some("opus"), "@nope").unwrap_err();
        assert!(error.contains("no model slot @nope"), "{error}");
        let listed = vec!["claude/haiku".to_owned()];
        assert_eq!(
            conversation_model(&layers, Provider::Claude, &listed, Some("opus"), "@fast")
                .unwrap()
                .0,
            "claude/haiku"
        );
    }
}
