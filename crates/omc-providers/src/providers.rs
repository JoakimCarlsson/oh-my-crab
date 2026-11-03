//! The providers crab knows, one module each; add a provider by adding a module and an entry here.

mod claude;
mod codex;
mod copilot;
mod ollama;
mod openrouter;

/// Declares [`Provider`], [`Provider::ALL`] and [`Provider::def`] from one list of
/// `Variant => module::DEF` entries; the order is the login picker's order.
macro_rules! providers {
    ($($(#[$meta:meta])* $variant:ident => $def:path),* $(,)?) => {
        /// A supported provider.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
        #[serde(rename_all = "lowercase")]
        pub enum Provider {
            $($(#[$meta])* $variant,)*
        }

        impl Provider {
            /// Every provider shown in the login picker, in display order.
            pub const ALL: &'static [Provider] = &[$(Provider::$variant),*];

            /// Returns the definition of this provider.
            pub fn def(self) -> &'static Def {
                match self {
                    $(Provider::$variant => &$def,)*
                }
            }
        }
    };
}

use crate::provider::Def;

providers! {
    /// Anthropic Claude via OAuth.
    Claude => claude::DEF,
    /// OpenAI Codex via ChatGPT OAuth.
    Codex => codex::DEF,
    /// A local Ollama server.
    Ollama => ollama::DEF,
    /// OpenRouter with an API key.
    OpenRouter => openrouter::DEF,
    /// GitHub Copilot through GitHub's device-code login.
    Copilot => copilot::DEF,
}
