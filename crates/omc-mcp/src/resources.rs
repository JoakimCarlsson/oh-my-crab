//! The resources tool servers offer, as one tool of the agent that lists and reads them.

use std::fmt::Write as _;
use std::sync::Arc;

use async_trait::async_trait;
use omc_tools::{Spill, Tool, ToolError, ToolOutput};
use serde_json::{Value, json};

use crate::client::Client;

/// The name the agent calls the tool by.
pub const RESOURCES_TOOL: &str = "mcp_resources";

/// Most bytes of a resource shown to the model; longer text is cut in the middle and saved whole.
const OUTPUT_LIMIT: usize = 20 * 1024;

/// The servers that offer resources, as they connect.
pub(crate) type Offering = Arc<std::sync::Mutex<Vec<Arc<Client>>>>;

/// Lists the resources and resource templates of the tool servers that offer them, and reads one.
pub(crate) struct Resources {
    /// The servers that offer resources.
    servers: Offering,
    /// Where text too long to show is saved whole.
    spill: Spill,
}

impl Resources {
    /// The tool over the servers in `servers`, saving text too long to show in `spill`.
    pub(crate) fn new(servers: Offering, spill: Spill) -> Self {
        Self { servers, spill }
    }

    /// The servers that offer resources now, those named `server` when it names one.
    ///
    /// # Errors
    ///
    /// Fails when no server offers resources, or none of them is called `server`.
    fn chosen(&self, server: Option<&str>) -> Result<Vec<Arc<Client>>, ToolError> {
        let all = self
            .servers
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .clone();
        let names = || {
            all.iter()
                .map(|client| client.name().to_owned())
                .collect::<Vec<_>>()
                .join(", ")
        };
        match server {
            None => Ok(all.clone()),
            Some(name) => {
                let picked: Vec<_> = all
                    .iter()
                    .filter(|client| client.name() == name)
                    .cloned()
                    .collect();
                if picked.is_empty() {
                    Err(ToolError::Failed(format!(
                        "no tool server called `{name}` offers resources; these do: {}",
                        names()
                    )))
                } else {
                    Ok(picked)
                }
            }
        }
    }
}

/// One listed resource or template as a line: the server, its address, its name and what it is.
fn line(server: &str, item: &Value, address: &str) -> String {
    let mut line = format!("{server}: {address}");
    let name = item["title"].as_str().or(item["name"].as_str());
    if let Some(name) = name {
        let _ = write!(line, " ({name})");
    }
    if let Some(kind) = item["mimeType"].as_str() {
        let _ = write!(line, " [{kind}]");
    }
    if let Some(description) = item["description"].as_str().filter(|text| !text.is_empty()) {
        let _ = write!(
            line,
            " - {}",
            description.lines().next().unwrap_or_default()
        );
    }
    line
}

/// The contents of a resource as text: each text part whole, and each binary part as a note of its kind and size.
fn contents(parts: &[Value]) -> String {
    parts
        .iter()
        .map(
            |part| match (part["text"].as_str(), part["blob"].as_str()) {
                (Some(text), _) => text.to_owned(),
                (None, Some(blob)) => format!(
                    "[binary {}, {} bytes of base64, at {}]",
                    part["mimeType"].as_str().unwrap_or("data"),
                    blob.len(),
                    part["uri"].as_str().unwrap_or_default()
                ),
                (None, None) => String::new(),
            },
        )
        .collect::<Vec<_>>()
        .join("\n")
}

#[async_trait]
impl Tool for Resources {
    /// The name the agent calls the tool by.
    fn name(&self) -> &'static str {
        RESOURCES_TOOL
    }

    /// What the tool does.
    fn description(&self) -> &str {
        "Lists and reads the resources of the connected MCP tool servers: files, records and other data a server offers by URI. Without `uri` it lists the resources and URI templates of every server, or of `server`; with `uri` it reads that resource, from `server` when given and otherwise from the first server that can. Fill in a template's `{placeholders}` to read from it."
    }

    /// The schema of the tool's arguments.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "server": { "type": "string", "description": "The tool server, by name. Optional." },
                "uri": { "type": "string", "description": "The resource to read. Leave out to list." }
            },
            "additionalProperties": false
        })
    }

    /// Listing and reading change nothing.
    fn read_only(&self) -> bool {
        true
    }

    /// The server and resource, or that it lists.
    fn describe(&self, args: &Value) -> String {
        let server = args["server"].as_str().unwrap_or("every server");
        match args["uri"].as_str() {
            Some(uri) => format!("read {uri} from {server}"),
            None => format!("list the resources of {server}"),
        }
    }

    /// Lists, or reads the resource `uri`, cutting text longer than [`OUTPUT_LIMIT`] in the middle and saving the whole of it.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let servers = self.chosen(args["server"].as_str())?;
        let capped = |text| self.spill.cap("mcp", text, OUTPUT_LIMIT);
        if let Some(uri) = args["uri"].as_str() {
            let mut failures = Vec::new();
            for client in &servers {
                match client.read(uri).await {
                    Ok(parts) => return Ok(ToolOutput::new(capped(contents(&parts)))),
                    Err(reason) => failures.push(format!("{}: {reason}", client.name())),
                }
            }
            return Err(ToolError::Failed(if failures.is_empty() {
                "no tool server offers resources".to_owned()
            } else {
                failures.join("\n")
            }));
        }
        let mut lines = Vec::new();
        for client in &servers {
            match client.resources().await {
                Ok(listed) => lines.extend(
                    listed
                        .iter()
                        .filter_map(|item| Some(line(client.name(), item, item["uri"].as_str()?))),
                ),
                Err(reason) => lines.push(format!(
                    "{}: cannot list resources: {reason}",
                    client.name()
                )),
            }
            if let Ok(templates) = client.templates().await {
                lines.extend(templates.iter().filter_map(|item| {
                    Some(line(
                        client.name(),
                        item,
                        &format!("template {}", item["uriTemplate"].as_str()?),
                    ))
                }));
            }
        }
        if lines.is_empty() {
            lines.push("no resources".to_owned());
        }
        Ok(ToolOutput::new(capped(lines.join("\n"))))
    }
}

#[cfg(test)]
/// Tests for showing resources.
mod tests {
    use super::*;

    /// A listed resource shows its server, address, name, kind and description.
    #[test]
    fn lines_read_well() {
        let item = json!({"uri": "file:///a", "name": "a", "mimeType": "text/plain", "description": "The a file\nmore"});
        assert_eq!(
            line("docs", &item, "file:///a"),
            "docs: file:///a (a) [text/plain] - The a file"
        );
    }

    /// Text is shown whole and binary parts as a note.
    #[test]
    fn contents_read_well() {
        let parts = [
            json!({"text": "hi"}),
            json!({"blob": "AAAA", "mimeType": "image/png", "uri": "x"}),
        ];
        assert_eq!(
            contents(&parts),
            "hi\n[binary image/png, 4 bytes of base64, at x]"
        );
    }
}
