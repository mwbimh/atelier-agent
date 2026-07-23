//! Local compatibility helpers for deployment metadata.
//!
//! Atelier does not contain Grok Build's vendor-managed configuration control
//! plane. Local `managed_config.toml` and `requirements.toml` files are loaded
//! by the normal configuration layers; this module never downloads, rewrites,
//! or deletes them.

/// Legacy logout hook retained for callers that also clear credentials.
///
/// A locally supplied policy belongs to the user or administrator, so logout
/// must never remove it.
pub fn clear_orphan() {}

/// Deployment id attached to explicitly configured provider requests.
pub fn resolve_deployment_id(deployment_key: Option<&str>) -> Option<String> {
    let key = deployment_key.filter(|key| !key.is_empty())?;
    crate::config::managed_deployment_id(&deployment_key_fingerprint(key))
        .or_else(|| Some(crate::agent::config::deployment_id_from_key(key)))
}

/// Resolve a deployment key from `ATELIER_DEPLOYMENT_KEY`, then local config.
pub fn resolve_deployment_key() -> Option<String> {
    let config_value = crate::config::load_effective_config()
        .map_err(|error| tracing::warn!("failed to load deployment key: {error}"))
        .ok()
        .and_then(|root| {
            root.get("endpoints")?
                .get("deployment_key")?
                .as_str()
                .map(str::to_owned)
        });

    crate::agent::config::resolve_string_flag(
        None,
        "ATELIER_DEPLOYMENT_KEY",
        config_value.as_deref(),
        None,
    )
    .map(|resolved| resolved.value)
}

fn deployment_key_fingerprint(key: &str) -> String {
    blake3::hash(key.as_bytes()).to_hex().to_string()
}

/// Local policy files are enforced by the configuration loader itself.
/// There is no remote cache whose presence needs a vendor identity gate.
pub fn managed_policy_gate() -> Result<(), String> {
    Ok(())
}
