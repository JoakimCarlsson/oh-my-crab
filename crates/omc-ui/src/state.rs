//! Application state shared by the event loop and the renderer.

use std::collections::HashMap;
use std::sync::Arc;

use omc_auth::Provider;
use omc_providers::Message;
use omc_tools::Registry;

/// Who wrote a chat entry.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// The person typing.
    User,
    /// The provider's reply.
    Assistant,
    /// Status and error messages from crab itself.
    System,
    /// A tool call the model requested.
    Tool,
    /// The collapsed result of a successful tool call.
    ToolOutput,
    /// The collapsed description of a failed tool call.
    ToolError,
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
    /// Choosing a model in the model picker.
    Models(ModelPicker),
}

/// The model picker: the offered models once fetched, and the highlighted row.
pub struct ModelPicker {
    /// The models the provider offers, or `None` while they are being fetched.
    pub items: Option<Vec<String>>,
    /// Index of the highlighted model.
    pub selected: usize,
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
    /// The conversation so far, replayed to the model on every request.
    pub history: Vec<Message>,
    /// The tools the model may call.
    pub registry: Arc<Registry>,
}

impl App {
    /// Creates the initial state with a greeting based on the active provider.
    pub fn new(active: Option<Provider>, registry: Arc<Registry>) -> Self {
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
            history: Vec::new(),
            registry,
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

    /// Appends a tool result entry, collapsed to a few lines.
    pub fn push_tool_result(&mut self, text: &str, is_error: bool) {
        let role = if is_error {
            Role::ToolError
        } else {
            Role::ToolOutput
        };
        self.push(role, collapse(text));
    }
}

/// Most lines of a tool result shown in the chat view.
const COLLAPSED_LINES: usize = 4;

/// Longest line of a collapsed tool result, in characters.
const COLLAPSED_WIDTH: usize = 160;

/// Shortens `text` to [`COLLAPSED_LINES`] lines of [`COLLAPSED_WIDTH`]
/// characters, noting how many lines were hidden.
fn collapse(text: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut shown: Vec<String> = lines
        .iter()
        .take(COLLAPSED_LINES)
        .map(|line| match line.char_indices().nth(COLLAPSED_WIDTH) {
            Some((end, _)) => format!("{}…", &line[..end]),
            None => (*line).to_string(),
        })
        .collect();
    if lines.len() > COLLAPSED_LINES {
        shown.push(format!("… ({} more lines)", lines.len() - COLLAPSED_LINES));
    }
    if shown.is_empty() {
        shown.push("(no output)".to_string());
    }
    shown.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Long results keep four lines and report the rest.
    #[test]
    fn collapse_hides_extra_lines() {
        let text = "1\n2\n3\n4\n5\n6";
        assert_eq!(collapse(text), "1\n2\n3\n4\n… (2 more lines)");
    }

    /// Empty output is labelled rather than shown blank.
    #[test]
    fn collapse_labels_empty_output() {
        assert_eq!(collapse(""), "(no output)");
    }
}
