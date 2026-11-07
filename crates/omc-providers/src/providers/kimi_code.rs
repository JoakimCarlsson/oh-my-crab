//! Kimi Code, the Kimi For Coding subscription from Moonshot, signed in with an API key or with a Kimi account.
//!
//! Kimi Code serves its subscription over an OpenAI-compatible endpoint and an
//! Anthropic-compatible one; crab uses the OpenAI-compatible one, since its
//! Anthropic wire is built for Anthropic's own API.
//!
//! With an API key, from the environment or saved with the logins, requests go
//! to the overseas address and carry crab's own `User-Agent`, as Kimi asks
//! third-party tools to. Signing in with a Kimi account instead uses the device
//! login Kimi's own command-line client uses: its client id, and the headers it
//! names itself with, which Kimi's account login requires and which are kept
//! with the login and sent on every request made with it. Kimi's terms forbid
//! third-party tools changing their client identity, so the account login puts
//! the account at that risk; the README says so.

use omc_auth::{Auth, Device};

use crate::provider::{Def, Wire};

/// The `User-Agent` sent to Kimi Code with an API key, naming crab and its version.
const USER_AGENT: &str = concat!("oh-my-crab/", env!("CARGO_PKG_VERSION"));

/// The version of Kimi's command-line client the account login names.
const CLIENT_VERSION: &str = "1.52.0";

/// Where an account login's requests go.
const ACCOUNT_API: &str = "https://api.kimi.com/coding/v1";

/// Kimi's device login, at its account server.
static DEVICE: Device = Device {
    client_id: "17e5f671-d194-4dfb-9706-5516cb48c098",
    scope: "",
    code_url: "https://auth.kimi.com/api/oauth/device_authorization",
    token_url: "https://auth.kimi.com/api/oauth/token",
    headers: &[],
    endpoint_url: None,
    api_url: Some(ACCOUNT_API),
    identity: Some(client_identity),
};

/// The definition of the Kimi Code provider; `OMC_KIMI_CODE_URL` points requests made with an API key at another host,
/// such as `https://api.kimi.com/coding/v1` for subscriptions from the China site.
pub(super) static DEF: Def = Def {
    id: "kimi-code",
    label: "Kimi Code",
    wire: Wire::ChatCompletions,
    default_model: Some("kimi-for-coding"),
    auth: Auth::DeviceCodeOrKey {
        device: &DEVICE,
        env: &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
    },
    base_url: "https://api.kimi.ai/coding/v1",
    base_url_env: Some("OMC_KIMI_CODE_URL"),
    headers: &[("User-Agent", USER_AGENT)],
    replay_reasoning: true,
};

/// The headers Kimi's command-line client names itself and its machine with: its version, this machine's name, model and system version, and a new device id.
fn client_identity() -> Vec<(String, String)> {
    let header = |name: &str, value: String| (name.to_owned(), printable(&value));
    vec![
        header("User-Agent", format!("KimiCLI/{CLIENT_VERSION}")),
        header("X-Msh-Platform", "kimi_cli".to_owned()),
        header("X-Msh-Version", CLIENT_VERSION.to_owned()),
        header("X-Msh-Device-Name", host_name()),
        header("X-Msh-Device-Model", device_model()),
        header("X-Msh-Os-Version", kernel("version")),
        header("X-Msh-Device-Id", device_id()),
    ]
}

/// `value` with everything but printable ASCII left out, or `unknown` when nothing is left, so that it can be a header value.
fn printable(value: &str) -> String {
    let kept: String = value.chars().filter(|c| (' '..='~').contains(c)).collect();
    let kept = kept.trim();
    if kept.is_empty() {
        "unknown".to_owned()
    } else {
        kept.to_owned()
    }
}

/// What the Linux kernel says of itself under `name`, such as `osrelease` or `version`; empty elsewhere.
fn kernel(name: &str) -> String {
    std::fs::read_to_string(format!("/proc/sys/kernel/{name}"))
        .map(|text| text.trim().to_owned())
        .unwrap_or_default()
}

/// The name of this machine.
fn host_name() -> String {
    let named = kernel("hostname");
    if !named.is_empty() {
        return named;
    }
    ["HOSTNAME", "COMPUTERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_default()
}

/// The system, its release and the processor, as `Linux 6.9.1 x86_64`.
fn device_model() -> String {
    let system = match std::env::consts::OS {
        "linux" => "Linux",
        "macos" => "macOS",
        "windows" => "Windows",
        other => other,
    };
    [system, &kernel("osrelease"), std::env::consts::ARCH]
        .iter()
        .filter(|part| !part.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A new random id for this machine's login, as 32 hexadecimal digits.
fn device_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = |salt: u64| {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(salt);
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        hasher.write_u32(std::process::id());
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(1), half(2))
}

#[cfg(test)]
/// Tests for the Kimi Code definition.
mod tests {
    use super::*;

    /// With a key, Kimi Code is reached over Chat Completions as crab itself at the overseas address, and its thinking goes back with tool calls.
    #[test]
    fn a_key_identifies_as_crab() {
        assert_eq!(DEF.wire, Wire::ChatCompletions);
        assert_eq!(DEF.base_url, "https://api.kimi.ai/coding/v1");
        assert!(
            DEF.headers
                .iter()
                .any(|(name, value)| *name == "User-Agent" && value.starts_with("oh-my-crab/"))
        );
        assert!(DEF.replay_reasoning);
        assert!(
            matches!(DEF.auth, Auth::DeviceCodeOrKey { env, .. } if env[0] == "KIMI_CODE_API_KEY")
        );
    }

    /// An account login goes to the subscription address and names Kimi's client with printable header values and a fresh device id each time.
    #[test]
    fn an_account_login_names_kimis_client() {
        assert_eq!(DEVICE.api_url, Some(ACCOUNT_API));
        let headers = client_identity();
        let get = |name: &str| {
            headers
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .unwrap()
        };
        assert_eq!(get("User-Agent"), format!("KimiCLI/{CLIENT_VERSION}"));
        assert_eq!(get("X-Msh-Platform"), "kimi_cli");
        let id = get("X-Msh-Device-Id");
        assert_eq!(id.len(), 32);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(id, device_id());
        assert!(
            headers
                .iter()
                .all(|(_, value)| value.bytes().all(|b| (0x20..0x7f).contains(&b)))
        );
        assert_eq!(printable("héllo\n"), "hllo");
        assert_eq!(printable("ü"), "unknown");
    }
}
