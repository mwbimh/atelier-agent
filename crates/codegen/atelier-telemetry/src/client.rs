//! Compatibility flags for legacy event call sites.
//!
//! Local observability is implemented by the file-log and tracing modules.
//! Event call sites that have not yet migrated remain permanently disabled.

pub fn is_enabled() -> bool {
    false
}

pub fn is_session_metrics_enabled() -> bool {
    false
}
