//! The tools a conversation can have, listed without opening one.

use std::sync::Arc;

use async_trait::async_trait;
use omc_context::{Discovered, Environment, Locations};
use omc_providers::{ChatModel, ProviderError, Reply, Request, Sink};
use omc_tools::{Registry, Skill};
use omc_wire::ToolInfo;
use tokio::sync::mpsc::unbounded_channel;

use crate::shared::{SessionHost, Shared};
use crate::toolset::{Inputs, Toolset};

/// The longest a listed description may be, in characters.
const LIMIT: usize = 160;

/// A model that is never asked, since listing tools makes no request.
struct Unused;

#[async_trait]
impl ChatModel for Unused {
    /// Fails, since a listing never asks the model anything.
    async fn complete(&self, _: Request<'_>, _: &mut dyn Sink) -> Result<Reply, ProviderError> {
        Err(ProviderError::NoModels)
    }
}

/// The first sentence of `description`, cut to [`LIMIT`] characters.
fn sentence(description: &str) -> String {
    let line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let end = line.find(". ").map_or(line.len(), |at| at + 1);
    line[..end].chars().take(LIMIT).collect()
}

/// Every tool a conversation can have in any mode, built-in ones and the ones that talk to the reader, in name order.
///
/// Tools of tool servers are not included, since they depend on what the client configures. The skill tool is included; a conversation without skills has one too, which says there are none.
///
/// # Errors
///
/// Fails with the reason when the working folder cannot be used to build the tools.
pub fn tools() -> Result<Vec<ToolInfo>, String> {
    let directory = std::env::current_dir().map_err(|error| error.to_string())?;
    let mut base = Registry::builtin(&directory).map_err(|error| error.to_string())?;
    base.register(Skill::new(Vec::new()));
    let (out, _unheard) = unbounded_channel();
    let shared = Shared::new(out);
    let locations = Locations::detect(&directory);
    let toolset = Toolset::build(&Inputs {
        base: &base,
        denied: &[],
        host: Arc::new(SessionHost {
            shared: shared.clone(),
        }),
        model: Arc::new(Unused),
        approvals: shared.approvals.clone(),
        events: Arc::new(|_| {}),
        effort: None,
        discovered: Arc::new(Discovered::default()),
        environment: Arc::new(Environment::detect(&locations, "")),
        tree: Arc::from(""),
        notebook: shared.notebook.clone(),
        checkpoint: None,
        jobs: shared.jobs.clone(),
        unnamed: true,
        isolation: None,
        memories: (!omc_memory::disabled_by_environment())
            .then(|| omc_memory::Memories::open(&directory, None))
            .flatten()
            .map(Arc::new),
    });
    let mut listed: Vec<ToolInfo> = Vec::new();
    for plan in [false, true] {
        for tool in toolset.tools(plan).iter() {
            if !listed.iter().any(|known| known.name == tool.name()) {
                listed.push(ToolInfo {
                    name: tool.name().to_owned(),
                    description: sentence(tool.description()),
                });
            }
        }
    }
    listed.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(listed)
}

#[cfg(test)]
/// Tests for the tool listing.
mod tests {
    use super::*;

    /// Every tool of either mode is listed once, with a short description; the memory tools are among them unless memory is turned off for the process.
    #[test]
    fn lists_every_tool_once() {
        let listed = tools().unwrap();
        let mut names: Vec<&str> = listed.iter().map(|tool| tool.name.as_str()).collect();
        let memory = ["memory_edit", "memory_search", "remember"];
        if !omc_memory::disabled_by_environment() {
            for name in memory {
                assert!(names.contains(&name), "{name} missing");
            }
        }
        names.retain(|name| !memory.contains(name));
        assert_eq!(
            names,
            [
                "ask",
                "ast_edit",
                "ast_grep",
                "bash",
                "context_notes",
                "edit",
                "exit_plan_mode",
                "glob",
                "grep",
                "new_context",
                "read",
                "set_title",
                "skill",
                "task",
                "todo",
                "wait",
                "write"
            ]
        );
        assert!(
            listed
                .iter()
                .all(|tool| !tool.description.is_empty()
                    && tool.description.chars().count() <= LIMIT)
        );
    }

    /// A description is cut after its first sentence.
    #[test]
    fn keeps_the_first_sentence() {
        assert_eq!(sentence("\nDoes a thing. And more."), "Does a thing.");
        assert_eq!(sentence("No stop"), "No stop");
    }
}
