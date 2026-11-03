//! The `ask` tool: questions put to the reader, answered before the agent goes on.

use std::sync::Arc;

use async_trait::async_trait;
use omc_wire::{Alternative, Question, Reply, summary};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::parse_args;
use crate::{Host, Tool, ToolError, ToolOutput};

/// One answer a question offers.
#[derive(Deserialize)]
struct Option_ {
    /// The answer's name.
    label: String,
    /// What it means.
    #[serde(default)]
    description: String,
}

/// One question of an `ask` call.
#[derive(Deserialize)]
struct Item {
    /// A short name for the question.
    #[serde(default)]
    header: String,
    /// The question.
    question: String,
    /// The answers on offer.
    #[serde(default)]
    options: Vec<Option_>,
    /// Whether several answers may be picked.
    #[serde(default)]
    multi: bool,
    /// The index of the answer to recommend.
    recommended: Option<usize>,
}

/// Arguments of an `ask` call.
#[derive(Deserialize)]
struct Args {
    /// The questions, in order.
    questions: Vec<Item>,
}

/// Puts questions to the reader and returns their answers.
pub struct Ask {
    /// Where the questions go.
    host: Arc<dyn Host>,
}

impl Ask {
    /// The tool, asking through `host`.
    pub fn new(host: Arc<dyn Host>) -> Self {
        Self { host }
    }
}

impl From<Item> for Question {
    /// The question as the reader is shown it, with the recommended answer marked.
    fn from(item: Item) -> Self {
        let recommended = item.recommended;
        Question {
            header: item.header,
            text: item.question,
            multiple: item.multi,
            alternatives: item
                .options
                .into_iter()
                .enumerate()
                .map(|(index, option)| Alternative {
                    label: if recommended == Some(index) {
                        format!("{} (Recommended)", option.label)
                    } else {
                        option.label
                    },
                    description: option.description,
                })
                .collect(),
        }
    }
}

#[async_trait]
impl Tool for Ask {
    /// Returns `ask`.
    fn name(&self) -> &'static str {
        "ask"
    }

    /// Asking changes nothing on disk.
    fn read_only(&self) -> bool {
        true
    }

    /// Describes the tool to the model.
    fn description(&self) -> &'static str {
        include_str!("../prompts/ask.md")
    }

    /// Returns the argument schema.
    fn schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Names the question." },
                            "question": { "type": "string" },
                            "header": { "type": "string", "description": "A word or two naming what is asked." },
                            "options": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multi": { "type": "boolean", "description": "Allow several answers." },
                            "recommended": { "type": "integer", "description": "0-based index of the default option." }
                        },
                        "required": ["id", "question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    /// Describes a call by its first question.
    fn describe(&self, args: &Value) -> String {
        args["questions"][0]["question"]
            .as_str()
            .unwrap_or("question")
            .to_owned()
    }

    /// Waits for the reader's answers and reports them to the agent.
    async fn call(&self, args: Value) -> Result<ToolOutput, ToolError> {
        let args: Args = parse_args(args)?;
        if args.questions.is_empty() {
            return Err(ToolError::InvalidArguments("no questions".to_owned()));
        }
        let questions: Vec<Question> = args.questions.into_iter().map(Question::from).collect();
        let Some(replies) = self.host.inquire(questions.clone()).await else {
            return Ok(ToolOutput::new("The user cancelled the questions."));
        };
        Ok(ToolOutput::new(report(&questions, &replies)))
    }
}

/// The answers to `questions` as the agent reads them, one to a line.
fn report(questions: &[Question], replies: &[Reply]) -> String {
    summary(questions, replies)
        .into_iter()
        .map(|(question, answer)| match answer {
            Some(answer) => format!("{question}: {answer}"),
            None => format!("{question}: the user did not answer"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
/// Tests for the `ask` tool.
mod tests {
    use super::*;
    use crate::testutil::RecordingHost;

    /// The questions reach the host with the recommendation marked, and the answers come back.
    #[tokio::test]
    async fn asks_and_reports() {
        let host = Arc::new(RecordingHost::default());
        *host.replies.lock().unwrap() = Some(vec![Reply {
            picks: vec!["Tabs (Recommended)".into()],
            other: String::new(),
        }]);
        let tool = Ask::new(host.clone());
        let output = tool
            .call(json!({"questions": [{
                "id": "style", "question": "Which indent?",
                "options": [{"label": "Tabs"}, {"label": "Spaces", "description": "four"}],
                "recommended": 0
            }]}))
            .await
            .unwrap();
        assert_eq!(output.text, "Which indent?: Tabs (Recommended)");
        let asked = host.asked.lock().unwrap();
        assert_eq!(asked[0][0].alternatives[0].label, "Tabs (Recommended)");
        assert_eq!(asked[0][0].alternatives[1].description, "four");
    }

    /// An abandoned form is reported as cancelled.
    #[tokio::test]
    async fn abandoned_form_is_cancelled() {
        let host = Arc::new(RecordingHost::default());
        let tool = Ask::new(host);
        let output = tool
            .call(json!({"questions": [{"id": "a", "question": "q", "options": []}]}))
            .await
            .unwrap();
        assert!(output.text.contains("cancelled"));
    }
}
