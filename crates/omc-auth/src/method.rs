//! How a provider is signed in to: the descriptions the login flows run from.

/// Static description of a provider's OAuth endpoints and parameters.
pub struct Spec {
    /// Public OAuth client id.
    pub client_id: &'static str,
    /// Browser authorization endpoint.
    pub authorize_url: &'static str,
    /// Token endpoint used for exchange and refresh.
    pub token_url: &'static str,
    /// Requested scopes.
    pub scopes: &'static [&'static str],
    /// Extra query parameters on the authorization URL.
    pub authorize_params: &'static [(&'static str, &'static str)],
    /// Local callback port.
    pub port: u16,
    /// Local callback path.
    pub path: &'static str,
    /// Redirect URI registered with the provider.
    pub redirect_uri: &'static str,
    /// Whether the token endpoint expects a JSON body instead of a form.
    pub json_body: bool,
    /// Whether the authorization `state` is echoed in the code exchange.
    pub state_in_exchange: bool,
    /// Extra headers sent on refresh requests.
    pub refresh_headers: &'static [(&'static str, &'static str)],
    /// Whether the account id is read from the access token's JWT claims.
    pub account_from_jwt: bool,
}

/// Makes the headers, as names and values, that name a client to a provider.
pub type Identity = fn() -> Vec<(String, String)>;

/// Static description of a provider's device-code login.
pub struct Device {
    /// Public OAuth client id.
    pub client_id: &'static str,
    /// Requested scope.
    pub scope: &'static str,
    /// Endpoint that starts the flow and returns the code to show.
    pub code_url: &'static str,
    /// Endpoint polled until the user has approved.
    pub token_url: &'static str,
    /// Headers sent on every request of the flow.
    pub headers: &'static [(&'static str, &'static str)],
    /// Endpoint that tells which API address serves the account, when that varies.
    pub endpoint_url: Option<&'static str>,
    /// The API address every login made this way is served from, when it differs from the provider's own.
    pub api_url: Option<&'static str>,
    /// Makes the headers that name the client to the provider, once at login: they are sent with the flow's requests and kept with the login for every request made with it.
    pub identity: Option<Identity>,
}

/// What a login asks the user to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginPrompt {
    /// Open this address in a browser.
    Url(String),
    /// Open this address and enter this code there.
    Code {
        /// Where to enter the code.
        url: String,
        /// The code to enter.
        code: String,
    },
}

/// How a provider is signed in to.
pub enum Auth {
    /// The browser authorization-code flow with PKCE.
    OAuth(&'static Spec),
    /// The device-code flow: the user enters a short code on a web page.
    DeviceCode(&'static Device),
    /// Either the device-code flow, signing in to an account, or a static key read from the first of these environment variables that is set or saved with the logins; a key from the environment wins.
    DeviceCodeOrKey {
        /// How the account is signed in to.
        device: &'static Device,
        /// Environment variables a key is read from, in order.
        env: &'static [&'static str],
    },
    /// A static key read from the first of these environment variables that is set.
    ApiKey {
        /// Environment variables checked in order.
        env: &'static [&'static str],
    },
    /// Credentials a cloud's own tools would find, in its environment variables and configuration files, which the provider reads itself on every request; a key from the first of `env` that is set, or one saved with the logins, is used instead when there is one.
    Cloud {
        /// Environment variables a key is read from, in order; empty when the cloud takes no key.
        env: &'static [&'static str],
        /// Says whether the cloud's credentials can be found, with what to set when they cannot; signing in runs it when there is no key.
        check: fn() -> Result<(), String>,
    },
    /// No credentials at all, as for a local server.
    None,
}
