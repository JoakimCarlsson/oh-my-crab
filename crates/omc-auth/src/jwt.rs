//! Minimal JWT claim reading; signatures are not verified.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// Claim namespace OpenAI uses for ChatGPT account data.
const AUTH_CLAIM: &str = "https://api.openai.com/auth";

/// The claims in the payload of `token`, when it is a readable JWT.
fn claims(token: &str) -> Option<serde_json::Value> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Extracts the `email` claim from an OpenID token, if present.
pub fn email(token: &str) -> Option<String> {
    claims(token)?.get("email")?.as_str().map(str::to_owned)
}

/// Extracts `chatgpt_account_id` from an OpenAI access token, if present.
pub fn chatgpt_account_id(token: &str) -> Option<String> {
    claims(token)?
        .get(AUTH_CLAIM)?
        .get("chatgpt_account_id")?
        .as_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The account id is read from the namespaced claim of the payload segment.
    #[test]
    fn reads_account_id_from_payload() {
        let payload = URL_SAFE_NO_PAD
            .encode(br#"{"https://api.openai.com/auth":{"chatgpt_account_id":"acc-1"}}"#);
        let token = format!("h.{payload}.s");
        assert_eq!(chatgpt_account_id(&token).as_deref(), Some("acc-1"));
        assert_eq!(chatgpt_account_id("not-a-jwt"), None);
    }
}
