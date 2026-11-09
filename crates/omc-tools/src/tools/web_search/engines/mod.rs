//! The search services, one module each. Every module exports a `SPEC`
//! describing its service, which [`super::backend::Engine::spec`] maps the
//! service to.

pub(super) mod brave;
pub(super) mod deepseek;
pub(super) mod duckduckgo;
pub(super) mod exa;
pub(super) mod firecrawl;
pub(super) mod jina;
pub(super) mod kagi;
pub(super) mod keenable;
pub(super) mod kimi;
pub(super) mod ollama;
pub(super) mod parallel;
pub(super) mod perplexity;
pub(super) mod searxng;
pub(super) mod synthetic;
pub(super) mod tavily;
pub(super) mod tinyfish;
