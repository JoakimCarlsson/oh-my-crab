//! Model slots: the roles a conversation asks a model for, the files they are kept in, and the presets that set them all at once.
//!
//! A slot holds a model selector, `provider/model` or `model` with an optional
//! `:effort`, and the selectors of its backups. The user's file,
//! `slots.json` in crab's config folder or the file [`FILE_VARIABLE`] names,
//! holds the user's slots and presets; a trusted project's
//! `.oh-my-crab/slots.json` overlays it slot by slot, and an
//! `OMC_MODEL_<SLOT>` variable sets a slot's any-provider model over both.
//! Each slot holds an assignment per provider and one for any provider; a
//! conversation uses its provider's, else the any-provider one, else `main`,
//! which is the conversation's model. Anywhere a
//! model is named, `@name` names the model of the slot `name`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use omc_providers::{Effort, ModelInfo, Provider};
use omc_wire::{PresetInfo, PresetSlot, SlotInfo, SlotProvider, SlotScope, SlotSource};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The variable that names the user's slots file, instead of `slots.json` under `oh-my-crab` in the user's config folder.
pub const FILE_VARIABLE: &str = "OMC_SLOTS_FILE";

/// What the variable that sets a slot's model starts with; the slot's name follows in upper case, as in `OMC_MODEL_FAST`.
pub const VARIABLE_PREFIX: &str = "OMC_MODEL_";

/// The project's slots file, under the working folder.
pub(crate) const PROJECT_FILE: &str = ".oh-my-crab/slots.json";

/// The slot of the conversation's own model.
pub(crate) const MAIN: &str = "main";

/// The slot of summaries, side questions and hand-off prompts.
pub(crate) const FAST: &str = "fast";

/// The slot of turns in plan mode.
pub(crate) const PLAN: &str = "plan";

/// The slot of subagents whose kind names no model.
pub(crate) const SUBAGENT: &str = "subagent";

/// The slot of reviews, such as judging a goal; reserved until crab asks a model for one.
pub(crate) const REVIEW: &str = "review";

/// The slot of describing images for a model that cannot read them; reserved until crab does.
pub(crate) const VISION: &str = "vision";

/// The slots every conversation has, in the order they are listed.
pub(crate) const BUILT_IN: [&str; 6] = [MAIN, FAST, PLAN, SUBAGENT, REVIEW, VISION];

/// A model as a selector names it: the model, with its provider before a `/` when given, and the effort after a `:` when given.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Selector {
    /// The model, as `provider/model` or `model`.
    pub(crate) model: String,
    /// The effort it is asked to think at, when the selector gives one.
    pub(crate) effort: Option<Effort>,
}

/// What a model name names: a slot, written `@name`, or a model.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Named {
    /// The slot of this name.
    Slot(String),
    /// This model.
    Model(Selector),
}

/// Checks that `name` can name a slot or a preset: letters, digits, `-` and `_`, at least one.
///
/// # Errors
///
/// Fails, saying why, when it cannot.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        Ok(())
    } else {
        Err(format!(
            "{name:?} is not a valid name; use letters, digits, - and _"
        ))
    }
}

/// Reads the selector `text`: `provider/model` or `model`, with `:effort` after it when the part after the last `:` is an effort, so that ids holding a `:` of their own still read.
///
/// # Errors
///
/// Fails, saying why, when it is empty, holds a space, or names a slot.
pub(crate) fn parse_selector(text: &str) -> Result<Selector, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("no model was named".to_owned());
    }
    if text.starts_with('@') {
        return Err(format!(
            "{text} names a slot; a slot's model and backups must name models"
        ));
    }
    if text.contains(char::is_whitespace) {
        return Err(format!("{text:?} is not a model selector"));
    }
    let (model, effort) = match text.rsplit_once(':') {
        Some((model, effort)) if Effort::parse(effort).is_some() => (model, Effort::parse(effort)),
        _ => (text, None),
    };
    if model.is_empty() || model.ends_with('/') {
        return Err(format!("{text:?} names no model"));
    }
    Ok(Selector {
        model: model.to_owned(),
        effort,
    })
}

/// Reads the model name `text`: `@name` for a slot, else a selector.
///
/// # Errors
///
/// Fails, saying why, when the slot's name or the selector is not valid.
pub(crate) fn parse_named(text: &str) -> Result<Named, String> {
    let text = text.trim();
    match text.strip_prefix('@') {
        Some(name) => check_name(name).map(|()| Named::Slot(name.to_owned())),
        None => parse_selector(text).map(Named::Model),
    }
}

/// What one file sets for a slot.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Assignment {
    /// The model selector; nothing leaves the model to a lower layer or the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) model: Option<String>,
    /// The selectors tried in order when the model fails.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) backups: Vec<String>,
}

impl Assignment {
    /// Whether it sets nothing.
    fn is_empty(&self) -> bool {
        self.model.is_none() && self.backups.is_empty()
    }

    /// Checks every selector it holds.
    ///
    /// # Errors
    ///
    /// Fails, saying which, when one is not valid.
    fn check(&self) -> Result<(), String> {
        for selector in self.model.iter().chain(&self.backups) {
            parse_selector(selector)?;
        }
        Ok(())
    }
}

/// What one file sets for a slot: the any-provider assignment, written flat as the slot's own `model` and `backups`, and an assignment for each provider under `providers`, by provider id.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SlotEntry {
    /// The assignment used for every provider that has none of its own.
    #[serde(flatten)]
    pub(crate) any: Assignment,
    /// The assignments of single providers, by provider id, such as `claude`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) providers: BTreeMap<String, Assignment>,
}

impl SlotEntry {
    /// The entry that sets only `any`, the any-provider assignment.
    #[cfg(test)]
    pub(crate) fn any(any: Assignment) -> Self {
        Self {
            any,
            providers: BTreeMap::new(),
        }
    }

    /// Whether it sets nothing for any provider.
    fn is_empty(&self) -> bool {
        self.any.is_empty() && self.providers.values().all(Assignment::is_empty)
    }

    /// The assignment of `provider`, or the any-provider one when `provider` is nothing.
    fn get(&self, provider: Option<&str>) -> Option<&Assignment> {
        match provider {
            Some(provider) => self.providers.get(provider),
            None => Some(&self.any),
        }
        .filter(|assignment| !assignment.is_empty())
    }

    /// Checks every provider id and selector it holds.
    ///
    /// # Errors
    ///
    /// Fails, saying which, when one is not valid.
    fn check(&self) -> Result<(), String> {
        self.any.check()?;
        for (provider, assignment) in &self.providers {
            check_name(provider)
                .and_then(|()| assignment.check())
                .map_err(|reason| format!("provider {provider}: {reason}"))?;
        }
        Ok(())
    }
}

/// A named snapshot of the slots and the default effort.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct Preset {
    /// The slots it sets, by name.
    #[serde(default)]
    pub(crate) slots: BTreeMap<String, SlotEntry>,
    /// The effort it sets, such as `high`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) effort: Option<String>,
}

/// A slots file as it is stored.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub(crate) struct SlotsFile {
    /// The slots it sets, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) slots: BTreeMap<String, SlotEntry>,
    /// The presets it keeps, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) presets: BTreeMap<String, Preset>,
    /// Whatever else it holds, kept as it is.
    #[serde(flatten)]
    other: Map<String, Value>,
}

/// A slot as the layers set it for one provider.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Effective {
    /// The model and backups that apply.
    pub(crate) assignment: Assignment,
    /// Where the model comes from; nothing when no layer sets one.
    pub(crate) source: Option<SlotSource>,
}

/// The slots as every layer sets them: the user's file, the project's over it, and the environment over both, resolved for the conversation's provider.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Layers {
    /// The user's file.
    pub(crate) user: SlotsFile,
    /// The project's file, when the project is trusted and has one.
    pub(crate) project: Option<SlotsFile>,
    /// The models the environment sets for the any-provider assignment, by slot name in lower case.
    pub(crate) env: BTreeMap<String, String>,
    /// The id of the provider slots are resolved for, the conversation's; nothing uses only the any-provider assignments.
    pub(crate) provider: Option<String>,
}

impl Layers {
    /// The layers of `user` and `project`, with the models the environment's `OMC_MODEL_<SLOT>` variables set.
    pub(crate) fn with_env(user: SlotsFile, project: Option<SlotsFile>) -> Self {
        Self {
            user,
            project,
            env: env_models(std::env::vars()),
            provider: None,
        }
    }

    /// The same layers, resolved for the provider of id `provider`.
    pub(crate) fn for_provider(mut self, provider: Option<&str>) -> Self {
        self.provider = provider.map(str::to_owned);
        self
    }

    /// The files, the user's then the project's.
    fn files(&self) -> impl Iterator<Item = &SlotsFile> {
        std::iter::once(&self.user).chain(self.project.iter())
    }

    /// Every slot's name: the built-in ones first, then the custom ones any layer sets, by name.
    pub(crate) fn names(&self) -> Vec<String> {
        let mut custom: Vec<String> = self
            .files()
            .flat_map(|file| file.slots.keys())
            .chain(self.env.keys())
            .filter(|name| !BUILT_IN.contains(&name.as_str()))
            .cloned()
            .collect();
        custom.sort();
        custom.dedup();
        BUILT_IN
            .iter()
            .map(|name| (*name).to_owned())
            .chain(custom)
            .collect()
    }

    /// Whether `name` is a slot: a built-in one, or one a layer sets.
    pub(crate) fn knows(&self, name: &str) -> bool {
        self.names().iter().any(|known| known == name)
    }

    /// The ids of the providers any layer gives the slot `name` an assignment of its own, by id.
    fn providers_of(&self, name: &str) -> Vec<String> {
        let mut ids: Vec<String> = self
            .files()
            .filter_map(|file| file.slots.get(name))
            .flat_map(|entry| entry.providers.keys())
            .cloned()
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }

    /// What the layers set for the slot `name` in the assignment of `provider`, or the any-provider assignment when `provider` is nothing: the project's over the user's, and for the any-provider assignment the environment's model over both.
    pub(crate) fn assignment(&self, name: &str, provider: Option<&str>) -> Effective {
        let project = self
            .project
            .as_ref()
            .and_then(|project| project.slots.get(name))
            .and_then(|entry| entry.get(provider));
        let user = self
            .user
            .slots
            .get(name)
            .and_then(|entry| entry.get(provider));
        let (mut assignment, mut source) = match (project, user) {
            (Some(entry), _) => (entry.clone(), Some(SlotSource::Project)),
            (None, Some(entry)) => (entry.clone(), Some(SlotSource::User)),
            (None, None) => (Assignment::default(), None),
        };
        if let Some(model) = self.env.get(name).filter(|_| provider.is_none()) {
            assignment.model = Some(model.clone());
            source = Some(SlotSource::Environment);
        }
        if assignment.model.is_none() {
            source = None;
        }
        Effective { assignment, source }
    }

    /// What applies to the slot `name` for the provider of id `provider`: its own assignment when a layer sets one, else the any-provider assignment.
    pub(crate) fn effective_for(&self, name: &str, provider: Option<&str>) -> Effective {
        if let Some(provider) = provider {
            let own = self.assignment(name, Some(provider));
            if !own.assignment.is_empty() {
                return own;
            }
        }
        self.assignment(name, None)
    }

    /// What applies to the slot `name` for the conversation's provider, as [`Layers::effective_for`] finds it.
    pub(crate) fn effective(&self, name: &str) -> Effective {
        self.effective_for(name, self.provider.as_deref())
    }

    /// The selector of the model the slot `name` sets for the conversation's provider, or nothing when it is left to the conversation's model.
    ///
    /// # Errors
    ///
    /// Fails, listing the slots there are, when there is no slot `name`, and saying why when its model is not a valid selector.
    pub(crate) fn model_of(&self, name: &str) -> Result<Option<Selector>, String> {
        if !self.knows(name) {
            return Err(format!(
                "there is no model slot @{name}; the slots are {}",
                self.names()
                    .iter()
                    .map(|name| format!("@{name}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if name == MAIN {
            return Ok(None);
        }
        self.effective(name)
            .assignment
            .model
            .as_deref()
            .map(parse_selector)
            .transpose()
    }

    /// The model the `main` slot sets for the conversation's provider, which a conversation opened without a model starts with; nothing when it sets none.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when its selector is not valid.
    pub(crate) fn default_model(&self) -> Result<Option<Selector>, String> {
        self.effective(MAIN)
            .assignment
            .model
            .as_deref()
            .map(parse_selector)
            .transpose()
    }

    /// The backups of the slot `name` for the conversation's provider, in order: those of the assignment that applies.
    pub(crate) fn backups_of(&self, name: &str) -> Vec<String> {
        self.effective(name).assignment.backups
    }

    /// The presets kept: the user's, with the project's over them by name.
    pub(crate) fn presets(&self) -> BTreeMap<String, Preset> {
        let mut presets = self.user.presets.clone();
        if let Some(project) = &self.project {
            presets.extend(project.presets.clone());
        }
        presets
    }

    /// The slots as the client is shown them: each with its any-provider model and backups, the model it resolves to for the conversation's provider with `main`, the conversation's model when it is open, where that comes from, and the assignment of every provider in `signed_in` and of every provider a layer sets one for; `suggest` gives the suggestion for a slot and a provider id when the provider has no model for it.
    pub(crate) fn infos(
        &self,
        main: Option<&str>,
        signed_in: &[String],
        suggest: &dyn Fn(&str, &str) -> Option<String>,
    ) -> Vec<SlotInfo> {
        self.names()
            .into_iter()
            .map(|name| {
                let any = self.assignment(&name, None).assignment;
                let effective = self.effective(&name);
                let own = effective.assignment.model.clone().filter(|_| name != MAIN);
                let inherited = own.is_none();
                let mut ids: Vec<String> = signed_in.to_vec();
                ids.extend(self.provider.clone());
                ids.extend(self.providers_of(&name));
                let mut seen = std::collections::BTreeSet::new();
                ids.retain(|id| seen.insert(id.clone()));
                let providers = ids
                    .into_iter()
                    .map(|provider| {
                        let assignment = self.assignment(&name, Some(&provider)).assignment;
                        SlotProvider {
                            suggested: if assignment.model.is_none() && name != MAIN {
                                suggest(&name, &provider)
                            } else {
                                None
                            },
                            model: assignment.model,
                            backups: assignment.backups,
                            provider,
                        }
                    })
                    .collect();
                SlotInfo {
                    resolved: own.clone().or_else(|| main.map(str::to_owned)),
                    source: if inherited {
                        SlotSource::Main
                    } else {
                        effective.source.unwrap_or(SlotSource::Main)
                    },
                    inherited,
                    suggested: match (&self.provider, inherited) {
                        (Some(provider), true) => suggest(&name, provider),
                        _ => None,
                    },
                    model: any.model,
                    backups: any.backups,
                    providers,
                    name,
                }
            })
            .collect()
    }

    /// Every slot as it stands now, every assignment of it, as a preset keeps it, with `effort`.
    pub(crate) fn snapshot(&self, effort: Option<String>) -> Preset {
        Preset {
            slots: self
                .names()
                .into_iter()
                .filter_map(|name| {
                    let entry = SlotEntry {
                        any: self.assignment(&name, None).assignment,
                        providers: self
                            .providers_of(&name)
                            .into_iter()
                            .map(|provider| {
                                let assignment = self.assignment(&name, Some(&provider)).assignment;
                                (provider, assignment)
                            })
                            .filter(|(_, assignment)| !assignment.is_empty())
                            .collect(),
                    };
                    (!entry.is_empty()).then_some((name, entry))
                })
                .collect(),
            effort,
        }
    }
}

/// The wire form of `presets`, by name.
pub(crate) fn preset_infos(presets: &BTreeMap<String, Preset>) -> Vec<PresetInfo> {
    presets
        .iter()
        .map(|(name, preset)| PresetInfo {
            name: name.clone(),
            slots: preset
                .slots
                .iter()
                .map(|(slot, entry)| PresetSlot {
                    name: slot.clone(),
                    model: entry.any.model.clone(),
                    backups: entry.any.backups.clone(),
                    providers: entry
                        .providers
                        .iter()
                        .map(|(provider, assignment)| SlotProvider {
                            provider: provider.clone(),
                            model: assignment.model.clone(),
                            backups: assignment.backups.clone(),
                            suggested: None,
                        })
                        .collect(),
                })
                .collect(),
            effort: preset.effort.clone(),
        })
        .collect()
}

/// The models `vars`, the environment, sets for slots with `OMC_MODEL_<SLOT>`, by slot name in lower case; empty values and names that are not valid are left out.
fn env_models(vars: impl Iterator<Item = (String, String)>) -> BTreeMap<String, String> {
    vars.filter_map(|(key, value)| {
        let name = key.strip_prefix(VARIABLE_PREFIX)?.to_ascii_lowercase();
        let value = value.trim().to_owned();
        (check_name(&name).is_ok() && !value.is_empty()).then_some((name, value))
    })
    .collect()
}

/// Where the slots files are.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct SlotFiles {
    /// The user's file, when there is a config folder or [`FILE_VARIABLE`] names one.
    pub(crate) user: Option<PathBuf>,
    /// The project's file, when a trusted project is open.
    pub(crate) project: Option<PathBuf>,
}

impl SlotFiles {
    /// The user's file, as [`FILE_VARIABLE`] or the config folder places it, and the project's under `project`, the working folder of a trusted project.
    pub(crate) fn locate(project: Option<&Path>) -> Self {
        let user = match std::env::var_os(FILE_VARIABLE) {
            Some(path) if !path.is_empty() => Some(PathBuf::from(path)),
            _ => dirs::config_dir().map(|config| config.join("oh-my-crab").join("slots.json")),
        };
        Self {
            user,
            project: project.map(|folder| folder.join(PROJECT_FILE)),
        }
    }

    /// The layers the files and the environment set; a file that is missing sets nothing.
    ///
    /// # Errors
    ///
    /// Fails, naming the file, when one cannot be read or is not valid.
    pub(crate) fn load(&self) -> Result<Layers, String> {
        let user = match &self.user {
            Some(path) => read(path)?,
            None => SlotsFile::default(),
        };
        let project = self.project.as_deref().map(read).transpose()?;
        Ok(Layers::with_env(user, project))
    }

    /// The file of `scope`.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when there is no such file: no config folder for the user's, no trusted project open for the project's.
    fn path(&self, scope: SlotScope) -> Result<&Path, String> {
        match scope {
            SlotScope::User => self.user.as_deref().ok_or_else(|| {
                format!("there is no config folder; set {FILE_VARIABLE} to a file to keep slots in")
            }),
            SlotScope::Project => self.project.as_deref().ok_or_else(|| {
                "no trusted project is open, so there is no project file to keep slots in"
                    .to_owned()
            }),
        }
    }

    /// Reads the file of `scope`, lets `change` change it, and writes it back whole; nothing is written when `change` fails.
    ///
    /// # Errors
    ///
    /// Fails, saying why, when the file cannot be read or written, or `change` fails.
    fn change(
        &self,
        scope: SlotScope,
        change: impl FnOnce(&mut SlotsFile) -> Result<(), String>,
    ) -> Result<(), String> {
        let path = self.path(scope)?;
        let mut file = read(path)?;
        change(&mut file)?;
        write(path, &file)
    }

    /// Sets the assignment of `provider`, or the any-provider one when it is nothing, of the slot `name` in the file of `scope` to `model` and `backups`, taking it out when both are empty and the slot out when it then sets nothing.
    ///
    /// # Errors
    ///
    /// Fails, writing nothing, when the name, the provider id or a selector is not valid, or the file cannot be read or written.
    pub(crate) fn set_slot(
        &self,
        scope: SlotScope,
        name: &str,
        provider: Option<&str>,
        model: Option<String>,
        backups: Vec<String>,
    ) -> Result<(), String> {
        check_name(name)?;
        if let Some(provider) = provider {
            check_name(provider)?;
        }
        let assignment = Assignment {
            model: model
                .map(|model| model.trim().to_owned())
                .filter(|model| !model.is_empty()),
            backups: backups
                .into_iter()
                .map(|backup| backup.trim().to_owned())
                .filter(|backup| !backup.is_empty())
                .collect(),
        };
        assignment.check()?;
        self.change(scope, |file| {
            let entry = file.slots.entry(name.to_owned()).or_default();
            match provider {
                Some(provider) if assignment.is_empty() => {
                    entry.providers.remove(provider);
                }
                Some(provider) => {
                    entry.providers.insert(provider.to_owned(), assignment);
                }
                None => entry.any = assignment,
            }
            if entry.is_empty() {
                file.slots.remove(name);
            }
            Ok(())
        })
    }

    /// Takes the assignment of `provider`, or the any-provider one when it is nothing, of the slot `name` out of the file of `scope`, and the slot when it then sets nothing.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or written.
    pub(crate) fn clear_slot(
        &self,
        scope: SlotScope,
        name: &str,
        provider: Option<&str>,
    ) -> Result<(), String> {
        self.change(scope, |file| {
            if let Some(entry) = file.slots.get_mut(name) {
                match provider {
                    Some(provider) => {
                        entry.providers.remove(provider);
                    }
                    None => entry.any = Assignment::default(),
                }
                if entry.is_empty() {
                    file.slots.remove(name);
                }
            }
            Ok(())
        })
    }

    /// Keeps `preset` in the user's file as `name`, in place of one of that name.
    ///
    /// # Errors
    ///
    /// Fails when the name or the effort is not valid, or the file cannot be read or written.
    pub(crate) fn save_preset(&self, name: &str, preset: Preset) -> Result<(), String> {
        check_name(name)?;
        if let Some(effort) = preset
            .effort
            .as_deref()
            .filter(|effort| Effort::parse(effort).is_none())
        {
            return Err(format!("{effort} is not an effort"));
        }
        self.change(SlotScope::User, |file| {
            file.presets.insert(name.to_owned(), preset);
            Ok(())
        })
    }

    /// Takes the preset `name` out of the user's file.
    ///
    /// # Errors
    ///
    /// Fails when there is no such preset, or the file cannot be read or written.
    pub(crate) fn delete_preset(&self, name: &str) -> Result<(), String> {
        self.change(SlotScope::User, |file| {
            file.presets
                .remove(name)
                .map(|_| ())
                .ok_or_else(|| format!("there is no preset {name}"))
        })
    }

    /// Replaces every slot of the file of `scope` with those of `preset`.
    ///
    /// # Errors
    ///
    /// Fails when the file cannot be read or written.
    pub(crate) fn apply_preset(&self, scope: SlotScope, preset: &Preset) -> Result<(), String> {
        self.change(scope, |file| {
            file.slots = preset.slots.clone();
            Ok(())
        })
    }
}

/// Reads the slots file at `path`; a missing file sets nothing.
///
/// # Errors
///
/// Fails, naming the file, when it cannot be read or is not valid.
fn read(path: &Path) -> Result<SlotsFile, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(SlotsFile::default());
        }
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(SlotsFile::default());
    }
    let file: SlotsFile =
        serde_json::from_str(&text).map_err(|error| format!("{}: {error}", path.display()))?;
    for (name, entry) in &file.slots {
        check_name(name)
            .and_then(|()| entry.check())
            .map_err(|reason| format!("{}: slot {name}: {reason}", path.display()))?;
    }
    Ok(file)
}

/// Writes `file` to `path` whole, through a file beside it, making its folder when there is none.
///
/// # Errors
///
/// Fails, naming the file, when it cannot be written.
fn write(path: &Path, file: &SlotsFile) -> Result<(), String> {
    let failed = |error: std::io::Error| format!("{}: {error}", path.display());
    if let Some(folder) = path
        .parent()
        .filter(|folder| !folder.as_os_str().is_empty())
    {
        std::fs::create_dir_all(folder).map_err(failed)?;
    }
    let text = serde_json::to_string_pretty(file).map_err(|error| error.to_string())?;
    let mut staging = path.as_os_str().to_owned();
    staging.push(".writing");
    let staging = PathBuf::from(staging);
    std::fs::write(&staging, format!("{text}\n")).map_err(failed)?;
    std::fs::rename(&staging, path).map_err(failed)
}

/// The model the catalog suggests for the slot `slot`, which has no model of its own, as `provider/model`: for `fast` the cheapest of `provider`'s current models that take tool calls, other than `main`; for `vision`, when `main` takes no images, the cheapest that takes images, of `provider` first and then of the providers `signed_in`. `catalog` gives a provider's current models that take tool calls. Nothing for other slots, or when nothing fits.
pub(crate) fn suggest(
    slot: &str,
    provider: Provider,
    main: &str,
    signed_in: &[Provider],
    catalog: &dyn Fn(Provider) -> Vec<(String, ModelInfo)>,
) -> Option<String> {
    let price = |info: &ModelInfo| {
        info.pricing
            .as_ref()
            .map(|price| price.input + price.output)
    };
    let cheapest = |provider: Provider, fits: &dyn Fn(&str, &ModelInfo) -> bool| {
        catalog(provider)
            .into_iter()
            .filter(|(id, info)| fits(id, info))
            .filter_map(|(id, info)| price(&info).map(|price| (price, id)))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, id)| format!("{}/{id}", provider.def().id))
    };
    match slot {
        FAST => cheapest(provider, &|id, _| !id.eq_ignore_ascii_case(main)),
        VISION => {
            let main_sees = catalog(provider)
                .iter()
                .any(|(id, info)| id.eq_ignore_ascii_case(main) && info.takes_images());
            if main_sees {
                return None;
            }
            std::iter::once(provider)
                .chain(signed_in.iter().copied().filter(|other| *other != provider))
                .find_map(|candidate| cheapest(candidate, &|_, info| info.takes_images()))
        }
        _ => None,
    }
}

#[cfg(test)]
/// Tests of slot names, selectors, layers, presets and suggestions.
mod tests {
    use super::*;

    /// A scratch folder for one test, emptied first.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../build/tests/omc-session/slots")
            .join(name);
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The assignment of `model` with `backups`.
    fn assigned(model: &str, backups: &[&str]) -> Assignment {
        Assignment {
            model: Some(model.to_owned()),
            backups: backups.iter().map(|backup| (*backup).to_owned()).collect(),
        }
    }

    /// The entry that sets only the any-provider assignment of `model` with `backups`.
    fn any(model: &str, backups: &[&str]) -> SlotEntry {
        SlotEntry::any(assigned(model, backups))
    }

    /// Selectors read their provider, model and effort; a `:` that is not followed by an effort stays part of the id; slot references read their name.
    #[test]
    fn selectors_and_slot_references_read() {
        assert_eq!(
            parse_selector("claude/haiku:low").unwrap(),
            Selector {
                model: "claude/haiku".to_owned(),
                effort: Some(Effort::Low)
            }
        );
        assert_eq!(
            parse_selector(" ollama/llama3:8b ").unwrap(),
            Selector {
                model: "ollama/llama3:8b".to_owned(),
                effort: None
            }
        );
        assert_eq!(
            parse_selector("gpt-5:high").unwrap().effort,
            Some(Effort::High)
        );
        assert!(parse_selector("").is_err());
        assert!(parse_selector("@fast").is_err());
        assert!(parse_selector("a b").is_err());
        assert!(parse_selector(":high").is_err());
        assert!(parse_selector("claude/").is_err());
        assert_eq!(
            parse_named("@fast").unwrap(),
            Named::Slot("fast".to_owned())
        );
        assert_eq!(
            parse_named("@my-slot_2").unwrap(),
            Named::Slot("my-slot_2".to_owned())
        );
        assert!(parse_named("@").is_err());
        assert!(parse_named("@no.dots").is_err());
        assert_eq!(
            parse_named("haiku").unwrap(),
            Named::Model(Selector {
                model: "haiku".to_owned(),
                effort: None
            })
        );
    }

    /// The project's entry wins over the user's slot by slot, the environment's model wins over both, and a slot nothing sets is left to main.
    #[test]
    fn layers_overlay_slot_by_slot() {
        let mut user = SlotsFile::default();
        user.slots
            .insert(FAST.to_owned(), any("claude/haiku", &["openai/gpt-5-mini"]));
        user.slots.insert(PLAN.to_owned(), any("claude/opus", &[]));
        let mut project = SlotsFile::default();
        project
            .slots
            .insert(PLAN.to_owned(), any("openai/gpt-5", &["claude/opus"]));
        let mut layers = Layers {
            user,
            project: Some(project),
            env: BTreeMap::new(),
            provider: None,
        };
        assert_eq!(layers.effective(FAST).source, Some(SlotSource::User));
        assert_eq!(layers.effective(PLAN).source, Some(SlotSource::Project));
        assert_eq!(layers.backups_of(PLAN), ["claude/opus"]);
        assert_eq!(
            layers.model_of(PLAN).unwrap().unwrap().model,
            "openai/gpt-5"
        );
        assert_eq!(layers.model_of(SUBAGENT).unwrap(), None);
        assert_eq!(layers.model_of(MAIN).unwrap(), None);
        layers.env =
            env_models([("OMC_MODEL_FAST".to_owned(), "groq/llama".to_owned())].into_iter());
        let fast = layers.effective(FAST);
        assert_eq!(fast.source, Some(SlotSource::Environment));
        assert_eq!(
            fast.assignment,
            assigned("groq/llama", &["openai/gpt-5-mini"])
        );
        let error = layers.model_of("nope").unwrap_err();
        assert!(error.contains("no model slot @nope"), "{error}");
        assert!(error.contains("@vision"), "{error}");
    }

    /// The slots shown say which are set and which are left to main, resolving those to the conversation's model, with suggestions only for those.
    #[test]
    fn empty_slots_fall_back_to_main() {
        let mut user = SlotsFile::default();
        user.slots.insert(FAST.to_owned(), any("claude/haiku", &[]));
        user.slots
            .insert("docs".to_owned(), any("claude/sonnet", &[]));
        let layers = Layers {
            user,
            ..Layers::default()
        };
        let layers = layers.for_provider(Some("claude"));
        let infos = layers.infos(Some("claude-opus"), &[], &|name, _| {
            Some(format!("suggested-{name}"))
        });
        let names: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
        assert_eq!(names, [MAIN, FAST, PLAN, SUBAGENT, REVIEW, VISION, "docs"]);
        let fast = &infos[1];
        assert!(!fast.inherited);
        assert_eq!(fast.resolved.as_deref(), Some("claude/haiku"));
        assert_eq!(fast.source, SlotSource::User);
        assert_eq!(fast.suggested, None);
        let plan = &infos[2];
        assert!(plan.inherited);
        assert_eq!(plan.resolved.as_deref(), Some("claude-opus"));
        assert_eq!(plan.source, SlotSource::Main);
        assert_eq!(plan.suggested.as_deref(), Some("suggested-plan"));
        assert!(layers.knows("docs"));
        let before_open = layers.infos(None, &[], &|_, _| None);
        assert_eq!(before_open[2].resolved, None);
    }

    /// Slots are set and cleared in the file of their scope, presets snapshot every slot and the effort, applying one sets every slot of a scope, and deleting one takes it out.
    #[test]
    fn slots_and_presets_are_kept_in_their_files() {
        let dir = scratch("files");
        let files = SlotFiles {
            user: Some(dir.join("user/slots.json")),
            project: Some(dir.join("project").join(PROJECT_FILE)),
        };
        files
            .set_slot(
                SlotScope::User,
                FAST,
                None,
                Some("claude/haiku:low".into()),
                vec!["openai/gpt-5-mini".into()],
            )
            .unwrap();
        files
            .set_slot(
                SlotScope::Project,
                FAST,
                None,
                Some("openai/gpt-5-nano".into()),
                Vec::new(),
            )
            .unwrap();
        assert!(
            files
                .set_slot(SlotScope::User, "bad name", None, None, Vec::new())
                .is_err()
        );
        assert!(
            files
                .set_slot(
                    SlotScope::User,
                    PLAN,
                    None,
                    Some("@fast".into()),
                    Vec::new()
                )
                .is_err()
        );
        let layers = files.load().unwrap();
        assert_eq!(layers.effective(FAST).source, Some(SlotSource::Project));
        files.clear_slot(SlotScope::Project, FAST, None).unwrap();
        let layers = files.load().unwrap();
        assert_eq!(
            layers.model_of(FAST).unwrap().unwrap(),
            Selector {
                model: "claude/haiku".to_owned(),
                effort: Some(Effort::Low)
            }
        );
        files
            .save_preset("cheap", layers.snapshot(Some("low".to_owned())))
            .unwrap();
        assert!(
            files
                .save_preset("odd", layers.snapshot(Some("loud".to_owned())))
                .is_err()
        );
        files
            .set_slot(SlotScope::User, FAST, None, None, Vec::new())
            .unwrap();
        files
            .set_slot(
                SlotScope::User,
                PLAN,
                None,
                Some("claude/opus".into()),
                Vec::new(),
            )
            .unwrap();
        let layers = files.load().unwrap();
        assert_eq!(layers.model_of(FAST).unwrap(), None);
        let preset = layers.presets()["cheap"].clone();
        assert_eq!(preset.effort.as_deref(), Some("low"));
        files.apply_preset(SlotScope::User, &preset).unwrap();
        let layers = files.load().unwrap();
        assert_eq!(
            layers.model_of(FAST).unwrap().unwrap().model,
            "claude/haiku"
        );
        assert_eq!(layers.model_of(PLAN).unwrap(), None);
        let infos = preset_infos(&layers.presets());
        assert_eq!(infos[0].name, "cheap");
        assert_eq!(infos[0].slots[0].backups, ["openai/gpt-5-mini"]);
        files.delete_preset("cheap").unwrap();
        assert!(files.delete_preset("cheap").is_err());
        assert!(files.load().unwrap().presets().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A model of the given input kinds and price per million tokens in and out.
    fn info(images: bool, price: f64) -> ModelInfo {
        ModelInfo {
            context_window: 100_000,
            max_output: 1_000,
            reasoning: false,
            efforts: Vec::new(),
            input: if images {
                vec!["text".into(), "image".into()]
            } else {
                vec!["text".into()]
            },
            pricing: Some(omc_providers::Pricing {
                input: price,
                output: price,
                cache_read: 0.0,
                cache_write: 0.0,
                cache_write_long: 0.0,
            }),
        }
    }

    /// `fast` suggests the cheapest other model of the main provider; `vision` the cheapest that takes images, looking at other signed-in providers when the main one has none, and nothing when the main model takes images; other slots get no suggestion.
    #[test]
    fn suggestions_come_from_the_catalog() {
        let catalog = |provider: Provider| match provider {
            Provider::Claude => vec![
                ("big".to_owned(), info(false, 10.0)),
                ("small".to_owned(), info(false, 1.0)),
                ("tiny".to_owned(), info(false, 0.5)),
            ],
            Provider::Codex => vec![("eyes".to_owned(), info(true, 2.0))],
            _ => Vec::new(),
        };
        assert_eq!(
            suggest(FAST, Provider::Claude, "big", &[], &catalog).as_deref(),
            Some("claude/tiny")
        );
        assert_eq!(
            suggest(FAST, Provider::Claude, "tiny", &[], &catalog).as_deref(),
            Some("claude/small")
        );
        assert_eq!(
            suggest(VISION, Provider::Claude, "big", &[], &catalog),
            None
        );
        assert_eq!(
            suggest(
                VISION,
                Provider::Claude,
                "big",
                &[Provider::Codex],
                &catalog
            )
            .as_deref(),
            Some("codex/eyes")
        );
        assert_eq!(
            suggest(VISION, Provider::Codex, "eyes", &[], &catalog),
            None
        );
        assert_eq!(suggest(PLAN, Provider::Claude, "big", &[], &catalog), None);
    }

    /// A provider's own assignment comes first, then the any-provider one, then main, with the backups of the assignment that applies; a Claude conversation and a Codex one get different `fast` models, and the environment overrides only the any-provider model.
    #[test]
    fn slots_resolve_for_the_conversation_provider() {
        let mut fast = any("openai/gpt-5-mini", &["openai/gpt-5-nano"]);
        fast.providers.insert(
            "claude".to_owned(),
            assigned("claude/haiku", &["claude/sonnet"]),
        );
        fast.providers
            .insert("codex".to_owned(), assigned("codex/gpt-5-mini", &[]));
        let mut plan = SlotEntry::default();
        plan.providers
            .insert("claude".to_owned(), assigned("claude/opus", &[]));
        let mut user = SlotsFile::default();
        user.slots.insert(FAST.to_owned(), fast);
        user.slots.insert(PLAN.to_owned(), plan);
        let mut layers = Layers {
            user,
            ..Layers::default()
        };
        let claude = layers.clone().for_provider(Some("claude"));
        let codex = layers.clone().for_provider(Some("codex"));
        let other = layers.clone().for_provider(Some("groq"));
        assert_eq!(
            claude.model_of(FAST).unwrap().unwrap().model,
            "claude/haiku"
        );
        assert_eq!(claude.backups_of(FAST), ["claude/sonnet"]);
        assert_eq!(
            codex.model_of(FAST).unwrap().unwrap().model,
            "codex/gpt-5-mini"
        );
        assert!(codex.backups_of(FAST).is_empty());
        assert_eq!(
            other.model_of(FAST).unwrap().unwrap().model,
            "openai/gpt-5-mini"
        );
        assert_eq!(other.backups_of(FAST), ["openai/gpt-5-nano"]);
        assert_eq!(claude.model_of(PLAN).unwrap().unwrap().model, "claude/opus");
        assert_eq!(codex.model_of(PLAN).unwrap(), None);
        layers.env =
            env_models([("OMC_MODEL_FAST".to_owned(), "groq/llama".to_owned())].into_iter());
        let claude = layers.clone().for_provider(Some("claude"));
        let other = layers.for_provider(Some("groq"));
        assert_eq!(
            claude.model_of(FAST).unwrap().unwrap().model,
            "claude/haiku"
        );
        assert_eq!(other.effective(FAST).source, Some(SlotSource::Environment));
        assert_eq!(other.model_of(FAST).unwrap().unwrap().model, "groq/llama");
        let infos = claude.infos(Some("opus"), &["codex".to_owned()], &|slot, provider| {
            Some(format!("{provider}-{slot}"))
        });
        let plan = &infos[2];
        let ids: Vec<&str> = plan.providers.iter().map(|p| p.provider.as_str()).collect();
        assert_eq!(ids, ["codex", "claude"]);
        assert_eq!(plan.providers[0].suggested.as_deref(), Some("codex-plan"));
        assert_eq!(plan.providers[1].model.as_deref(), Some("claude/opus"));
        assert_eq!(plan.providers[1].suggested, None);
        assert_eq!(plan.resolved.as_deref(), Some("claude/opus"));
    }

    /// A file of the flat form, a slot's own `model` and `backups`, reads as the any-provider assignment, and is written back in the same form beside the providers' assignments.
    #[test]
    fn the_flat_form_reads_as_the_any_provider_assignment() {
        let dir = scratch("flat");
        let path = dir.join("slots.json");
        std::fs::write(
            &path,
            r#"{"slots":{"fast":{"model":"claude/haiku:low","backups":["openai/gpt-5-mini"]}},"theme":"dark"}"#,
        )
        .unwrap();
        let files = SlotFiles {
            user: Some(path.clone()),
            project: None,
        };
        let layers = files.load().unwrap().for_provider(Some("codex"));
        assert_eq!(
            layers.effective(FAST).assignment,
            assigned("claude/haiku:low", &["openai/gpt-5-mini"])
        );
        files
            .set_slot(
                SlotScope::User,
                FAST,
                Some("codex"),
                Some("codex/gpt-5-mini".into()),
                Vec::new(),
            )
            .unwrap();
        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(written["slots"]["fast"]["model"], "claude/haiku:low");
        assert_eq!(
            written["slots"]["fast"]["providers"]["codex"]["model"],
            "codex/gpt-5-mini"
        );
        assert_eq!(written["theme"], "dark");
        let layers = files.load().unwrap();
        let preset = layers.snapshot(None);
        assert_eq!(
            preset.slots[FAST].providers["codex"].model.as_deref(),
            Some("codex/gpt-5-mini")
        );
        let infos = preset_infos(&BTreeMap::from([("p".to_owned(), preset)]));
        assert_eq!(infos[0].slots[0].providers[0].provider, "codex");
        files.clear_slot(SlotScope::User, FAST, None).unwrap();
        let layers = files.load().unwrap();
        assert_eq!(
            layers
                .clone()
                .for_provider(Some("claude"))
                .model_of(FAST)
                .unwrap(),
            None
        );
        assert_eq!(
            layers
                .for_provider(Some("codex"))
                .model_of(FAST)
                .unwrap()
                .unwrap()
                .model,
            "codex/gpt-5-mini"
        );
        files
            .clear_slot(SlotScope::User, FAST, Some("codex"))
            .unwrap();
        assert!(files.load().unwrap().user.slots.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
