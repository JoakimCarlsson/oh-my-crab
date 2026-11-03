//! The Claude Code identity that subscription OAuth tokens must present.

use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::xxh64::xxh64;

/// Claude Code version used until the server names a newer minimum.
const DEFAULT_VERSION: &str = "2.1.280";

/// Version of the `@anthropic-ai/sdk` bundled with that Claude Code release.
const SDK_VERSION: &str = "0.112.1";

/// The text of the identity block that opens the system prompt.
pub const IDENTITY: &str = "You are Claude Code, Anthropic's official CLI for Claude.";

/// Prefix of the billing header block that opens the system prompt.
pub const BILLING_PREFIX: &str = "x-anthropic-billing-header:";

/// Placeholder the attestation hash is computed over and then replaces.
const CCH_PLACEHOLDER: &str = "cch=00000";

/// Seed of the attestation hash.
const CCH_SEED: u64 = 0x4d65_9218_e32a_3268;

/// Most bytes between the billing prefix and its placeholder.
const CCH_SEARCH_WINDOW: usize = 150;

/// Salt of the version fingerprint in the billing header.
const FINGERPRINT_SALT: &str = "59cf53e54c78";

/// Prefix put on tool names on the wire.
const TOOL_PREFIX: &str = "_";

/// Tool names of Anthropic's own tools, which are never prefixed.
const BUILTIN_TOOLS: [&str; 4] = ["web_search", "code_execution", "text_editor", "computer"];

/// Betas of Claude Code's agent requests, in the order it sends them.
const AGENT_BETAS: [&str; 7] = [
    "claude-code-20250219",
    "oauth-2025-04-20",
    "interleaved-thinking-2025-05-14",
    "thinking-token-count-2026-05-13",
    "context-management-2025-06-27",
    "prompt-caching-scope-2026-01-05",
    "mid-conversation-system-2026-04-07",
];

/// Beta that carries `output_config.effort`.
const EFFORT_BETA: &str = "effort-2025-11-24";

/// Beta Claude Code adds to every agent request after the thinking ones.
const FALLBACK_CREDIT_BETA: &str = "fallback-credit-2026-06-01";

/// Beta that allows interleaved thinking on models that need it announced.
const INTERLEAVED_BETA: &str = "interleaved-thinking-2025-05-14";

/// The version adopted from a server rejection, when there was one.
static ADOPTED_VERSION: Mutex<Option<String>> = Mutex::new(None);

/// The identifier shared by every request of this process.
static SESSION_ID: OnceLock<String> = OnceLock::new();

/// Returns whether `token` is a Claude subscription OAuth token rather than an API key.
pub fn is_oauth_token(token: &str) -> bool {
    token.contains("sk-ant-oat")
}

/// The Claude Code version the requests claim to come from.
pub fn version() -> String {
    ADOPTED_VERSION
        .lock()
        .ok()
        .and_then(|adopted| adopted.clone())
        .unwrap_or_else(|| DEFAULT_VERSION.to_string())
}

/// The `User-Agent` of Claude Code's inference entrypoint.
pub fn user_agent() -> String {
    format!("claude-cli/{} (external, cli)", version())
}

/// Splits `version` into its three numbers when it has the form `a.b.c`.
fn parse_semver(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.split('.').map(|part| part.parse::<u64>().ok());
    let numbers = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(numbers)
}

/// Adopts the minimum version a `claude_code_version_too_old` rejection names.
///
/// Returns true only when the claimed version actually rose, so a repeated
/// rejection cannot loop.
pub fn adopt_required_version(error_body: &str) -> bool {
    if !error_body.contains("claude_code_version_too_old") {
        return false;
    }
    let Some(required) = required_version(error_body) else {
        return false;
    };
    let (Some(wanted), Some(current)) = (parse_semver(&required), parse_semver(&version())) else {
        return false;
    };
    if wanted <= current {
        return false;
    }
    match ADOPTED_VERSION.lock() {
        Ok(mut adopted) => {
            *adopted = Some(required);
            true
        }
        Err(_) => false,
    }
}

/// Finds the `X.Y.Z` in "version X.Y.Z or newer is required".
fn required_version(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let start = lower.find("version ")? + "version ".len();
    let word: String = lower[start..]
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let tail = &lower[start + word.len()..];
    (tail.trim_start().starts_with("or newer is required") && parse_semver(&word).is_some())
        .then_some(word)
}

/// Formats sixteen random bytes as a version 4 UUID.
fn uuid_v4(mut bytes: [u8; 16]) -> String {
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// The session id every request of this process carries, created on first use.
pub fn session_id() -> &'static str {
    SESSION_ID.get_or_init(|| {
        let mut bytes = [0u8; 16];
        let _ = getrandom::fill(&mut bytes);
        uuid_v4(bytes)
    })
}

/// Hex-encodes the SHA-256 of `parts` joined by NUL bytes.
fn sha256_hex(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (index, part) in parts.iter().enumerate() {
        if index > 0 {
            hasher.update([0u8]);
        }
        hasher.update(part.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The stable device id for `account_id`, which anchors `metadata.user_id`.
pub fn device_id(account_id: Option<&str>) -> String {
    sha256_hex(&["omc-claude-device-id-v1", account_id.unwrap_or("")])
}

/// The JSON string Claude Code puts in `metadata.user_id`.
pub fn metadata_user_id(account_id: Option<&str>, session_id: &str) -> String {
    let mut user = json!({
        "device_id": device_id(account_id),
        "session_id": session_id,
    });
    if let Some(account) = account_id.filter(|account| !account.is_empty()) {
        user["account_uuid"] = Value::String(account.to_string());
    }
    user.to_string()
}

/// The billing header block text for a conversation whose first user message is `first_user`.
///
/// The fingerprint hashes characters 4, 7 and 20 of that message (UTF-16 units,
/// `0` when short) with the version, and the `cch` is left as the placeholder
/// that [`attest`] fills in.
pub fn billing_header(first_user: &str) -> String {
    billing_header_for(&version(), first_user)
}

/// The billing header block text for the claimed `version` and a conversation whose first user message is `first_user`.
fn billing_header_for(version: &str, first_user: &str) -> String {
    let units: Vec<u16> = first_user.encode_utf16().collect();
    let picked: String = [4usize, 7, 20]
        .iter()
        .map(|index| {
            units
                .get(*index)
                .and_then(|unit| char::from_u32(u32::from(*unit)))
                .unwrap_or('0')
        })
        .collect();
    let seed = format!("{FINGERPRINT_SALT}{picked}{version}");
    let suffix = &sha256_hex(&[&seed])[..3];
    format!("{BILLING_PREFIX} cc_version={version}.{suffix}; cc_entrypoint=cli; {CCH_PLACEHOLDER};")
}

/// Replaces the `cch` placeholder in a serialized request `body` with its attestation hash.
///
/// The hash is the xxHash64 of the body with the placeholder in place, seeded
/// as Claude Code does, keeping the low 20 bits as five hex digits. Returns
/// false and leaves the body alone when the billing block cannot be found.
pub fn attest(body: &mut [u8]) -> bool {
    let anchor = format!("\"text\":\"{BILLING_PREFIX}");
    let Some(marker) = rfind(body, anchor.as_bytes()) else {
        return false;
    };
    let from = marker + anchor.len();
    let Some(offset) = find(&body[from..], CCH_PLACEHOLDER.as_bytes()) else {
        return false;
    };
    if offset > CCH_SEARCH_WINDOW {
        return false;
    }
    let at = from + offset;
    let hash = xxh64(body, CCH_SEED) & 0xf_ffff;
    let digits = format!("{hash:05x}");
    body[at + 4..at + 9].copy_from_slice(digits.as_bytes());
    true
}

/// The first position of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The last position of `needle` in `haystack`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// Puts the wire-only prefix on a tool `name`, except for Anthropic's own tools.
pub fn prefix_tool(name: &str) -> String {
    if BUILTIN_TOOLS.contains(&name.to_ascii_lowercase().as_str()) {
        name.to_string()
    } else {
        format!("{TOOL_PREFIX}{name}")
    }
}

/// Removes the wire-only prefix from a tool `name` the model sent back.
pub fn strip_tool_prefix(name: &str) -> &str {
    name.strip_prefix(TOOL_PREFIX).unwrap_or(name)
}

/// Builds the `anthropic-beta` list for an OAuth request.
///
/// `thinking` is whether the request asks for thinking or carries an effort,
/// which adds the effort beta. The long-context beta is never sent: subscription
/// tokens have no long-context credit and Anthropic answers 429 to it.
pub fn oauth_betas(thinking: bool) -> Vec<&'static str> {
    let mut betas = AGENT_BETAS.to_vec();
    if thinking {
        betas.push(EFFORT_BETA);
    }
    betas.push(FALLBACK_CREDIT_BETA);
    betas
}

/// Builds the `anthropic-beta` list for an API-key request.
///
/// `budget_thinking` is whether the request uses a thinking budget, which
/// needs interleaved thinking announced, and `effort` whether it carries an
/// effort.
pub fn api_key_betas(budget_thinking: bool, effort: bool) -> Vec<&'static str> {
    let mut betas = Vec::new();
    if budget_thinking {
        betas.push(INTERLEAVED_BETA);
    }
    if effort {
        betas.push(EFFORT_BETA);
    }
    betas
}

/// Maps the build target's OS to the Stainless SDK's wire value.
fn stainless_os() -> String {
    match std::env::consts::OS {
        "macos" => "MacOS".into(),
        "windows" => "Windows".into(),
        "linux" => "Linux".into(),
        "freebsd" => "FreeBSD".into(),
        other => format!("Other::{other}"),
    }
}

/// Maps the build target's architecture to the Stainless SDK's wire value.
fn stainless_arch() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "x64".into(),
        "aarch64" => "arm64".into(),
        "x86" => "x86".into(),
        other => format!("other::{other}"),
    }
}

/// The headers of a Messages request, as name and value pairs.
///
/// An OAuth `token` gets the full Claude Code fingerprint and a bearer
/// header; an API key gets only `x-api-key` and the SDK-independent headers.
pub fn headers(token: &str, betas: &[&str], stream: bool) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let mut put = |name: &str, value: String| headers.push((name.to_string(), value));
    if is_oauth_token(token) {
        put("Accept", "application/json".into());
        put("Content-Type", "application/json".into());
        put("User-Agent", user_agent());
        put("X-Claude-Code-Session-Id", session_id().to_string());
        put("X-Stainless-Arch", stainless_arch());
        put("X-Stainless-Lang", "js".into());
        put("X-Stainless-OS", stainless_os());
        put("X-Stainless-Package-Version", SDK_VERSION.into());
        put("X-Stainless-Retry-Count", "0".into());
        put("X-Stainless-Runtime", "node".into());
        put("X-Stainless-Runtime-Version", "v26.3.0".into());
        put("X-Stainless-Timeout", "600".into());
        if !betas.is_empty() {
            put("anthropic-beta", betas.join(","));
        }
        put("anthropic-dangerous-direct-browser-access", "true".into());
        put("anthropic-version", "2023-06-01".into());
        put("Authorization", format!("Bearer {token}"));
        put("x-app", "cli".into());
    } else {
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        put("Accept", accept.into());
        put("Content-Type", "application/json".into());
        put("anthropic-dangerous-direct-browser-access", "true".into());
        put("x-app", "cli".into());
        put("anthropic-version", "2023-06-01".into());
        if !betas.is_empty() {
            put("anthropic-beta", betas.join(","));
        }
        put("X-Api-Key", token.to_string());
    }
    headers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Looks up header `name` in `headers`.
    fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// OAuth requests carry the Claude Code fingerprint and a bearer token.
    #[test]
    fn oauth_headers_carry_the_fingerprint() {
        let betas = oauth_betas(true);
        let headers = headers("sk-ant-oat01-abc", &betas, true);
        assert!(
            header(&headers, "user-agent")
                .unwrap()
                .starts_with("claude-cli/")
        );
        assert!(
            header(&headers, "user-agent")
                .unwrap()
                .ends_with("(external, cli)")
        );
        assert_eq!(header(&headers, "x-app"), Some("cli"));
        assert_eq!(
            header(&headers, "authorization"),
            Some("Bearer sk-ant-oat01-abc")
        );
        assert_eq!(header(&headers, "x-stainless-lang"), Some("js"));
        assert_eq!(header(&headers, "x-stainless-timeout"), Some("600"));
        assert_eq!(
            header(&headers, "x-claude-code-session-id"),
            Some(session_id())
        );
        assert_eq!(
            header(&headers, "anthropic-dangerous-direct-browser-access"),
            Some("true")
        );
        assert!(header(&headers, "x-api-key").is_none());
        let beta = header(&headers, "anthropic-beta").unwrap();
        assert!(beta.starts_with("claude-code-20250219,oauth-2025-04-20,"));
        assert!(beta.contains("effort-2025-11-24"));
        assert!(!beta.contains("context-1m"));
    }

    /// API keys travel in `x-api-key` and never as a bearer or with OAuth betas.
    #[test]
    fn api_key_headers_use_x_api_key() {
        let headers = headers("sk-ant-api03-abc", &api_key_betas(true, false), true);
        assert_eq!(header(&headers, "x-api-key"), Some("sk-ant-api03-abc"));
        assert!(header(&headers, "authorization").is_none());
        assert_eq!(header(&headers, "accept"), Some("text/event-stream"));
        assert_eq!(
            header(&headers, "anthropic-beta"),
            Some("interleaved-thinking-2025-05-14")
        );
    }

    /// The tool prefix round-trips and leaves Anthropic's own tools alone.
    #[test]
    fn tool_prefix_round_trips() {
        assert_eq!(prefix_tool("read"), "_read");
        assert_eq!(strip_tool_prefix(&prefix_tool("read")), "read");
        assert_eq!(prefix_tool("web_search"), "web_search");
        assert_eq!(strip_tool_prefix("bash"), "bash");
    }

    /// The billing header carries version, fingerprint and the placeholder.
    #[test]
    fn billing_header_shape() {
        let text = billing_header("hello there, this is a long first message");
        assert!(text.starts_with("x-anthropic-billing-header: cc_version="));
        assert!(text.contains("; cc_entrypoint=cli; cch=00000;"));
        let suffix = text
            .split("cc_version=")
            .nth(1)
            .and_then(|rest| rest.split(';').next())
            .and_then(|version| version.rsplit('.').next())
            .unwrap();
        assert_eq!(suffix.len(), 3);
        assert!(suffix.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The fingerprint picks characters 4, 7 and 20, padding short messages with zeros.
    #[test]
    fn fingerprint_depends_on_picked_characters() {
        let first = "abcdXefYhijklmnopqrsZuvw";
        let same_picks = "zzzzXzzYzzzzzzzzzzzzZzzz";
        let other_pick = "abcdQefYhijklmnopqrsZuvw";
        assert_eq!(
            billing_header_for("1.0.0", first),
            billing_header_for("1.0.0", same_picks)
        );
        assert_ne!(
            billing_header_for("1.0.0", first),
            billing_header_for("1.0.0", other_pick)
        );
        assert_eq!(
            billing_header_for("1.0.0", ""),
            billing_header_for("1.0.0", "abc")
        );
    }

    /// Attestation fills five hex digits, hashed over the placeholder body.
    #[test]
    fn attest_patches_the_placeholder() {
        let block = billing_header("hi");
        let body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "x-anthropic-billing-header: cch=00000" }],
            "system": [{ "type": "text", "text": block }],
        })
        .to_string();
        let mut bytes = body.clone().into_bytes();
        assert!(attest(&mut bytes));
        let patched = String::from_utf8(bytes.clone()).unwrap();
        assert!(!patched.ends_with(&body));
        let system_part = patched.split("\"system\"").nth(1).unwrap();
        assert!(!system_part.contains("cch=00000"));
        let digits = system_part.split("cch=").nth(1).unwrap();
        assert!(digits[..5].chars().all(|c| c.is_ascii_hexdigit()));
        let user_part = patched.split("\"system\"").next().unwrap();
        assert!(user_part.contains("cch=00000"));
        let expected = xxh64(body.as_bytes(), CCH_SEED) & 0xf_ffff;
        assert_eq!(&digits[..5], format!("{expected:05x}"));
    }

    /// A body with no billing block is left untouched.
    #[test]
    fn attest_leaves_other_bodies_alone() {
        let mut bytes = br#"{"system":[{"type":"text","text":"hi"}]}"#.to_vec();
        let before = bytes.clone();
        assert!(!attest(&mut bytes));
        assert_eq!(bytes, before);
    }

    /// The metadata user id is JSON with device, session and account.
    #[test]
    fn metadata_user_id_is_json() {
        let raw = metadata_user_id(Some("acc-1"), "sess-1");
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["session_id"], "sess-1");
        assert_eq!(parsed["account_uuid"], "acc-1");
        assert_eq!(parsed["device_id"].as_str().unwrap().len(), 64);
        let anonymous: Value = serde_json::from_str(&metadata_user_id(None, "s")).unwrap();
        assert!(anonymous.get("account_uuid").is_none());
    }

    /// Session ids are well-formed version 4 UUIDs.
    #[test]
    fn uuid_shape() {
        let id = uuid_v4([0xff; 16]);
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes()[14], b'4');
        assert!(matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b'));
    }

    /// The required version is read out of the server's rejection and only ever rises.
    #[test]
    fn version_too_old_is_adopted_once() {
        let body = r#"{"error":{"type":"claude_code_version_too_old","message":"Claude Code version 9.9.9 or newer is required"}}"#;
        assert_eq!(required_version(body).as_deref(), Some("9.9.9"));
        assert!(adopt_required_version(body));
        assert_eq!(version(), "9.9.9");
        assert!(user_agent().contains("9.9.9"));
        assert!(!adopt_required_version(body));
        assert!(!adopt_required_version("some other error"));
    }
}
