//! Requests about a conversation that stand outside it: a question asked on the side, and the prompt that hands the conversation over to a new one.

use omc_providers::{ChatModel, Effort, Message, Request};

use crate::AgentError;
use crate::compaction::{latest, live, transcript};

/// The system prompt of a question asked on the side.
const ASIDE_SYSTEM: &str = include_str!("prompts/aside_system.md");

/// The system prompt of a hand-off.
const HANDOFF_SYSTEM: &str = include_str!("prompts/handoff_system.md");

/// What the model is asked for a hand-off without a goal.
const HANDOFF_ASK: &str = "Write the prompt that hands this conversation over, so that the new agent carries on the work.";

/// Asks `model`, thinking as hard as `effort` says, the reader's `question` about the conversation `history`, which it is shown as a record, and returns its answer; nothing is added to `history`.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when the request fails, and [`AgentError::Empty`] when the model answers with no text.
pub async fn answer_aside(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    history: &[Message],
    question: &str,
) -> Result<String, AgentError> {
    let prompt = format!(
        "{}<question>\n{}\n</question>",
        record(history),
        question.trim()
    );
    ask(model, effort, ASIDE_SYSTEM, prompt).await
}

/// Asks `model`, thinking as hard as `effort` says, to write the prompt that hands the conversation `history` over to a new conversation, toward `goal` when one is given, and returns that prompt.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when the request fails, and [`AgentError::Empty`] when the model answers with no text.
pub async fn write_handoff(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    history: &[Message],
    goal: Option<&str>,
) -> Result<String, AgentError> {
    let ask_for = goal
        .map(str::trim)
        .filter(|goal| !goal.is_empty())
        .map_or_else(
            || HANDOFF_ASK.to_owned(),
            |goal| {
                format!(
                    "Write the prompt that hands this conversation over, so that the new agent works toward this goal: {goal}"
                )
            },
        );
    let prompt = format!("{}{ask_for}", record(history));
    ask(model, effort, HANDOFF_SYSTEM, prompt).await
}

/// The record of `history` a request about it starts with: the summary of its latest compaction, when it has one, then the messages the model is given now, as plain text.
fn record(history: &[Message]) -> String {
    let summary = latest(history)
        .map(|(_, compaction)| {
            format!(
                "<earlier-summary>\n{}\n</earlier-summary>\n\n",
                compaction.summary
            )
        })
        .unwrap_or_default();
    format!(
        "{summary}<conversation>\n{}\n</conversation>\n\n",
        transcript(history, &live(history))
    )
}

/// Sends `prompt` to `model` under the system prompt `system`, offering no tools and writing nothing to the provider's prompt cache, and returns the text of its reply, trimmed.
///
/// # Errors
///
/// Returns [`AgentError::Provider`] when the request fails, and [`AgentError::Empty`] when the reply has no text.
async fn ask(
    model: &dyn ChatModel,
    effort: Option<Effort>,
    system: &str,
    prompt: String,
) -> Result<String, AgentError> {
    let messages = [Message::User(prompt)];
    let request = Request {
        system,
        messages: &messages,
        tools: &[],
        effort,
    };
    let reply = model.complete_uncached(request, &mut Quiet).await?;
    let text = reply.text.trim();
    if text.is_empty() {
        return Err(AgentError::Empty);
    }
    Ok(text.to_owned())
}

/// A sink that drops the chunks of a reply as they stream in, since the reply is told once it is whole.
struct Quiet;

impl omc_providers::Sink for Quiet {
    /// Ignores the start of a block.
    fn start(&mut self, _part: omc_providers::Part) {}

    /// Ignores a chunk of text.
    fn text(&mut self, _chunk: &str) {}

    /// Ignores a chunk of thinking.
    fn thinking(&mut self, _chunk: &str) {}
}

#[cfg(test)]
/// Tests for the requests about a conversation.
mod tests {
    use omc_providers::{Reply, ToolCall, ToolResult};

    use super::*;
    use crate::testkit::{Router, say};

    /// A short conversation: a prompt, a tool call and its result, and an answer.
    fn conversation() -> Vec<Message> {
        vec![
            Message::User("read a.txt".into()),
            Message::Assistant(Reply {
                tool_calls: vec![ToolCall {
                    id: "1".into(),
                    name: "read".into(),
                    arguments: serde_json::json!({"path": "a.txt"}),
                }],
                ..Reply::default()
            }),
            Message::ToolResults(vec![ToolResult {
                call_id: "1".into(),
                name: "read".into(),
                text: "alpha".into(),
                is_error: false,
                images: Vec::new(),
            }]),
            Message::Assistant(say("a.txt holds alpha")),
        ]
    }

    /// A question on the side is sent as one record of the conversation with the question, without tools, and its answer comes back trimmed while the history stays as it was.
    #[tokio::test]
    async fn an_aside_sends_a_record_and_the_question() {
        let model = Router::new(|_| say("  it holds alpha  "));
        let history = conversation();
        let answer = answer_aside(model.as_ref(), None, &history, "what is in a.txt?")
            .await
            .unwrap();
        assert_eq!(answer, "it holds alpha");
        let seen = model.seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].messages, 1);
        assert!(seen[0].tools.is_empty());
        assert_eq!(seen[0].system, ASIDE_SYSTEM);
        let asked = seen[0].last_user.clone().unwrap();
        assert!(asked.contains("[User]: read a.txt"));
        assert!(asked.contains("[Tool result, read]: alpha"));
        assert!(asked.ends_with("<question>\nwhat is in a.txt?\n</question>"));
        assert_eq!(history, conversation());
    }

    /// A hand-off names its goal when given one, and an empty reply is an error rather than an empty prompt.
    #[tokio::test]
    async fn a_handoff_names_its_goal_and_refuses_an_empty_reply() {
        let model = Router::new(|_| say("Carry on with a.txt."));
        let prompt = write_handoff(model.as_ref(), None, &conversation(), Some("ship b.txt"))
            .await
            .unwrap();
        assert_eq!(prompt, "Carry on with a.txt.");
        let asked = model.seen.lock().unwrap()[0].last_user.clone().unwrap();
        assert!(asked.ends_with("toward this goal: ship b.txt"));
        let silent = Router::new(|_| say("   "));
        let failed = write_handoff(silent.as_ref(), None, &conversation(), None).await;
        assert!(matches!(failed, Err(AgentError::Empty)));
    }
}
