//! Local observability for Atelier sessions.
//!
//! This crate owns local JSONL/debug logs, tracing layers, profiling and
//! in-process event compatibility. It deliberately contains no remote sink,
//! crash reporter, exporter or upload client.

mod appender;
pub mod client;
pub mod config;
pub mod context;
pub mod debug_log;
pub mod enums;
pub mod events;
mod home;
pub mod hooks_log;
pub mod id;
pub mod instrumentation;
pub mod memory_log;
pub mod memory_telemetry;
pub mod prompt_timing;
pub(crate) mod redact_common;
pub mod sampling_log;
pub mod session_ctx;
pub mod session_metrics;
pub mod unified_log;

pub use client::{is_enabled, is_session_metrics_enabled};
pub use events::TelemetryEvent;
pub use session_ctx::{
    EmitterOrigin, TelemetryCtx, emit_event, emit_event_with_origin, log_event, log_session_event,
    log_session_event_with_origin, with_session_ctx,
};
