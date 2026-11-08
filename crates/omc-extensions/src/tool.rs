//! A tool an extension gives the model, as a tool of the agent.

use std::sync::Arc;

use async_trait::async_trait;
use omc_tools::{Tool, ToolError, ToolOutput};
use serde_json::{Value, json};

use crate::host::{Running, Setting};

/// How long a tool's name may be, which is what the providers take.
const NAME_LIMIT: usize = 64;

/// A tool as an extension declares it in its answer to `initialize`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Offered {
    /// The name the model calls it by.
    pub(crate) name: String,
    /// What it does, for the model.
    pub(crate) description: String,
    /// The JSON schema of its arguments.
    pub(crate) schema: Value,
    /// Whether it only looks, so plan mode and read-only subagents keep it.
    pub(crate) read_only: bool,
    /// Whether the reader is asked before each call.
    pub(crate) approval: bool,
}

impl Offered {
    /// The tool `declared` describes: `name`, `description`, `input_schema` (an object schema, by default one that takes any object), `read_only` and `approval`, both off by default.
    ///
    /// # Errors
    ///
    /// Fails with the reason when the name is missing, longer than providers take or holds other characters than letters, digits, `_` and `-`, or the schema is not an object.
    pub(crate) fn read(declared: &Value) -> Result<Self, String> {
        let name = declared
            .get("name")
            .and_then(Value::as_str)
            .ok_or("a tool without a `name`")?;
        if name.is_empty()
            || name.len() > NAME_LIMIT
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(format!(
                "`{name}` is not a tool name: at most {NAME_LIMIT} letters, digits, `_` and `-`"
            ));
        }
        let schema = declared
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        if !schema.is_object() {
            return Err(format!("the `input_schema` of `{name}` is not an object"));
        }
        let flag = |key: &str| declared.get(key) == Some(&Value::Bool(true));
        Ok(Self {
            name: name.to_owned(),
            description: declared
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            schema,
            read_only: flag("read_only"),
            approval: flag("approval"),
        })
    }
}

/// A tool an extension gives, run by asking the extension, started again first when it stopped.
pub(crate) struct ExtensionTool {
    /// The name the model calls it by.
    name: &'static str,
    /// What it does.
    description: &'static str,
    /// How the extension declared it.
    offered: Offered,
    /// The extension that runs it.
    extension: Arc<Running>,
    /// What the extension is started with.
    setting: Arc<Setting>,
}

impl ExtensionTool {
    /// The tool `offered` by `extension`, which is started with `setting`.
    pub(crate) fn new(offered: Offered, extension: Arc<Running>, setting: Arc<Setting>) -> Self {
        Self {
            name: Box::leak(offered.name.clone().into_boxed_str()),
            description: Box::leak(offered.description.clone().into_boxed_str()),
            offered,
            extension,
            setting,
        }
    }
}

/// The text the model reads from an extension's answer to a `tool` request: `content` as text, or the text blocks of a list of them joined, or the whole answer as JSON when it has neither.
fn content_of(answer: &Value) -> String {
    match answer.get("content") {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => answer.to_string(),
    }
}

#[async_trait]
impl Tool for ExtensionTool {
    /// The name the model calls the tool by.
    fn name(&self) -> &'static str {
        self.name
    }

    /// What the tool does.
    fn description(&self) -> &'static str {
        self.description
    }

    /// The schema of the tool's arguments.
    fn schema(&self) -> Value {
        self.offered.schema.clone()
    }

    /// Whether the extension said the tool only looks.
    fn read_only(&self) -> bool {
        self.offered.read_only
    }

    /// Whether the extension asked for each call to be approved.
    fn needs_approval(&self) -> bool {
        self.offered.approval
    }

    /// The scope a call is approved under, the tool's name, when the extension asked for approval.
    fn approval_scope(&self, _args: &Value) -> Option<String> {
        self.offered.approval.then(|| self.name.to_owned())
    }

    /// The extension and tool, with the arguments.
    fn describe(&self, args: &Value) -> String {
        format!("{}: {} {args}", self.extension.spec.name, self.name)
    }

    /// Runs the tool with no call id.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Asks the extension to run the tool for the call `id` with `args`.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let params = json!({"name": self.name, "call_id": id, "arguments": args});
        match self.extension.call(&self.setting, params).await {
            Ok(answer) if answer.get("is_error") == Some(&Value::Bool(true)) => {
                Err(ToolError::Failed(content_of(&answer)))
            }
            Ok(answer) => Ok(ToolOutput::new(content_of(&answer))),
            Err(failure) => Err(ToolError::Failed(format!(
                "the extension {} could not run {}: {failure}",
                self.extension.spec.name, self.name
            ))),
        }
    }
}

#[cfg(test)]
/// Tests for reading tool declarations.
mod tests {
    use super::*;

    /// A declaration is read with its defaults; a bad name or schema is refused; an answer's text is found.
    #[test]
    fn reads_declarations_and_answers() {
        let offered =
            Offered::read(&json!({"name": "count_words", "description": "Counts."})).unwrap();
        assert_eq!(offered.schema["type"], "object");
        assert!(!offered.read_only && !offered.approval);
        assert!(Offered::read(&json!({"name": "two words"})).is_err());
        assert!(Offered::read(&json!({"name": "x", "input_schema": 3})).is_err());
        assert_eq!(content_of(&json!({"content": "3"})), "3");
        assert_eq!(
            content_of(
                &json!({"content": [{"type": "text", "text": "a"}, {"type": "text", "text": "b"}]})
            ),
            "a\nb"
        );
    }
}
