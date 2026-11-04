//! The `skill` tool: loads a skill's instructions, or a file in its folder.

use std::path::Component;

use async_trait::async_trait;
use omc_context::Skill as Found;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Tool, ToolError, ToolOutput};

/// The most of a skill file one call returns, in bytes.
const LIMIT: usize = 100_000;

/// Arguments of a `skill` call.
#[derive(Deserialize)]
struct Args {
    /// The skill's name.
    name: String,
    /// A file inside the skill's folder, relative to it.
    path: Option<String>,
}

/// Loads skills by name.
pub struct Skill {
    /// The skills that may be loaded.
    skills: Vec<Found>,
}

impl Skill {
    /// The tool, loading from `skills`.
    pub fn new(skills: Vec<Found>) -> Self {
        Self { skills }
    }
}

#[async_trait]
impl Tool for Skill {
    /// Returns `skill`.
    fn name(&self) -> &'static str {
        "skill"
    }

    /// Reading changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/skill.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "The skill's name, as listed in the system prompt." },
                "path": { "type": "string", "description": "A file inside the skill's folder, relative to it." }
            },
            "required": ["name"]
        })
    }

    /// Describes a call by the skill it loads.
    fn describe(&self, args: &Value) -> String {
        args["name"].as_str().unwrap_or_default().to_owned()
    }

    /// Returns the skill's instructions, or the file asked for inside its folder.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let skill = self
            .skills
            .iter()
            .find(|skill| skill.name == args.name)
            .ok_or_else(|| {
                let known: Vec<&str> = self.skills.iter().map(|s| s.name.as_str()).collect();
                ToolError::Failed(format!(
                    "unknown skill {}; available: {}",
                    args.name,
                    known.join(", ")
                ))
            })?;
        let text = match args.path {
            None => skill.body()?,
            Some(path) => {
                let relative = std::path::Path::new(&path);
                if relative.is_absolute()
                    || relative
                        .components()
                        .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
                {
                    return Err(ToolError::OutsidePath(path));
                }
                std::fs::read_to_string(skill.base.join(relative))?
            }
        };
        let mut text = text;
        if text.len() > LIMIT {
            let mut end = LIMIT;
            while !text.is_char_boundary(end) {
                end -= 1;
            }
            text.truncate(end);
            text.push_str("\n[truncated]");
        }
        Ok(ToolOutput::new(format!(
            "Skill {} (folder {}):\n\n{}",
            skill.name,
            skill.base.display(),
            text.trim()
        )))
    }
}

#[cfg(test)]
/// Tests for the `skill` tool.
mod tests {
    use super::*;
    use crate::testutil::scratch;

    /// Builds a tool with one skill, `pdf`, with a side file.
    fn tool(name: &str) -> Skill {
        let base = scratch(name).join("pdf");
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("SKILL.md"), "---\ndescription: d\n---\nUse it.").unwrap();
        std::fs::write(base.join("extra.md"), "more").unwrap();
        Skill::new(vec![Found {
            name: "pdf".into(),
            description: "d".into(),
            path: base.join("SKILL.md"),
            base,
            hidden: false,
            origin: omc_context::Origin::User,
        }])
    }

    /// The body comes without front matter, side files load, and escapes are refused.
    #[tokio::test]
    async fn loads_body_and_files() {
        let tool = tool("skill-tool");
        let body = tool.call(json!({"name": "pdf"})).await.unwrap();
        assert!(body.text.contains("Use it.") && !body.text.contains("description: d"));
        let extra = tool
            .call(json!({"name": "pdf", "path": "extra.md"}))
            .await
            .unwrap();
        assert!(extra.text.ends_with("more"));
        assert!(
            tool.call(json!({"name": "pdf", "path": "../x"}))
                .await
                .is_err()
        );
        assert!(tool.call(json!({"name": "nope"})).await.is_err());
    }
}
