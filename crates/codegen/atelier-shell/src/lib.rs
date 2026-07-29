#![allow(
    unused_imports,
    unused_variables,
    unused_mut,
    unreachable_code,
    dead_code
)]
pub(crate) use atelier_telemetry::unified_log;
pub use atelier_tracing_macros::{teprintln, timed, tprintln};
pub mod active_sessions;
pub mod agent;
pub mod auth;
pub mod builtin;
pub mod claude_import;
pub mod claude_import_state;
pub mod cli_models;
pub mod config;
pub use atelier_shell_base::cpu_profile;
pub use atelier_shell_base::env;
pub mod extensions;
pub use atelier_workspace::foreign_sessions;
pub mod heap_profile;
pub use atelier_http as http;
pub mod inspect;
pub mod instrumentation;
pub mod leader;
pub(crate) mod local_artifacts;
pub mod local_runtime;
pub mod managed_config;
pub mod mcp_doctor;
pub use atelier_models as models;
pub mod plugin;
pub mod runtime_control;
pub mod runtime_defaults;
pub mod sampling;
pub mod session;
pub mod terminal;
#[cfg(test)]
pub(crate) mod test_support;
pub mod tier;
pub mod tools;
pub mod trace_classifier;
pub mod util;
