//! The wire protocols crab speaks, one module each; a provider names the one it uses.

pub(crate) mod anthropic_messages;
pub(crate) mod chat_completions;
pub(crate) mod gemini;
pub(crate) mod openai_responses;
