//! The `load_tools` tool: hands the model the description and schema of tools hidden from its tool list.

use async_trait::async_trait;
use omc_context::render;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Registry, Tool, ToolError, ToolOutput};

/// The name the model calls the tool by.
pub const LOAD_TOOLS: &str = "load_tools";

/// The most characters of a hidden tool's summary.
const SUMMARY_LIMIT: usize = 160;

/// What the model is told about the tool, before the hidden tools are listed.
const PROMPT: &str = include_str!("../prompts/load_tools.md");

/// One hidden tool, as the lookup hands it over.
#[derive(Clone, Debug, PartialEq)]
struct Hidden {
    /// The name the model calls it by.
    name: &'static str,
    /// Its whole description.
    description: String,
    /// The JSON schema of its arguments.
    schema: Value,
}

/// Arguments of a `load_tools` call.
#[derive(Deserialize)]
struct Args {
    /// The hidden tools to load.
    names: Vec<String>,
}

/// Hands the model the description and schema of tools hidden from its tool list, so that it can call them.
pub struct LoadTools {
    /// The hidden tools, in name order.
    hidden: Vec<Hidden>,
    /// What the model is told, with every hidden tool named and summed up in a line.
    description: String,
}

impl LoadTools {
    /// The tool for the tools of `registry` that are hidden until looked up.
    pub fn listing(registry: &Registry) -> Self {
        let hidden: Vec<Hidden> = registry
            .iter()
            .filter(|tool| registry.is_deferred(tool.name()))
            .map(|tool| Hidden {
                name: tool.name(),
                description: tool.description().to_owned(),
                schema: tool.schema(),
            })
            .collect();
        let listing = hidden
            .iter()
            .map(|tool| format!("- `{}`: {}", tool.name, one_line_summary(&tool.description)))
            .collect::<Vec<_>>()
            .join("\n");
        let description = render(PROMPT, &[("tools", listing.as_str())]);
        Self {
            hidden,
            description,
        }
    }

    /// The text the model reads for the hidden tool `tool`: its name, description and schema.
    fn shown(tool: &Hidden) -> String {
        let schema = serde_json::to_string(&tool.schema).unwrap_or_else(|_| "{}".to_owned());
        format!(
            "<tool name=\"{}\">\n<description>\n{}\n</description>\n<parameters>{schema}</parameters>\n</tool>",
            tool.name,
            tool.description.trim()
        )
    }
}

/// The first sentence of `description`, on one line, cut to [`SUMMARY_LIMIT`] characters.
pub fn one_line_summary(description: &str) -> String {
    let line = description
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let end = line.find(". ").map_or(line.len(), |at| at + 1);
    line[..end].chars().take(SUMMARY_LIMIT).collect()
}

#[async_trait]
impl Tool for LoadTools {
    /// Returns `load_tools`.
    fn name(&self) -> &'static str {
        LOAD_TOOLS
    }

    /// Describes the tool and names every hidden tool with a one-line summary.
    fn description(&self) -> &str {
        &self.description
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "names": {
                    "type": "array",
                    "items": { "type": "string" },
                    "minItems": 1,
                    "description": "The hidden tools to load, by name."
                }
            },
            "required": ["names"]
        })
    }

    /// Looking a tool up changes nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// Names the tools looked up.
    fn describe(&self, args: &Value) -> String {
        args["names"]
            .as_array()
            .map(|names| {
                names
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    }

    /// Returns the description and schema of each hidden tool named, noting the names that are not hidden tools.
    ///
    /// # Errors
    ///
    /// Returns [`ToolError::InvalidArguments`] when the arguments do not fit, and [`ToolError::Failed`], listing the hidden tools, when none of the names is one.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        let mut found: Vec<&Hidden> = Vec::new();
        let mut missing: Vec<&str> = Vec::new();
        for name in &args.names {
            let name = name.trim();
            match self.hidden.iter().find(|tool| tool.name == name) {
                Some(tool) if !found.iter().any(|known| known.name == tool.name) => {
                    found.push(tool);
                }
                Some(_) => {}
                None => missing.push(name),
            }
        }
        let known = || {
            self.hidden
                .iter()
                .map(|tool| format!("`{}`", tool.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        if found.is_empty() {
            return Err(ToolError::Failed(format!(
                "none of these is a hidden tool; the hidden tools are: {}. A tool on your tool list needs no loading.",
                known()
            )));
        }
        let mut parts: Vec<String> = found.iter().map(|tool| Self::shown(tool)).collect();
        if !missing.is_empty() {
            parts.push(format!(
                "Not hidden tools, so not loaded: {}. The hidden tools are: {}.",
                missing
                    .iter()
                    .map(|name| format!("`{name}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                known()
            ));
        }
        let names: Vec<String> = found
            .iter()
            .map(|tool| format!("`{}`", tool.name))
            .collect();
        parts.push(format!(
            "Loaded {}. Call them by name, like the tools on your list, with arguments that fit the parameters above.",
            names.join(", ")
        ));
        Ok(ToolOutput::new(parts.join("\n\n")))
    }
}

#[cfg(test)]
/// Tests for looking hidden tools up.
mod tests {
    use super::*;
    use crate::Deferral;

    /// A registry of the built-in tools with `ast_grep` and `ast_edit` hidden.
    fn registry() -> Registry {
        let mut registry = Registry::builtin(crate::testutil::scratch("load-tools")).unwrap();
        registry.defer(&Deferral::parse(Some("ast_*")));
        registry.fitted()
    }

    /// The description names every hidden tool with its first sentence, and only those.
    #[test]
    fn lists_the_hidden_tools_in_a_line_each() {
        let registry = registry();
        let lookup = registry.get(LOAD_TOOLS).unwrap();
        let description = lookup.description();
        assert!(description.contains("- `ast_edit`: Rewrite code by syntax tree across files"));
        assert!(description.contains("- `ast_grep`: Search code by syntax tree"));
        assert!(!description.contains("`grep`:"));
    }

    /// Looking a tool up gives its description and schema; a name that is not hidden is noted, and a call naming none fails listing the hidden tools.
    #[tokio::test]
    async fn hands_over_description_and_schema() {
        let registry = registry();
        let lookup = registry.get(LOAD_TOOLS).unwrap();
        let text = lookup
            .call(json!({"names": ["ast_grep", "read", "ast_grep"]}))
            .await
            .unwrap()
            .text;
        assert_eq!(text.matches("<tool name=\"ast_grep\">").count(), 1);
        assert!(text.contains("\"pattern\""), "{text}");
        assert!(
            text.contains("Not hidden tools, so not loaded: `read`"),
            "{text}"
        );
        let failed = lookup.call(json!({"names": ["read"]})).await.unwrap_err();
        assert!(
            failed.to_string().contains("`ast_edit`, `ast_grep`"),
            "{failed}"
        );
    }

    /// A summary is the first sentence of the first line, cut short.
    #[test]
    fn summaries_are_one_sentence() {
        assert_eq!(
            one_line_summary("\nDoes a thing. And more.\nNext"),
            "Does a thing."
        );
        assert_eq!(
            one_line_summary(&"x".repeat(400)).chars().count(),
            SUMMARY_LIMIT
        );
    }
}
