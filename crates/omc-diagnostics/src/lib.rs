//! What a program needs to tell its makers what went wrong.
//!
//! [`CrashLog`] keeps the last panics a program hit, written by the hook
//! [`install`] sets, so the next start can say so through
//! [`CrashLog::take_unnoticed`]. [`Report`] bundles facts about the machine,
//! those crashes and any other files into a zip the user can send by hand.
//! [`Telemetry`] sends the spans and metrics of the `tracing` macros to an
//! OpenTelemetry collector over OTLP, when the crate is built with the `otlp`
//! feature and the `OTEL_EXPORTER_OTLP_*ENDPOINT` variables name one.

mod crash;
mod report;
mod telemetry;

pub use crash::{Crash, CrashLog, install, now_ms};
pub use report::{Facts, Report};
pub use telemetry::Telemetry;
