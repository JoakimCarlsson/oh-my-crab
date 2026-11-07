//! The providers crab knows, one module each; add a provider by adding a module and an entry here.

mod anthropic;
mod bedrock;
mod cerebras;
mod claude;
mod codex;
mod copilot;
mod deepseek;
mod fireworks;
mod gemini;
mod groq;
mod kimi_code;
mod llamacpp;
mod lmstudio;
mod minimax;
mod mistral;
mod moonshot;
mod ollama;
mod openai;
mod openrouter;
mod together;
mod vertex;
mod vllm;
mod xai;
mod zai;
mod zai_coding;

/// Declares [`Provider`], [`Provider::ALL`] and [`Provider::def`] from one list of
/// `Variant => module::DEF` entries; the order is the login picker's order.
///
/// Besides the listed variants, [`Provider`] has [`Provider::Custom`] for the
/// providers the user defines in configuration, which come after the built-in ones.
macro_rules! providers {
    ($($(#[$meta:meta])* $variant:ident => $def:path),* $(,)?) => {
        /// A supported provider.
        ///
        /// It is saved as its id, such as `"claude"`, and read back from one.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum Provider {
            $($(#[$meta])* $variant,)*
            /// A provider the user defined in configuration; see [`crate::Custom`].
            Custom(Custom),
        }

        impl Provider {
            /// Every built-in provider shown in the login picker, in display order; [`Provider::all`] adds the user's own.
            pub const ALL: &'static [Provider] = &[$(Provider::$variant),*];

            /// Returns the definition of this provider.
            pub fn def(self) -> &'static Def {
                match self {
                    $(Provider::$variant => &$def,)*
                    Provider::Custom(custom) => custom.def(),
                }
            }
        }
    };
}

use crate::custom::Custom;
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
    /// Kimi Code, Moonshot's Kimi For Coding subscription, with an API key.
    KimiCode => kimi_code::DEF,
    /// Moonshot's Kimi platform API with an API key.
    Moonshot => moonshot::DEF,
    /// Z.ai's GLM Coding Plan with an API key.
    ZaiCoding => zai_coding::DEF,
    /// Z.ai's pay-as-you-go API with an API key.
    Zai => zai::DEF,
    /// MiniMax with an API key.
    MiniMax => minimax::DEF,
    /// DeepSeek with an API key.
    DeepSeek => deepseek::DEF,
    /// xAI's Grok models with an API key.
    Xai => xai::DEF,
    /// Groq with an API key.
    Groq => groq::DEF,
    /// Cerebras with an API key.
    Cerebras => cerebras::DEF,
    /// Mistral's La Plateforme with an API key.
    Mistral => mistral::DEF,
    /// The OpenAI API with an API key.
    OpenAi => openai::DEF,
    /// Google Gemini through its OpenAI-compatible endpoint, with an API key.
    Gemini => gemini::DEF,
    /// Together AI with an API key.
    Together => together::DEF,
    /// Fireworks AI with an API key.
    Fireworks => fireworks::DEF,
    /// A local LM Studio server.
    LmStudio => lmstudio::DEF,
    /// A local llama.cpp `llama-server`.
    LlamaCpp => llamacpp::DEF,
    /// A local vLLM server.
    Vllm => vllm::DEF,
    /// The Anthropic API with an API key.
    Anthropic => anthropic::DEF,
    /// Claude on Amazon Bedrock, with AWS credentials or a Bedrock API key.
    Bedrock => bedrock::DEF,
    /// Claude on Google Vertex AI, with Google Cloud credentials.
    Vertex => vertex::DEF,
}
