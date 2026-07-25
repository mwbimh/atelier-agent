//! Ambient session context for local tracing and legacy event call sites.

use std::sync::Arc;

use serde::Serialize;

use crate::events::TelemetryEvent;

/// Ambient session context for telemetry. Snapshotted synchronously by
/// `log_event` at call time to avoid racing with turn increments.
#[derive(Clone)]
pub struct TelemetryCtx {
    pub session_id: String,
    pub prompt_index: Arc<tokio::sync::Mutex<usize>>,
}

impl TelemetryCtx {
    pub fn new(session_id: String, prompt_index: Arc<tokio::sync::Mutex<usize>>) -> Self {
        Self {
            session_id,
            prompt_index,
        }
    }
}

tokio::task_local! {
    static TELEMETRY_CTX: Arc<TelemetryCtx>;
}

/// The `session_id` field name the debug-log firehose router keys on:
/// `debug_log::SessionIdVisitor` stashes a `SessionId` extension on any span
/// carrying this field — the span *name* is not load-bearing for routing. Shared
/// so the `info_span!` here and the router in `debug_log` can't silently drift; a
/// rename trips `session_span_exposes_router_field` below.
pub(crate) const SESSION_ID_FIELD: &str = "session_id";

/// Build the per-session tracing span the firehose router routes by. The field
/// name MUST be the literal `session_id` (tracing field names can't come from a
/// const); the test below pins it against [`SESSION_ID_FIELD`].
fn session_span(session_id: &str) -> tracing::Span {
    tracing::info_span!("session", session_id = %session_id)
}

/// Run `fut` with telemetry context active. Also sets a `tracing` span.
pub async fn with_session_ctx<F: std::future::Future>(ctx: TelemetryCtx, fut: F) -> F::Output {
    use tracing::Instrument;
    let span = session_span(&ctx.session_id);
    TELEMETRY_CTX
        .scope(Arc::new(ctx), fut.instrument(span))
        .await
}

/// Local runtime surface associated with a legacy event payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumCount)]
pub enum EmitterOrigin {
    /// `atelier-shell` (and the pager/TUI that emit through it).
    Shell,
    /// `atelier-workspace` (remote sampler / workspace server).
    Workspace,
}

impl EmitterOrigin {
    /// Every emitter origin.
    pub const ALL: [EmitterOrigin; 2] = [EmitterOrigin::Shell, EmitterOrigin::Workspace];

    /// Stable local event-name prefix for this origin.
    pub fn event_prefix(self) -> &'static str {
        match self {
            EmitterOrigin::Shell => "atelier-shell-",
            EmitterOrigin::Workspace => "atelier-workspace-",
        }
    }
}

/// Compile-time completeness guard for [`EmitterOrigin::ALL`].
const _: () = assert!(EmitterOrigin::ALL.len() == <EmitterOrigin as strum::EnumCount>::COUNT);

/// Compatibility event hook. Local tracing and event files are written by
/// their dedicated subsystems, so this legacy hook intentionally does nothing.
pub fn log_event<T: TelemetryEvent>(data: T) {
    let _ = data;
}

/// Compatibility helper for call sites with an explicit event gate.
pub fn log_event_dual<T: TelemetryEvent>(internal_enabled: bool, data: T) {
    if internal_enabled {
        log_event(data);
    }
}

/// Session lifecycle compatibility hook.
pub fn log_session_event<T: TelemetryEvent>(data: T) {
    let _ = data;
}

/// Session lifecycle compatibility hook tagged with its local origin.
pub fn log_session_event_with_origin<T: TelemetryEvent>(origin: EmitterOrigin, data: T) {
    let _ = (origin, data);
}

/// Emit an event with the default [`EmitterOrigin::Shell`] prefix.
pub fn emit_event<T: Serialize + Send + 'static>(event_suffix: impl Into<String>, data: T) {
    emit_event_with_origin(EmitterOrigin::Shell, event_suffix, data);
}

/// Consume a legacy event payload without creating a sink or background task.
pub fn emit_event_with_origin<T: Serialize + Send + 'static>(
    origin: EmitterOrigin,
    event_suffix: impl Into<String>,
    data: T,
) {
    let _ = (origin, event_suffix.into(), data);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The debug-log firehose router (`debug_log`) finds the session span by its
    /// `session_id` field (not by name). That field name is a literal in
    /// `session_span` (tracing field names can't be a const), so pin it against the
    /// shared const here — a rename of either breaks this test instead of silently
    /// degrading routing to the per-pid fallback.
    #[test]
    fn session_span_exposes_router_field() {
        // A bare registry enables every callsite, so the span has live metadata.
        let subscriber = tracing_subscriber::registry();
        tracing::subscriber::with_default(subscriber, || {
            let span = session_span("test-id");
            let meta = span
                .metadata()
                .expect("session span must have metadata under an enabling subscriber");
            assert!(
                meta.fields().field(SESSION_ID_FIELD).is_some(),
                "session span must expose `{SESSION_ID_FIELD}` for debug-log routing",
            );
        });
    }

    /// Event-name prefixes remain stable while legacy call sites are migrated.
    #[test]
    fn event_prefix_is_stable_per_origin() {
        assert_eq!(EmitterOrigin::Shell.event_prefix(), "atelier-shell-");
        assert_eq!(
            EmitterOrigin::Workspace.event_prefix(),
            "atelier-workspace-"
        );
    }

    /// The `Shell` reroute must reproduce the historical
    /// `format!("atelier-shell-{suffix}")` event name byte-for-byte, since every
    /// existing `log_session_event` / `log_event` / `emit_event` call funnels
    /// through `EmitterOrigin::Shell`.
    #[test]
    fn shell_origin_event_name_matches_legacy_format() {
        let suffix = "turn";
        let rerouted = format!("{}{}", EmitterOrigin::Shell.event_prefix(), suffix);
        let legacy = format!("atelier-shell-{suffix}");
        assert_eq!(rerouted, legacy);
    }

    #[test]
    fn workspace_origin_event_name_uses_workspace_prefix() {
        let name = format!("{}turn", EmitterOrigin::Workspace.event_prefix());
        assert_eq!(name, "atelier-workspace-turn");
    }

    /// `ALL` must enumerate every variant so the stripper in `client` can
    /// recover the `event_value` for any origin the emitter produces. Length
    /// completeness is also compiler-enforced by the `const _` assertion in
    /// this module (via `strum::EnumCount`); this test additionally pins that
    /// the known variants are present and that every origin yields a distinct,
    /// non-empty prefix (which `EnumCount` alone does not guarantee).
    #[test]
    fn all_covers_every_origin_with_distinct_nonempty_prefixes() {
        assert!(EmitterOrigin::ALL.contains(&EmitterOrigin::Shell));
        assert!(EmitterOrigin::ALL.contains(&EmitterOrigin::Workspace));
        assert_eq!(
            EmitterOrigin::ALL.len(),
            <EmitterOrigin as strum::EnumCount>::COUNT,
            "ALL must list every EmitterOrigin variant",
        );

        let mut prefixes: Vec<&str> = EmitterOrigin::ALL
            .iter()
            .map(|o| o.event_prefix())
            .collect();
        assert!(
            prefixes.iter().all(|p| !p.is_empty()),
            "every origin must have a non-empty prefix",
        );
        let total = prefixes.len();
        prefixes.sort_unstable();
        prefixes.dedup();
        assert_eq!(
            prefixes.len(),
            total,
            "every origin must yield a distinct prefix",
        );
    }
}
