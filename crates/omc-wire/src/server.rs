//! The OpenAI-compatible servers the user declares in the harness's configuration, as a client reads and edits them.

/// The protocol a declared server speaks.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerWire {
    /// The Chat Completions API, at `/chat/completions`.
    #[default]
    ChatCompletions,
    /// The Responses API, at `/responses`.
    Responses,
}

/// One OpenAI-compatible server the user declared, such as a LiteLLM gateway or a local vLLM, LM Studio or llama.cpp server; it is offered as a provider whose id is `id`.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Server {
    /// The id it is chosen by, as a provider: lowercase letters, digits, `-` and `_`, and not the id of a built-in provider.
    pub id: String,
    /// The name a client shows, or the id when there is none.
    #[serde(default)]
    pub label: Option<String>,
    /// The API address that `/chat/completions`, `/responses` and `/models` are appended to, such as `http://localhost:8000/v1`.
    pub base_url: String,
    /// The protocol it speaks.
    #[serde(default)]
    pub wire: ServerWire,
    /// The environment variables its API key is read from, first first; empty when it reads none.
    #[serde(default)]
    pub api_key_env: Vec<String>,
    /// The API key itself, kept in the configuration. The harness never sends it back, and says whether there is one in `has_api_key` instead. Sent to the harness, nothing keeps the key saved for the server, an empty string removes it, and anything else replaces it.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Whether an API key is kept in the configuration for the server; set by the harness, and ignored when sent to it.
    #[serde(default)]
    pub has_api_key: bool,
    /// Headers sent on every request, as name and value pairs.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// The model a conversation opens with when none is picked, or the first one offered when there is none.
    #[serde(default)]
    pub default_model: Option<String>,
    /// The models offered, in order; empty to ask the server at `GET {base_url}/models`.
    #[serde(default)]
    pub models: Vec<String>,
    /// Whether the model's reasoning goes back to the server as `reasoning_content` on later Chat Completions requests, which servers of some thinking models want around tool calls.
    #[serde(default)]
    pub replay_reasoning: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A server sent with only its id and address reads with the defaults, and writes back every field.
    #[test]
    fn reads_with_defaults() {
        let server: Server =
            serde_json::from_str(r#"{"id":"box","base_url":"http://localhost:8000/v1"}"#).unwrap();
        assert_eq!(server.wire, ServerWire::ChatCompletions);
        assert!(server.models.is_empty() && server.api_key.is_none() && !server.has_api_key);
        let written = serde_json::to_value(&server).unwrap();
        assert_eq!(written["wire"], "chat_completions");
        assert_eq!(written["api_key"], serde_json::Value::Null);
        let responses: ServerWire = serde_json::from_str(r#""responses""#).unwrap();
        assert_eq!(responses, ServerWire::Responses);
    }
}
