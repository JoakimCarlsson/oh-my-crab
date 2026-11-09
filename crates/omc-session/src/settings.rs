//! The settings a conversation offers: its model, how hard it thinks, and how much it asks before acting.

use omc_providers::{Effort, Retention};
use omc_wire::{Pick, Setting};

/// The id of the setting that picks the model.
pub(crate) const MODEL: &str = "model";

/// The id of the setting that picks the model used while plan mode is on.
pub(crate) const PLAN_MODEL: &str = "plan_model";

/// The value of the plan model setting that plans with the model in use.
pub(crate) const SAME: &str = "same";

/// The id of the setting that turns saving each plan in the project too on or off; it takes [`ON`] and [`OFF`].
pub(crate) const PLAN_AUTOSAVE: &str = "plan_autosave";

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

/// The id of the setting that picks how long the provider keeps the prompt cache.
pub(crate) const CACHE: &str = "cache";

/// The value of the cache setting for the provider's default lifetime.
pub(crate) const SHORT: &str = "5m";

/// The value of the cache setting for an hour-long lifetime.
pub(crate) const LONG: &str = "1h";

/// The id of the setting that turns keeping the prompt cache warm on or off; it takes [`ON`] and [`OFF`].
pub(crate) const WARMING: &str = "cache_warming";

/// The id of the setting that turns the memory that outlives conversations on or off; it takes [`ON`] and [`OFF`].
pub(crate) const MEMORY: &str = "memory";

/// The value of the cache setting that stands for `retention`.
pub(crate) fn retention_value(retention: Retention) -> &'static str {
    match retention {
        Retention::Short => SHORT,
        Retention::Long => LONG,
    }
}

/// The lifetime a value of the cache setting stands for.
pub(crate) fn parse_retention(value: &str) -> Option<Retention> {
    [Retention::Short, Retention::Long]
        .into_iter()
        .find(|retention| retention_value(*retention) == value)
}

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
    /// The model used while plan mode is on, when one is set.
    pub(crate) plan_model: Option<&'a str>,
    /// Whether each plan is saved in the project too.
    pub(crate) autosave: bool,
    /// The mode in use.
    pub(crate) mode: Mode,
    /// The effort levels the model in use accepts.
    pub(crate) efforts: &'a [Effort],
    /// The effort level in use.
    pub(crate) effort: Option<Effort>,
    /// Whether the agent has the checkpoint and rewind tools.
    pub(crate) checkpoints: bool,
    /// The prompt cache lifetime in use, when the provider offers a choice.
    pub(crate) retention: Option<Retention>,
    /// Whether the prompt cache is kept warm, when the provider can be warmed.
    pub(crate) warming: Option<bool>,
    /// Whether the agent has its memory, when memory can be had at all.
    pub(crate) memory: Option<bool>,
}

/// The effort a model thinks at when the reader has not picked one: medium when it offers it, otherwise its lightest.
pub(crate) fn default_effort(efforts: &[Effort]) -> Option<Effort> {
    efforts
        .contains(&Effort::Medium)
        .then_some(Effort::Medium)
        .or_else(|| efforts.first().copied())
}

/// The settings for `offer`; the effort setting is left out when the model takes none, the cache settings when the provider offers no choice, and the memory setting when memory is turned off for the whole process or has no folder to live in.
pub(crate) fn offered(offer: &Offer<'_>) -> Vec<Setting> {
    let mut names: Vec<&str> = offer.models.iter().map(String::as_str).collect();
    if !names.contains(&offer.model) {
        names.insert(0, offer.model);
    }
    let mut planners = vec![pick(
        SAME,
        "Same as the model",
        "Plan with the model in use",
    )];
    if let Some(planner) = offer.plan_model.filter(|planner| !names.contains(planner)) {
        planners.push(pick(planner, planner, ""));
    }
    planners.extend(names.iter().map(|name| pick(name, name, "")));
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
    if let Some(retention) = offer.retention {
        settings.push(Setting {
            id: CACHE.to_owned(),
            category: CACHE.to_owned(),
            name: "Prompt cache".to_owned(),
            current: retention_value(retention).to_owned(),
            picks: vec![
                pick(
                    SHORT,
                    "5 minutes",
                    "Cached context lives five minutes after each request and costs a quarter more than input to write",
                ),
                pick(
                    LONG,
                    "1 hour",
                    "Cached context lives an hour after each request and costs twice the input price to write",
                ),
            ],
        });
    }
    if let Some(warming) = offer.warming {
        settings.push(Setting {
            id: WARMING.to_owned(),
            category: WARMING.to_owned(),
            name: "Keep cache warm".to_owned(),
            current: if warming { ON } else { OFF }.to_owned(),
            picks: vec![
                pick(
                    ON,
                    "On",
                    "Refresh the cached context shortly before it expires while a long tool runs or the conversation waits, when that is expected to cost less than losing it",
                ),
                pick(OFF, "Off", "Let the cached context expire"),
            ],
        });
    }
    if let Some(memory) = offer.memory {
        settings.push(Setting {
            id: MEMORY.to_owned(),
            category: MEMORY.to_owned(),
            name: "Memory".to_owned(),
            current: if memory { ON } else { OFF }.to_owned(),
            picks: vec![
                pick(
                    ON,
                    "On",
                    "The agent starts with what earlier conversations in this project remembered, and can save, search, correct and forget memories",
                ),
                pick(
                    OFF,
                    "Off",
                    "No memories are loaded, and the agent has no memory tools",
                ),
            ],
        });
    }
    settings.push(Setting {
        id: PLAN_MODEL.to_owned(),
        category: PLAN_MODEL.to_owned(),
        name: "Plan model".to_owned(),
        current: offer.plan_model.unwrap_or(SAME).to_owned(),
        picks: planners,
    });
    settings.push(Setting {
        id: PLAN_AUTOSAVE.to_owned(),
        category: PLAN_AUTOSAVE.to_owned(),
        name: "Save plans in the project".to_owned(),
        current: if offer.autosave { ON } else { OFF }.to_owned(),
        picks: vec![
            pick(
                OFF,
                "Off",
                "Plans are saved with the conversation only",
            ),
            pick(
                ON,
                "On",
                "Each plan is also saved as markdown in the project, under .oh-my-crab/plans unless OMC_PLANS_DIR names another folder",
            ),
        ],
    });
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
            plan_model: None,
            autosave: false,
            mode: Mode::Asking,
            efforts,
            effort,
            checkpoints: false,
            retention: None,
            warming: None,
            memory: None,
        }
    }

    /// The cache and memory settings come before the checkpoints, only when they can be had, and the cache values round-trip.
    #[test]
    fn cache_settings_follow_the_provider() {
        let models = [];
        let mut offer = offer(&models, &[], None);
        offer.retention = Some(Retention::Long);
        offer.warming = Some(true);
        offer.memory = Some(false);
        let settings = offered(&offer);
        let ids: Vec<&str> = settings.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "model",
                "mode",
                "cache",
                "cache_warming",
                "memory",
                "plan_model",
                "plan_autosave",
                "checkpoints"
            ]
        );
        assert_eq!(settings[4].current, "off");
        assert_eq!(
            (settings[2].current.as_str(), settings[3].current.as_str()),
            ("1h", "on")
        );
        for retention in [Retention::Short, Retention::Long] {
            assert_eq!(parse_retention(retention_value(retention)), Some(retention));
        }
        assert_eq!(parse_retention("2h"), None);
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
        assert_eq!(offered(&offer(&[], &[], None)).len(), 5);
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

    /// The plan model plans with the model in use until another is picked, offers every model and one picked that was not listed, and autosave is off until it is turned on.
    #[test]
    fn plan_settings_default_to_the_model_and_off() {
        let models = ["a".to_owned(), "b".to_owned()];
        let mut offer = offer(&models, &[], None);
        let settings = offered(&offer);
        let find = |settings: &[Setting], id: &str| {
            settings
                .iter()
                .find(|setting| setting.id == id)
                .cloned()
                .unwrap()
        };
        let planner = find(&settings, PLAN_MODEL);
        assert_eq!(planner.current, SAME);
        let values: Vec<&str> = planner.picks.iter().map(|p| p.value.as_str()).collect();
        assert_eq!(values, [SAME, "a", "b"]);
        assert_eq!(find(&settings, PLAN_AUTOSAVE).current, OFF);
        offer.plan_model = Some("other/big");
        offer.autosave = true;
        let settings = offered(&offer);
        let planner = find(&settings, PLAN_MODEL);
        assert_eq!(planner.current, "other/big");
        assert_eq!(planner.picks[1].value, "other/big");
        assert_eq!(find(&settings, PLAN_AUTOSAVE).current, ON);
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
