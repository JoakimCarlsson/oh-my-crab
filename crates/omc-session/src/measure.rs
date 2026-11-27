//! Telemetry of model requests: a span per request and metrics of its tokens and duration, as the OpenTelemetry conventions for generative AI name them.
//!
//! Nothing is measured unless the program set a `tracing` subscriber, as `crab` does when telemetry is sent.

use std::future::Future;
use std::time::Instant;

use omc_providers::{Provider, ProviderError, Reply};
use tracing::Instrument;

/// Awaits `request`, a chat request to `model` of `provider`, inside a `chat` span, and records what it cost on the span and as the `gen_ai.client.token.usage` and `gen_ai.client.operation.duration` metrics.
///
/// # Errors
///
/// Passes on the request's error, recorded on the span as its `error.type`.
pub(crate) async fn chat(
    provider: Provider,
    model: &str,
    request: impl Future<Output = Result<Reply, ProviderError>>,
) -> Result<Reply, ProviderError> {
    let span = tracing::info_span!(
        "chat",
        otel.name = format!("chat {model}"),
        gen_ai.operation.name = "chat",
        gen_ai.provider.name = provider.def().id,
        gen_ai.request.model = model,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
        "error.type" = tracing::field::Empty,
    );
    if span.is_disabled() {
        return request.await;
    }
    let started = Instant::now();
    let answered = request.instrument(span.clone()).await;
    let seconds = started.elapsed().as_secs_f64();
    let _entered = span.enter();
    let id = provider.def().id;
    match &answered {
        Ok(reply) => {
            if let Some(tokens) = reply.tokens {
                span.record("gen_ai.usage.input_tokens", tokens.context());
                span.record("gen_ai.usage.output_tokens", tokens.output);
                tracing::info!(
                    histogram.gen_ai.client.token.usage = tokens.context(),
                    "gen_ai.token.type" = "input",
                    gen_ai.provider.name = id,
                    gen_ai.request.model = model,
                );
                tracing::info!(
                    histogram.gen_ai.client.token.usage = tokens.output,
                    "gen_ai.token.type" = "output",
                    gen_ai.provider.name = id,
                    gen_ai.request.model = model,
                );
            }
            tracing::info!(
                histogram.gen_ai.client.operation.duration = seconds,
                gen_ai.provider.name = id,
                gen_ai.request.model = model,
            );
        }
        Err(_) => {
            span.record("error.type", "provider_error");
            tracing::info!(
                histogram.gen_ai.client.operation.duration = seconds,
                gen_ai.provider.name = id,
                gen_ai.request.model = model,
                "error.type" = "provider_error",
            );
        }
    }
    answered
}
