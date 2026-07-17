use super::mcp::*;
use toml::Value as TomlValue;
pub fn load_relay_sync_enabled_sync() -> bool {
    // Relay sync is a vendor session-sharing path.  It must not be
    // re-enabled by a local config file or environment variable.
    false
}
/// `[harness]` blocking-upload settings from ONE effective-config parse:
/// `block_for_upload` (default false — prompt handling waits for turn-end
/// uploads when set) and `upload_flush_timeout_secs` (default 60 — budget for
/// that wait).
pub fn load_blocking_upload_config_sync() -> (bool, std::time::Duration) {
    const DEFAULT_FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    let root: TomlValue = match crate::config::load_effective_config() {
        Ok(r) => r,
        Err(_) => return (false, DEFAULT_FLUSH_TIMEOUT),
    };
    let harness = match &root {
        TomlValue::Table(table) => table.get("harness"),
        _ => None,
    };
    let block_for_upload = harness
        .and_then(|h| h.get("block_for_upload"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let flush_timeout = harness
        .and_then(|h| h.get("upload_flush_timeout_secs"))
        .and_then(|v| v.as_integer())
        .and_then(|v| u64::try_from(v).ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(DEFAULT_FLUSH_TIMEOUT);
    (block_for_upload, flush_timeout)
}
pub async fn load_config() -> Config {
    let root: TomlValue = match crate::config::load_effective_config() {
        Ok(v) => v,
        Err(_) => return Config::default(),
    };
    load_config_from_toml(&root)
}
/// Parse `Config` from a pre-loaded TOML value. Used by both async and sync paths.
pub fn load_config_from_toml(root: &TomlValue) -> Config {
    let table = match root.as_table() {
        Some(t) => t,
        None => return Config::default(),
    };
    fn section<T: serde::de::DeserializeOwned + Default>(
        table: &toml::map::Map<String, TomlValue>,
        key: &str,
    ) -> T {
        table
            .get(key)
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default()
    }
    if let Some(TomlValue::Table(toolset)) = table.get("toolset")
        && toolset.get("use_concise").is_some()
    {
        tracing::warn!(
            "`[toolset] use_concise` is deprecated and no longer has any effect. \
             Set `use_concise = true` on individual model entries in config.toml instead."
        );
    }
    let management_api_key = table
        .get("endpoints")
        .and_then(|v| v.get("management_api_key"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let permission = table
        .get("permission")
        .and_then(|v| v.clone().try_into::<PermissionConfig>().ok());
    Config {
        cli: section(table, "cli"),
        models: section(table, "models"),
        ui: section(table, "ui"),
        harness: section(table, "harness"),
        skills: section(table, "skills"),
        compat: section(table, "compat"),
        management_api_key,
        permission,
        diagnostics: section(table, "diagnostics"),
        session: section(table, "session"),
        ask_user_question: table
            .get("toolset")
            .and_then(|t| t.get("ask_user_question"))
            .and_then(|v| v.clone().try_into().ok())
            .unwrap_or_default(),
    }
}
/// Resolve permission config with project override semantics.
///
/// Priority (per approved plan):
/// 1. Nearest project `.atelier/config.toml` with `[permission]` section (from cwd upward)
/// 2. Global `~/.atelier/config.toml` `[permission]` section
///
/// Project `[permission]` overrides global wholesale (no deep merge).
///
/// Returns `(config, source_path)` from the highest-priority config file
/// that contains a `[permission]` section.
pub async fn resolve_permission_config(
    cwd: &std::path::Path,
) -> Option<(PermissionConfig, std::path::PathBuf)> {
    let project_configs = crate::config::find_project_configs(cwd);
    for config_path in project_configs.into_iter().rev() {
        if let Ok(root) = crate::config::load_config_file(&config_path)
            && let Some(perm_val) = root.get("permission")
        {
            match perm_val.clone().try_into::<PermissionConfig>() {
                Ok(perm_config) => {
                    tracing::info!("Loaded [permission] from project");
                    return Some((perm_config, config_path));
                }
                Err(e) => tracing::warn!(error = % e, "Failed to parse [permission]"),
            }
        }
    }
    let global_path = user_config_path();
    load_config().await.permission.map(|cfg| (cfg, global_path))
}
#[cfg(test)]
mod tests {
    use super::*;
    use toml::Value as TomlValue;
    #[test]
    fn test_models_default_parsing() {
        let toml_str = r#"
[models]
default = "atelier-code-fast-1"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(models)) = table.get("models")
        {
            let default = models
                .get("default")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            assert_eq!(default.as_deref(), Some("atelier-code-fast-1"));
        } else {
            panic!("Expected models table");
        }
    }
    #[test]
    fn test_remote_secret_parsing() {
        let toml_str = r#"
[remote]
secret = "my-secret-token"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(remote)) = table.get("remote")
        {
            let secret = remote
                .get("secret")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            assert_eq!(secret, Some("my-secret-token".to_string()));
        } else {
            panic!("Expected remote table");
        }
    }
    #[test]
    fn test_remote_secret_empty_section() {
        let toml_str = r#"
[remote]
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(remote)) = table.get("remote")
        {
            let secret = remote
                .get("secret")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            assert!(secret.is_none());
        } else {
            panic!("Expected remote table");
        }
    }
    #[test]
    fn test_remote_secret_no_section() {
        let toml_str = r#"
[models]
default = "atelier-code-fast-1"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root {
            let has_remote = table.get("remote").is_some();
            assert!(!has_remote);
        }
    }
    #[test]
    fn test_relay_sync_enabled_true() {
        let toml_str = r#"
[relay]
enabled = true
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(relay)) = table.get("relay")
        {
            let enabled = relay
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            assert!(enabled);
        } else {
            panic!("Expected relay table");
        }
    }
    #[test]
    fn test_relay_sync_enabled_false() {
        let toml_str = r#"
[relay]
enabled = false
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(relay)) = table.get("relay")
        {
            let enabled = relay
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            assert!(!enabled);
        } else {
            panic!("Expected relay table");
        }
    }
    #[test]
    fn test_relay_sync_default_false() {
        let toml_str = r#"
[relay]
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root
            && let Some(TomlValue::Table(relay)) = table.get("relay")
        {
            let enabled = relay
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            assert!(!enabled);
        }
    }
    #[test]
    fn test_relay_sync_no_section() {
        let toml_str = r#"
[models]
default = "atelier-code-fast-1"
"#;
        let root: TomlValue = toml::from_str(toml_str).unwrap();
        if let TomlValue::Table(table) = root {
            let has_relay = table.get("relay").is_some();
            assert!(!has_relay);
        }
    }
    #[test]
    fn test_relay_sync_config_struct() {
        let config = RelaySyncConfig {
            enabled: Some(true),
        };
        assert_eq!(config.enabled, Some(true));
        let config_disabled = RelaySyncConfig {
            enabled: Some(false),
        };
        assert_eq!(config_disabled.enabled, Some(false));
        let config_default = RelaySyncConfig::default();
        assert_eq!(config_default.enabled, None);
    }

    #[test]
    fn relay_sync_is_permanently_disabled() {
        // This guard intentionally does not consult process configuration:
        // remote session synchronization is not part of Atelier.
        assert!(!load_relay_sync_enabled_sync());
    }
}
