//! Claude on Google Vertex AI, with the Google Cloud credentials Google's tools find.

use omc_auth::Auth;

use crate::cloud::google;
use crate::provider::{Def, Wire};

/// The definition of the Google Vertex AI provider.
///
/// `{region}` in the address is the region in use, and the global endpoint has none; `OMC_VERTEX_URL` replaces the whole address.
pub(super) static DEF: Def = Def {
    id: "vertex",
    label: "Google Vertex AI",
    wire: Wire::Vertex,
    default_model: Some("claude-sonnet-5-5"),
    auth: Auth::Cloud {
        env: &[],
        check: google::check,
        credentials: "Google Cloud credentials (gcloud or Application Default Credentials)",
    },
    base_url: "https://{region}-aiplatform.googleapis.com",
    base_url_env: Some("OMC_VERTEX_URL"),
    headers: &[],
    replay_reasoning: false,
};
