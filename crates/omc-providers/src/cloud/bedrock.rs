//! Claude on Amazon Bedrock through `InvokeModelWithResponseStream`.
//!
//! Bedrock takes the Anthropic Messages body nearly as it is, with
//! `anthropic_version` in place of the model and stream fields and the betas in
//! `anthropic_beta`, so prompt caching, thinking and its signatures behave as
//! on Anthropic's own API. Its answer is an AWS event stream whose `chunk`
//! events each carry one Messages event, base64-encoded.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde_json::{Value, json};

use super::aws;
use super::eventstream::Message;
use super::sigv4::{self, Keys};
use crate::{Provider, ProviderError};

/// The Messages API version Bedrock is asked for.
const VERSION: &str = "bedrock-2023-05-31";

/// The service name Bedrock requests are signed for.
const SERVICE: &str = "bedrock";

/// Makes `body`, built for Anthropic's own API, into a Bedrock body: no `model` or `stream`, the API version, and `betas` in the body.
pub(crate) fn adapt_body(body: &mut Value, betas: &[&str]) {
    if let Some(object) = body.as_object_mut() {
        object.remove("model");
        object.remove("stream");
        object.insert("anthropic_version".to_owned(), json!(VERSION));
        if !betas.is_empty() {
            object.insert("anthropic_beta".to_owned(), json!(betas));
        }
    }
}

/// The address of the Bedrock runtime for `region`: the provider's address with the region put in, or the override its address variable sets.
pub(crate) fn base_url(region: &str) -> String {
    Provider::Bedrock
        .def()
        .base_url()
        .replace("{region}", region)
}

/// The address `model` is invoked at under `base`, streamed or not.
pub(crate) fn invoke_url(base: &str, model: &str, stream: bool) -> String {
    let action = if stream {
        "invoke-with-response-stream"
    } else {
        "invoke"
    };
    format!(
        "{}/model/{}/{action}",
        base.trim_end_matches('/'),
        sigv4::uri_encode(model, true)
    )
}

/// How a Bedrock request is authorized.
pub(crate) enum Authorization {
    /// A Bedrock API key, sent as a bearer token.
    Bearer(String),
    /// AWS keys that sign the request.
    Signed(Keys),
}

impl Authorization {
    /// The authorization for a login whose key is `access`: that key as a bearer token when there is one, else the AWS keys the environment and profile hold.
    ///
    /// # Errors
    ///
    /// Returns [`ProviderError::Credentials`] when there is no key and no AWS keys can be found.
    pub(crate) fn find(access: &str) -> Result<Self, ProviderError> {
        if !access.trim().is_empty() {
            return Ok(Self::Bearer(access.trim().to_owned()));
        }
        aws::keys()
            .map(Self::Signed)
            .map_err(ProviderError::Credentials)
    }
}

/// The headers of a request with `body` to `url` in `region`, authorized by `authorization` at the time `amz_date`, as names and values.
///
/// # Errors
///
/// Returns [`ProviderError::Credentials`] when `url` is not an address.
pub(crate) fn headers(
    url: &str,
    body: &[u8],
    stream: bool,
    region: &str,
    authorization: &Authorization,
    amz_date: &str,
) -> Result<Vec<(String, String)>, ProviderError> {
    let accept = if stream {
        "application/vnd.amazon.eventstream"
    } else {
        "application/json"
    };
    let mut headers = vec![
        ("content-type".to_owned(), "application/json".to_owned()),
        ("accept".to_owned(), accept.to_owned()),
    ];
    match authorization {
        Authorization::Bearer(key) => {
            headers.push(("authorization".to_owned(), format!("Bearer {key}")));
        }
        Authorization::Signed(keys) => {
            let parsed = reqwest::Url::parse(url)
                .map_err(|error| ProviderError::Credentials(format!("{url}: {error}")))?;
            let host = match (parsed.host_str(), parsed.port()) {
                (Some(host), Some(port)) => format!("{host}:{port}"),
                (Some(host), None) => host.to_owned(),
                (None, _) => String::new(),
            };
            let signing = sigv4::Signing {
                method: "POST",
                host: &host,
                path: parsed.path(),
                query: &[],
                headers: &[("content-type", "application/json"), ("accept", accept)],
                payload: body,
                region,
                service: SERVICE,
                amz_date,
            };
            headers.extend(sigv4::sign(keys, &signing));
        }
    }
    Ok(headers)
}

/// The payload of a `chunk` event.
#[derive(Deserialize)]
struct Chunk {
    /// One Messages event as base64-encoded JSON.
    bytes: String,
}

/// The JSON of the Messages event `message` carries: some for a `chunk` event, none for other events.
///
/// # Errors
///
/// Returns [`ProviderError::Stream`] for an exception or error message, and for a chunk that cannot be read.
pub(crate) fn event_data(message: &Message) -> Result<Option<String>, ProviderError> {
    match message.header(":message-type") {
        Some("exception") => {
            let kind = message.header(":exception-type").unwrap_or("exception");
            let text = serde_json::from_slice::<Value>(&message.payload)
                .ok()
                .and_then(|body| body["message"].as_str().map(str::to_owned))
                .unwrap_or_else(|| String::from_utf8_lossy(&message.payload).into_owned());
            Err(ProviderError::Stream(format!("{kind}: {text}")))
        }
        Some("error") => Err(ProviderError::Stream(format!(
            "{}: {}",
            message.header(":error-code").unwrap_or("error"),
            message.header(":error-message").unwrap_or("")
        ))),
        _ if message.header(":event-type") == Some("chunk") => {
            let chunk: Chunk = serde_json::from_slice(&message.payload)
                .map_err(|error| ProviderError::Stream(format!("unreadable chunk: {error}")))?;
            let bytes = STANDARD
                .decode(chunk.bytes.as_bytes())
                .map_err(|error| ProviderError::Stream(format!("unreadable chunk: {error}")))?;
            Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
        }
        _ => Ok(None),
    }
}

/// The Claude models Bedrock serves in `region`, as the catalog lists them: the global inference profiles, then those of the region's geography, then the models themselves.
pub(crate) fn models(region: &str) -> Vec<String> {
    let geography = region.split('-').next().unwrap_or(region);
    let prefixes: &[&str] = match geography {
        "us" => &["us."],
        "eu" => &["eu."],
        "ap" => &["apac.", "jp.", "au.", "in."],
        _ => &[],
    };
    let claude: Vec<String> = crate::catalog::listed("amazon-bedrock")
        .into_iter()
        .filter(|id| id.contains("anthropic.claude"))
        .collect();
    let mut ordered: Vec<String> = claude
        .iter()
        .filter(|id| id.starts_with("global."))
        .cloned()
        .collect();
    for prefix in prefixes {
        ordered.extend(claude.iter().filter(|id| id.starts_with(prefix)).cloned());
    }
    ordered.extend(
        claude
            .iter()
            .filter(|id| id.starts_with("anthropic."))
            .cloned(),
    );
    ordered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud::eventstream::Decoder;
    use crate::cloud::eventstream::tests::frame;

    /// The body loses its model and stream fields and gains the version and betas.
    #[test]
    fn adapts_the_messages_body() {
        let mut body = json!({ "model": "m", "stream": true, "max_tokens": 5, "messages": [] });
        adapt_body(&mut body, &["interleaved-thinking-2025-05-14"]);
        assert!(body.get("model").is_none() && body.get("stream").is_none());
        assert_eq!(body["anthropic_version"], VERSION);
        assert_eq!(body["anthropic_beta"][0], "interleaved-thinking-2025-05-14");
        let mut plain = json!({ "model": "m" });
        adapt_body(&mut plain, &[]);
        assert!(plain.get("anthropic_beta").is_none());
    }

    /// The model id is encoded in the path, and streaming picks the streaming action.
    #[test]
    fn builds_invoke_urls() {
        assert_eq!(
            invoke_url(
                "https://bedrock-runtime.us-east-1.amazonaws.com/",
                "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
                true
            ),
            "https://bedrock-runtime.us-east-1.amazonaws.com/model/us.anthropic.claude-sonnet-4-5-20250929-v1%3A0/invoke-with-response-stream"
        );
        assert!(invoke_url("http://x", "m", false).ends_with("/model/m/invoke"));
    }

    /// A bearer key is sent as it is, and AWS keys sign the request with its host, path and body.
    #[test]
    fn authorizes_with_a_key_or_a_signature() {
        let url = "https://bedrock-runtime.us-east-1.amazonaws.com/model/m%3A0/invoke-with-response-stream";
        let bearer = headers(
            url,
            b"{}",
            true,
            "us-east-1",
            &Authorization::Bearer("k".into()),
            "20261107T120000Z",
        )
        .unwrap();
        assert!(bearer.contains(&("authorization".into(), "Bearer k".into())));
        assert!(bearer.contains(&("accept".into(), "application/vnd.amazon.eventstream".into())));
        let keys = Keys {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "secret".into(),
            session_token: Some("tok".into()),
        };
        let signed = headers(
            url,
            b"{}",
            true,
            "us-east-1",
            &Authorization::Signed(keys.clone()),
            "20261107T120000Z",
        )
        .unwrap();
        let authorization = &signed
            .iter()
            .find(|(name, _)| name == "authorization")
            .unwrap()
            .1;
        assert!(authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20261107/us-east-1/bedrock/aws4_request, SignedHeaders=accept;content-type;host;x-amz-date;x-amz-security-token, Signature="
        ));
        let again = headers(
            url,
            b"{}",
            true,
            "us-east-1",
            &Authorization::Signed(keys.clone()),
            "20261107T120000Z",
        )
        .unwrap();
        assert_eq!(signed, again);
        let other = headers(
            url,
            b"{ }",
            true,
            "us-east-1",
            &Authorization::Signed(keys),
            "20261107T120000Z",
        )
        .unwrap();
        assert_ne!(signed, other);
    }

    /// Chunk events give their Messages event, other events nothing, and exceptions an error.
    #[test]
    fn reads_stream_messages() {
        let event = json!({ "type": "message_stop" }).to_string();
        let chunk = json!({ "bytes": STANDARD.encode(&event), "p": "abc" }).to_string();
        let mut bytes = frame(
            &[
                (":event-type", "chunk"),
                (":content-type", "application/json"),
                (":message-type", "event"),
            ],
            chunk.as_bytes(),
        );
        bytes.extend(frame(
            &[(":event-type", "other"), (":message-type", "event")],
            b"{}",
        ));
        bytes.extend(frame(
            &[
                (":exception-type", "throttlingException"),
                (":message-type", "exception"),
            ],
            br#"{"message":"Too many requests, please wait before trying again."}"#,
        ));
        let mut decoder = Decoder::default();
        decoder.push(&bytes);
        let first = decoder.next().unwrap().unwrap();
        assert_eq!(event_data(&first).unwrap().as_deref(), Some(event.as_str()));
        let second = decoder.next().unwrap().unwrap();
        assert_eq!(event_data(&second).unwrap(), None);
        let third = decoder.next().unwrap().unwrap();
        let error = event_data(&third).unwrap_err();
        assert!(error.to_string().contains("throttlingException"), "{error}");
        assert!(error.is_transient());
    }

    /// The models listed lead with the global profiles, then the region's geography, and are all Claude.
    #[test]
    fn lists_claude_models_for_the_region() {
        let listed = models("us-east-1");
        assert!(listed[0].starts_with("global.anthropic.claude"));
        assert!(listed.iter().any(|id| id.starts_with("us.anthropic.")));
        assert!(!listed.iter().any(|id| id.starts_with("eu.")));
        assert!(listed.iter().all(|id| id.contains("anthropic.claude")));
    }
}
