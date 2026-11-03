//! What the reader can change about a conversation, and the commands an agent offers.

/// One value a setting can take.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Pick {
    /// What the agent calls the value.
    pub value: String,
    /// What it is called to the reader.
    pub name: String,
    /// What choosing it means, when the agent says.
    pub description: String,
}

/// Something about the conversation the reader can change: its model, mode or effort.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Setting {
    /// What the agent calls the setting.
    pub id: String,
    /// What the setting is for: `model`, `mode`, `thought_level` or nothing in particular.
    pub category: String,
    /// What it is called to the reader.
    pub name: String,
    /// The value it has now.
    pub current: String,
    /// The values it can take.
    pub picks: Vec<Pick>,
}

impl Setting {
    /// The name the reader knows the current value by.
    pub fn current_name(&self) -> Option<&str> {
        self.picks
            .iter()
            .find(|pick| pick.value == self.current)
            .map(|pick| pick.name.as_str())
    }
}

/// A command the agent offers behind a slash.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Slash {
    /// What is typed after the slash.
    pub name: String,
    /// What it does.
    pub description: String,
}
