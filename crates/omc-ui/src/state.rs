//! Application state shared by the event loop and the renderer.

use std::collections::HashMap;

use omc_auth::Provider;

/// Who wrote a chat entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person typing.
    User,
    /// The provider's reply.
    Assistant,
    /// Status and error messages from crab itself.
    System,
}

/// One entry in the chat view.
pub struct Entry {
    /// Who wrote it.
    pub role: Role,
    /// Its text, which grows while a reply streams in.
    pub text: String,
}

/// What the keyboard currently controls.
pub enum Mode {
    /// Typing into the input line.
    Chat,
    /// Choosing a provider in the login picker, with the highlighted index.
    Picker(usize),
}

/// The whole UI state.
pub struct App {
    /// Chat entries in display order.
    pub entries: Vec<Entry>,
    /// Text typed on the input line.
    pub input: String,
    /// Current keyboard mode.
    pub mode: Mode,
    /// Whether a reply is still streaming.
    pub busy: bool,
    /// Set when the user asked to exit.
    pub quit: bool,
    /// The provider messages are sent to, once logged in.
    pub active: Option<Provider>,
    /// The model chosen per provider; absent means the provider default.
    pub models: HashMap<Provider, String>,
}

impl App {
    /// Creates the initial state with a greeting based on the active provider.
    pub fn new(active: Option<Provider>) -> Self {
        let greeting = match active {
            Some(provider) => format!("Logged in with {}. Say hi.", provider.label()),
            None => "Not logged in. Type /login to pick Claude or Codex.".to_string(),
        };
        let mut app = Self {
            entries: Vec::new(),
            input: String::new(),
            mode: Mode::Chat,
            busy: false,
            quit: false,
            active,
            models: HashMap::new(),
        };
        app.say(greeting);
        app
    }

    /// Appends a system message.
    pub fn say(&mut self, text: impl Into<String>) {
        self.push(Role::System, text.into());
    }

    /// Appends an entry with the given role and text.
    pub fn push(&mut self, role: Role, text: String) {
        self.entries.push(Entry { role, text });
    }

    /// Appends streamed text to the assistant entry, creating it when needed.
    pub fn append_reply(&mut self, delta: &str) {
        match self.entries.last_mut() {
            Some(entry) if entry.role == Role::Assistant => entry.text.push_str(delta),
            _ => self.push(Role::Assistant, delta.to_string()),
        }
    }
}
