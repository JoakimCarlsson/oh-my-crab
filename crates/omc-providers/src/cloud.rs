//! What Claude on Amazon Bedrock and Google Vertex AI needs besides the Messages body: finding each cloud's credentials, signing, addresses and Bedrock's binary event stream.

pub(crate) mod aws;
pub(crate) mod bedrock;
pub(crate) mod eventstream;
pub(crate) mod google;
pub(crate) mod google_token;
pub(crate) mod sigv4;
pub(crate) mod vertex;
