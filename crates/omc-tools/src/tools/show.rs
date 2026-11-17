//! The `show` tool: the agent puts a visual in front of the reader and goes on.

use std::sync::Arc;

use async_trait::async_trait;
use omc_wire::{Exhibit, ExhibitKind};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Concurrency, Host, Tool, ToolError, ToolOutput};

/// The most content one exhibit may carry, in bytes.
const LIMIT: usize = 256 * 1024;

/// The longest title that is kept, in characters.
const TITLE_LIMIT: usize = 80;

/// What the content of a `show` call is, as the model names it.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Form {
    /// Mermaid source.
    Mermaid,
    /// A self-contained HTML document.
    Html,
}

impl From<Form> for ExhibitKind {
    /// The kind the client is told for what the model called `form`.
    fn from(form: Form) -> Self {
        match form {
            Form::Mermaid => ExhibitKind::Diagram,
            Form::Html => ExhibitKind::Prototype,
        }
    }
}

/// Arguments of a `show` call.
#[derive(Deserialize)]
struct Args {
    /// What the visual is called.
    title: String,
    /// What `content` is.
    kind: Form,
    /// The Mermaid source or the HTML document.
    content: String,
}

/// Shows the reader a diagram or a prototype without waiting on them.
pub struct Show {
    /// Where the visual goes.
    host: Arc<dyn Host>,
}

impl Show {
    /// The tool, showing through `host`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

/// The exhibit the call numbered `id` with `args` shows: the title on one line and cut to its limit, the content as it was given.
///
/// # Errors
///
/// Returns [`ToolError::InvalidArguments`] when the title or the content is empty, or the content is larger than the limit.
fn exhibit(id: &str, args: Args) -> Result<Exhibit, ToolError> {
    let title: String = args.title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title.chars().take(TITLE_LIMIT).collect();
    if title.is_empty() {
        return Err(ToolError::InvalidArguments("the title is empty".to_owned()));
    }
    if args.content.trim().is_empty() {
        return Err(ToolError::InvalidArguments(
            "the content is empty".to_owned(),
        ));
    }
    if args.content.len() > LIMIT {
        return Err(ToolError::InvalidArguments(format!(
            "the content is {} bytes, over the limit of {LIMIT} bytes ({} KiB); show something smaller",
            args.content.len(),
            LIMIT / 1024
        )));
    }
    Ok(Exhibit {
        id: id.to_owned(),
        title,
        kind: args.kind.into(),
        content: args.content,
    })
}

#[async_trait]
impl Tool for Show {
    /// Returns `show`.
    fn name(&self) -> &'static str {
        "show"
    }

    /// Showing changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Talks to the reader, so it runs alone, in the model's order.
    fn concurrency(&self, _args: &Value) -> Concurrency {
        Concurrency::Exclusive
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/show.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": { "type": "string", "description": "A short name for the visual." },
                "kind": {
                    "type": "string",
                    "enum": ["mermaid", "html"],
                    "description": "`mermaid` for a diagram, `html` for a prototype."
                },
                "content": {
                    "type": "string",
                    "description": "Mermaid source, or a self-contained HTML document with inline styles and no external resources."
                }
            },
            "required": ["title", "kind", "content"]
        })
    }

    /// Describes a call by the title of what it shows.
    fn describe(&self, args: &Value) -> String {
        args["title"].as_str().unwrap_or_default().to_owned()
    }

    /// Shows the visual under no call id, for a call made outside the agent loop.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        self.call_as("", args).await
    }

    /// Shows the visual under the call's `id` and goes on without waiting for the reader.
    async fn call_as(&self, id: &str, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        self.host.show(exhibit(id, args)?);
        Ok(ToolOutput::new("Shown to the user."))
    }
}

#[cfg(test)]
/// Tests for the `show` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// A diagram and a prototype reach the host under the call's id, with the kind the client knows and the title tidied.
    #[tokio::test]
    async fn shows_under_the_call_id() {
        let host = Arc::new(RecordingHost::default());
        let tool = Show::new(host.clone());
        let output = tool
            .call_as(
                "call-7",
                json!({"title": "  Login   flow ", "kind": "mermaid", "content": "graph TD; A-->B"}),
            )
            .await
            .unwrap();
        assert_eq!(output.text, "Shown to the user.");
        tool.call_as(
            "call-8",
            json!({"title": "Card", "kind": "html", "content": "<!doctype html><p>hi</p>"}),
        )
        .await
        .unwrap();
        let shown = host.shown.lock().unwrap();
        assert_eq!(
            shown[0],
            Exhibit {
                id: "call-7".to_owned(),
                title: "Login flow".to_owned(),
                kind: ExhibitKind::Diagram,
                content: "graph TD; A-->B".to_owned(),
            }
        );
        assert_eq!(shown[1].id, "call-8");
        assert_eq!(shown[1].kind, ExhibitKind::Prototype);
    }

    /// Empty content, an empty title, an unknown kind and content over the limit are refused, and nothing is shown.
    #[tokio::test]
    async fn refuses_what_cannot_be_shown() {
        let host = Arc::new(RecordingHost::default());
        let tool = Show::new(host.clone());
        let empty = tool
            .call(json!({"title": "t", "kind": "html", "content": "  \n"}))
            .await
            .unwrap_err();
        assert!(empty.to_string().contains("content is empty"));
        assert!(
            tool.call(json!({"title": " ", "kind": "html", "content": "<p>x</p>"}))
                .await
                .is_err()
        );
        assert!(
            tool.call(json!({"title": "t", "kind": "svg", "content": "<svg/>"}))
                .await
                .is_err()
        );
        let large = tool
            .call(json!({"title": "t", "kind": "html", "content": "x".repeat(LIMIT + 1)}))
            .await
            .unwrap_err();
        assert!(large.to_string().contains("256 KiB"));
        assert!(
            tool.call(json!({"title": "t", "kind": "html", "content": "x".repeat(LIMIT)}))
                .await
                .is_ok()
        );
        assert_eq!(host.shown.lock().unwrap().len(), 1);
    }
}
