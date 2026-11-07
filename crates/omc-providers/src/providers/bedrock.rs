//! Claude on Amazon Bedrock, with the AWS credentials the AWS tools find or a Bedrock API key.

use omc_auth::Auth;

use crate::cloud::aws;
use crate::provider::{Def, Wire};

/// The definition of the Amazon Bedrock provider.
///
/// `{region}` in the address is the region in use; `OMC_BEDROCK_URL` replaces the whole address, as for a VPC endpoint.
pub(super) static DEF: Def = Def {
    id: "bedrock",
    label: "Amazon Bedrock",
    wire: Wire::Bedrock,
    default_model: Some("global.anthropic.claude-sonnet-5-5"),
    auth: Auth::Cloud {
        env: &["AWS_BEARER_TOKEN_BEDROCK"],
        check: aws::check,
        credentials: "AWS credentials (the AWS CLI's profiles, SSO or environment)",
    },
    base_url: "https://bedrock-runtime.{region}.amazonaws.com",
    base_url_env: Some("OMC_BEDROCK_URL"),
    headers: &[],
    replay_reasoning: false,
};
