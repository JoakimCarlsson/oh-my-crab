//! OAuth login, credential storage and token refresh for oh-my-crab.

mod callback;
mod credentials;
mod error;
mod flow;
mod http;
mod jwt;
mod provider;
mod session;
mod store;

pub use credentials::Credentials;
pub use error::AuthError;
pub use provider::Provider;
pub use session::{active_credentials, active_provider, login};
