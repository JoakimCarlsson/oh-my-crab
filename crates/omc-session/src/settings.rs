//! The settings a conversation offers: its model, how hard it thinks, and how much it asks before acting.

use omc_providers::Effort;
use omc_wire::{Pick, Setting};

/// The id of the setting that picks the model.
pub(crate) const MODEL: &str = "model";

/// The id of the setting that picks how much the agent asks before acting.
pub(crate) const MODE: &str = "mode";

/// The id of the setting that picks how hard the model thinks.
pub(crate) const EFFORT: &str = "effort";

/// The id of the setting that turns the checkpoint and rewind tools on or off.
pub(crate) const CHECKPOINTS: &str = "checkpoints";

/// The value of the checkpoints setting that gives the agent the tools.
pub(crate) const ON: &str = "on";

/// The value of the checkpoints setting that withholds the tools.
pub(crate) const OFF: &str = "off";

/// The category of the setting that picks how hard the model thinks.
const THOUGHT_LEVEL: &str = "thought_level";

/// How much the agent asks before acting, and whether it may change anything.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mode {
    /// Asks before tools that change things run.
    Asking,
    /// Runs everything without asking.
    Yolo,
    /// Only looks, and writes a plan for approval.
    Plan,
}

impl Mode {
    /// The value of the mode setting that stands for the mode.
    pub(crate) fn value(self) -> &'static str {
        match self {
            Mode::Asking => "default",
            Mode::Yolo => "yolo",
            Mode::Plan => "plan",
        }
    }

    /// The mode a setting value stands for.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        [Mode::Asking, Mode::Yolo, Mode::Plan]
            .into_iter()
            .find(|mode| mode.value() == value)
    }
}

/// What the settings are offered from.
pub(crate) struct Offer<'a> {
    /// The models the provider listed.
    pub(crate) models: &'a [String],
    /// The model in use.
    pub(crate) model: &'a str,
    /// The mode in use.
    pub(crate) mode: Mode,
    /// The effort levels the model in use accepts.
    pub(crate) efforts: &'a [Effort],
    /// The effort level in use.
    pub(crate) effort: Option<Effort>,
    /// Whether the agent has the checkpoint and rewind tools.
    pub(crate) checkpoints: bool,
}

/// The effort a model thinks at when the reader has not picked one: medium when it offers it, otherwise its lightest.
pub(crate) fn default_effort(efforts: &[Effort]) -> Option<Effort> {
    efforts
        .contains(&Effort::Medium)
        .then_some(Effort::Medium)
        .or_else(|| efforts.first().copied())
}

/// The settings for `offer`; the effort setting is left out when the model takes none.
pub(crate) fn offered(offer: &Offer<'_>) -> Vec<Setting> {
    let mut names: Vec<&str> = offer.models.iter().map(String::as_str).collect();
    if !names.contains(&offer.model) {
        names.insert(0, offer.model);
    }
    let mut settings = vec![
        Setting {
            id: MODEL.to_owned(),
            category: MODEL.to_owned(),
            name: "Model".to_owned(),
            current: offer.model.to_owned(),
            picks: names.into_iter().map(|name| pick(name, name, "")).collect(),
        },
        Setting {
            id: MODE.to_owned(),
            category: MODE.to_owned(),
            name: "Mode".to_owned(),
            current: offer.mode.value().to_owned(),
            picks: vec![
                pick(
                    Mode::Asking.value(),
                    "Ask",
                    "Ask before editing files or running commands",
                ),
                pick(
                    Mode::Yolo.value(),
                    "Run freely",
                    "Run everything without asking",
                ),
                pick(
                    Mode::Plan.value(),
                    "Plan",
                    "Only look around, then write a plan for approval before changing anything",
                ),
            ],
        },
    ];
    let current = offer.effort.filter(|effort| offer.efforts.contains(effort));
    if let Some(current) = current.or_else(|| default_effort(offer.efforts)) {
        settings.push(Setting {
            id: EFFORT.to_owned(),
            category: THOUGHT_LEVEL.to_owned(),
            name: "Effort".to_owned(),
            current: current.as_str().to_owned(),
            picks: offer
                .efforts
                .iter()
                .map(|effort| pick(effort.as_str(), &title(effort.as_str()), ""))
                .collect(),
        });
    }
    settings.push(Setting {
        id: CHECKPOINTS.to_owned(),
        category: CHECKPOINTS.to_owned(),
        name: "Checkpoints".to_owned(),
        current: if offer.checkpoints { ON } else { OFF }.to_owned(),
        picks: vec![
            pick(OFF, "Off", "The agent has no checkpoint and rewind tools"),
            pick(
                ON,
                "On",
                "The agent may mark a checkpoint and later replace the exploration since it with a report; files are never restored",
            ),
        ],
    });
    settings
}

/// A pick of `value`, shown as `name`, meaning `description`.
fn pick(value: &str, name: &str, description: &str) -> Pick {
    Pick {
        value: value.to_owned(),
        name: name.to_owned(),
        description: description.to_owned(),
    }
}

/// `word` with a capital first letter.
fn title(word: &str) -> String {
    let mut letters = word.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}

#[cfg(test)]
/// Tests for the settings offered.
mod tests {
    use super::*;

    /// Builds an offer with the given efforts and model `b`.
    fn offer<'a>(models: &'a [String], efforts: &'a [Effort], effort: Option<Effort>) -> Offer<'a> {
        Offer {
            models,
            model: "b",
            mode: Mode::Asking,
            efforts,
            effort,
            checkpoints: false,
        }
    }

    /// The current model is offered even when the provider did not list it.
    #[test]
    fn current_model_is_always_a_pick() {
        let models = ["a".to_owned()];
        let settings = offered(&offer(&models, &[], None));
        assert!(settings[0].picks.iter().any(|pick| pick.value == "b"));
        assert_eq!(settings[1].current, "default");
        assert!(settings[1].picks.iter().any(|pick| pick.value == "plan"));
    }

    /// Effort is hidden when the model takes none, and otherwise falls back to medium.
    #[test]
    fn effort_follows_the_model() {
        assert_eq!(offered(&offer(&[], &[], None)).len(), 3);
        let settings = offered(&offer(&[], &Effort::ALL, None));
        let effort = &settings[2];
        assert_eq!(
            (effort.id.as_str(), effort.category.as_str()),
            ("effort", "thought_level")
        );
        assert_eq!(effort.current, "medium");
        assert_eq!(effort.picks.len(), 5);
        let picked = offered(&offer(&[], &Effort::ALL, Some(Effort::High)));
        assert_eq!(picked[2].current, "high");
        let narrowed = offered(&offer(&[], &[Effort::Low, Effort::High], Some(Effort::Max)));
        assert_eq!(narrowed[2].current, "low");
    }

    /// The checkpoints setting is last, off unless it was turned on.
    #[test]
    fn checkpoints_are_off_by_default() {
        let models = [];
        let mut offer = offer(&models, &[], None);
        let off = offered(&offer);
        let last = off.last().unwrap();
        assert_eq!(
            (last.id.as_str(), last.current.as_str()),
            ("checkpoints", "off")
        );
        offer.checkpoints = true;
        assert_eq!(offered(&offer).last().unwrap().current, "on");
    }

    /// Modes round-trip through their values.
    #[test]
    fn modes_parse() {
        for mode in [Mode::Asking, Mode::Yolo, Mode::Plan] {
            assert_eq!(Mode::parse(mode.value()), Some(mode));
        }
        assert_eq!(Mode::parse("x"), None);
    }
}
