//! Everything found on disk for a conversation.

use crate::{AgentDefinition, Instruction, Locations, Skill, agents, instructions, skills};

/// The instruction files, skills and agent definitions found for a conversation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Discovered {
    /// The instruction files, the project's nearest first, then the reader's.
    pub instructions: Vec<Instruction>,
    /// The skills.
    pub skills: Vec<Skill>,
    /// The agent kinds, built-in ones last.
    pub agents: Vec<AgentDefinition>,
}

/// Finds what there is for work in `locations`, leaving out instruction files by path, and skills and agents by name, as `disabled` lists them.
pub fn discover(locations: &Locations, disabled: &[String]) -> Discovered {
    Discovered {
        instructions: instructions::find(locations, disabled),
        skills: skills::find(locations, disabled),
        agents: agents::find(locations, disabled),
    }
}
