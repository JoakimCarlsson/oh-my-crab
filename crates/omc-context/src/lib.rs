//! What the agent is told about its surroundings: the system prompt, the project's
//! instruction files, and the skills and agent definitions found on disk.

mod agents;
mod discovery;
mod environment;
mod frontmatter;
mod instructions;
mod invoke;
mod notices;
mod origin;
mod place;
mod plugins;
mod scope;
mod skills;
mod system;
mod template;
mod toml;
mod tree;

#[cfg(test)]
mod testutil;

pub use agents::{AgentDefinition, READ_ONLY_TOOLS, builtin_agents};
pub use discovery::{Discovered, discover};
pub use environment::{Environment, timestamp};
pub use frontmatter::{Parsed, parse as parse_frontmatter};
pub use instructions::{
    APPEND_SYSTEM_FILE, Instruction, SYSTEM_FILE, Scoped, SystemFiles, instructions_for,
    native_system_files, path_rules, path_rules_in, system_files,
};
pub use invoke::{
    expand_skill, plan_abandoned, plan_approved, plan_approved_fresh, plan_fresh, plan_rejected,
    seeded_plan, skill_invocation, subagent_assignment, subagent_plan,
};
pub use notices::{Notice, context_changes, environment_reminder, memory_notice, plan_mode_notice};
pub use origin::Origin;
pub use place::{agent_places, instruction_places, skill_places};
pub use plugins::plugin_folders;
pub use scope::{CONFIG_DIR_VARIABLE, Locations, PROJECT_FOLDER, crab_folder};
pub use skills::Skill;
pub use system::{PromptInput, build_prompt, build_subagent_prompt};
pub use template::render;
pub use tree::workspace_tree;
