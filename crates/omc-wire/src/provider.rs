//! The providers a harness can hold a conversation through, as a client lists them.

/// How a provider is signed in to.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    /// A login page in the browser that hands back to the harness when done, reported as a [`crate::Event::LoginUrl`].
    Browser,
    /// A short code the reader enters on a web page, reported as a [`crate::Event::LoginCode`].
    DeviceCode,
    /// A key read from an environment variable; signing in fails when it is not set.
    ApiKey,
    /// Nothing: the provider needs no credentials, as for a local server.
    None,
}

impl SignIn {
    /// Whether signing in names an account, so that a client can keep several logins of the provider apart.
    pub fn has_accounts(self) -> bool {
        matches!(self, Self::Browser | Self::DeviceCode)
    }
}

/// One provider a conversation can be opened with.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ProviderInfo {
    /// The id given when opening a conversation or signing in, such as `claude`.
    pub id: String,
    /// The name a client shows for it, such as `GitHub Copilot`.
    pub name: String,
    /// How it is signed in to.
    pub sign_in: SignIn,
    /// The environment variables an API key is read from, first first; empty when the provider takes no key.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_env: Vec<String>,
}

/// Where the API key a provider is signed in with comes from.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case", tag = "from")]
pub enum KeySource {
    /// The environment variable `name`, which wins over a saved key.
    Environment {
        /// The variable the key was read from.
        name: String,
    },
    /// A key saved with the harness's logins.
    Saved,
}
