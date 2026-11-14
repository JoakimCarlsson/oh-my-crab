//! Model slots: the named roles a conversation asks a model for, such as `fast` for summaries and `plan` for plan mode, and the presets that set them all at once.

/// Which configuration file a slot change is written to.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SlotScope {
    /// The user's own file, for every project.
    #[default]
    User,
    /// The project's file, which wins over the user's slot by slot.
    Project,
}

/// Where the model a slot resolves to comes from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SlotSource {
    /// An environment variable, `OMC_MODEL_<SLOT>`.
    Environment,
    /// The project's file.
    Project,
    /// The user's file.
    User,
    /// Nothing sets it, so it is the conversation's model.
    Main,
}

/// One slot as it stands, with the model it resolves to.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlotInfo {
    /// The slot's name, such as `fast`.
    pub name: String,
    /// The model selector its any-provider assignment sets, as `provider/model` or `model`, with `:effort` when one is given; nothing when it is not set.
    #[serde(default)]
    pub model: Option<String>,
    /// The selectors its any-provider assignment tries in order when a request to the slot's model fails in a way another model may not.
    #[serde(default)]
    pub backups: Vec<String>,
    /// The model the slot resolves to now for the conversation's provider: its own, else the conversation's; nothing before a conversation is open and when nothing sets it.
    #[serde(default)]
    pub resolved: Option<String>,
    /// Where the model it resolves to comes from.
    pub source: SlotSource,
    /// Whether it has no model of its own and so takes the conversation's.
    pub inherited: bool,
    /// A model the catalog suggests for a slot with no model of its own; never applied by itself.
    #[serde(default)]
    pub suggested: Option<String>,
    /// The slot's assignment for each provider that has one or that the reader is signed in to, by provider id.
    #[serde(default)]
    pub providers: Vec<SlotProvider>,
}

/// One slot's assignment for one provider, used by conversations of that provider before the any-provider assignment.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SlotProvider {
    /// The provider's id, such as `claude` or `codex`.
    pub provider: String,
    /// The model selector set for this provider; nothing when it sets none.
    #[serde(default)]
    pub model: Option<String>,
    /// The selectors tried in order when the model fails.
    #[serde(default)]
    pub backups: Vec<String>,
    /// A model of this provider the catalog suggests when the slot sets none for it; never applied by itself, and never set in a preset.
    #[serde(default)]
    pub suggested: Option<String>,
}

/// One slot's assignment kept in a preset.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PresetSlot {
    /// The slot's name.
    pub name: String,
    /// Its model selector; nothing leaves it to the conversation's model.
    #[serde(default)]
    pub model: Option<String>,
    /// Its backup selectors, in order.
    #[serde(default)]
    pub backups: Vec<String>,
    /// Its assignments for single providers, by provider id.
    #[serde(default)]
    pub providers: Vec<SlotProvider>,
}

/// A named snapshot of every slot's assignment and the default effort.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PresetInfo {
    /// The preset's name.
    pub name: String,
    /// The slots it sets.
    pub slots: Vec<PresetSlot>,
    /// The effort the conversation is set to when it is applied, such as `high`; nothing leaves the effort as it is.
    #[serde(default)]
    pub effort: Option<String>,
}
