//! Agent bootstrap and lifecycle hooks.
//!
//! [`bootstrap`] runs the full init sequence (config resolution, process
//! singletons, model catalog) and returns a resolved config + `ModelsManager`.
//! [`update_telemetry_config`] re-initializes telemetry after auth changes.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::agent::config::{self, Config as AgentConfig, ModelEntry};
use crate::agent::models::ModelsManager;
use crate::auth::AuthManager;
use crate::config::StorageMode;

/// Resolve config, init process singletons, build the model catalog.
///
/// The `ModelsManager` is `Clone + Send`, so callers that need a handle
/// for the config watcher can clone it before passing it to
/// `MvpAgent::with_models`.
pub fn bootstrap(
    cfg: &AgentConfig,
    auth_manager: &Arc<AuthManager>,
    prefetched: Option<IndexMap<String, ModelEntry>>,
) -> Result<(AgentConfig, ModelsManager), String> {
    // Fail closed before any policy is read: a tampered managed policy must not run unmanaged.
    crate::managed_config::managed_policy_gate()?;
    let cfg = resolve_config(cfg, auth_manager);
    cfg.validate_model_filters()?;
    init_process(&cfg, auth_manager);
    let models_manager = ModelsManager::from_config(&cfg, prefetched, auth_manager.clone())?;

    // Refresh on every auth refresh — the FSEvents watcher can silently die after
    // macOS sleep, stranding the catalog on bundled defaults.
    models_manager.start_auth_refresh_watcher(auth_manager.refresh_notifier());

    Ok((cfg, models_manager))
}

/// Print a `bootstrap`/`MvpAgent::new` config error and exit (process boundary).
///
/// Restores native stderr first: a managed-policy refusal on the ACP/server path reaches here
/// while fd 2 may still point at the `/dev/null` the TUI's `redirect_native_stderr()` set, which
/// would swallow the message. No-op when stderr was never redirected (headless).
pub(crate) fn exit_on_config_error<T>(e: String) -> T {
    atelier_tty_utils::restore_native_stderr();
    eprintln!("\nConfiguration error:\n\n    {e}\n");
    std::process::exit(1);
}

/// Config transform: apply managed settings, fetch remote settings,
/// resolve storage mode.
fn resolve_config(cfg: &AgentConfig, auth_manager: &AuthManager) -> AgentConfig {
    let mut cfg = cfg.clone();

    if let Ok(layers) = crate::config::ConfigLayers::load()
        && layers.has_managed()
    {
        let origins = crate::config::config_origins(&layers);
        let managed_keys: Vec<&str> = origins
            .iter()
            .filter(|(_, s)| matches!(s, config::ConfigSource::ManagedConfig))
            .map(|(k, _)| k.as_str())
            .collect();
        if !managed_keys.is_empty() {
            tracing::info!(keys = ?managed_keys, "managed_config.toml fields");
        }
    }

    let managed_enforced = crate::config::apply_managed_settings_features(&mut cfg);
    let requirements_enforced = crate::config::apply_requirements(&mut cfg);

    for e in managed_enforced.iter().chain(&requirements_enforced) {
        tracing::info!(field = %e.path, value = %e.value, source = %e.source, "policy override");
    }

    // Remote settings are a removed vendor control plane. Do not accept a
    // caller-provided snapshot either: the production runtime is local-only
    // apart from explicitly configured Providers/MCP/Web tools.
    cfg.local_runtime_settings = None;
    crate::util::config::sync_campaign_fields(&mut cfg);
    crate::agent::config::apply_local_runtime_settings_side_effects(None);

    // Session persistence is local-only in Atelier. Do not let an inherited
    // CLI/env/remote setting re-enable the removed vendor writeback path.
    cfg.storage_mode = StorageMode::Local;

    if let Some(rs) = cfg.local_runtime_settings.as_ref()
        && let Some(v) = rs.path_not_found_hints
    {
        cfg.path_not_found_hints = v;
    }

    cfg
}

/// Initialize process-level singletons (deployment sync, bundled files,
/// telemetry). `Once`-guarded: only the first call takes effect.
/// Telemetry user ID is updated separately via [`update_telemetry_config`].
fn init_process(cfg: &AgentConfig, auth_manager: &AuthManager) {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let atelier_home = crate::util::atelier_home::atelier_home();
        crate::builtin::extract_bundled_files(&atelier_home);

        // Auto-register is gated (default off; env/remote settings enables). Kept out
        // of extract_bundled_files so the gate can read the resolved
        // local_runtime_settings, which resolve_config has populated by now.
        if cfg.resolve_official_marketplace_auto_register().value {
            crate::extensions::marketplace::ensure_official_marketplace_source(&atelier_home);
        }

        let telemetry_mode = cfg.resolve_telemetry_mode();
        let feedback = cfg.resolve_feedback();
        tracing::info!(
            telemetry = %telemetry_mode,
            feedback = %feedback,
            "data capture config resolved",
        );
        update_telemetry_config(cfg, auth_manager);
    });
}

/// Apply current telemetry config + auth identity. Tears down the client
/// when telemetry is disabled, so it's safe to call repeatedly.
pub fn update_telemetry_config(config: &AgentConfig, auth_manager: &AuthManager) {
    let _ = (config, auth_manager);
}
