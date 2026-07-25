use toml::Value as TomlValue;

/// Env override for the **auto** permission-mode feature gate.
pub(crate) const ENV_AUTO_PERMISSION_MODE: &str = "ATELIER_AUTO_PERMISSION_MODE";

/// Crate-wide serialization lock for tests that mutate
/// `ATELIER_AUTO_PERMISSION_MODE`. Every test reading the gate (here and in
/// `permissions.rs`, compiled into the same test binary) locks this so a
/// concurrent setter can't make them flaky.
#[cfg(test)]
pub(crate) static AUTO_PERMISSION_MODE_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Extract the `[auto_mode] enabled` gate from one TOML layer (the local opt-in
/// that replaced `[features] auto_permission_mode`).
fn auto_permission_mode_from_toml(v: Option<&TomlValue>) -> Option<bool> {
    v?.get("auto_mode")?.get("enabled")?.as_bool()
}

/// Pure precedence core for the auto-permission-mode gate, shared by the typed
/// resolver and the free-function disk reader so the two can't drift.
/// Precedence: requirement > env (`ATELIER_AUTO_PERMISSION_MODE`) > config >
/// managed > default (`true`).
fn resolve_auto_permission_mode_layers(
    requirement: Option<bool>,
    config: Option<bool>,
    managed: Option<bool>,
    feature_flag: Option<bool>,
) -> crate::agent::config::Resolved<bool> {
    use crate::agent::config::BoolFlag;
    BoolFlag::env(ENV_AUTO_PERMISSION_MODE)
        .requirement(requirement)
        .config(config)
        .managed(managed)
        .feature_flag(feature_flag)
        .default(true)
        .resolve()
}

/// Resolve whether the **auto** permission mode feature (`PermissionMode::Auto`,
/// the LLM/heuristic classifier) is enabled. Full chain mirroring
/// [`resolve_zdr_access_enabled`](super::resolve_zdr_access_enabled):
///
/// requirements > env (`ATELIER_AUTO_PERMISSION_MODE`) > `[auto_mode] enabled` in
/// `config.toml` > managed > default (`true`).
///
/// Default ON: Auto is offered unless a higher layer pins it off. Returns
/// [`Resolved`] so callers can log the winning source.
pub fn resolve_auto_permission_mode_enabled(
    requirements: Option<&TomlValue>,
    user: Option<&TomlValue>,
    managed: Option<&TomlValue>,
) -> crate::agent::config::Resolved<bool> {
    resolve_auto_permission_mode_layers(
        auto_permission_mode_from_toml(requirements),
        auto_permission_mode_from_toml(user),
        auto_permission_mode_from_toml(managed),
        None,
    )
}

/// Deserialize the `[auto_mode]` table from one effective-config TOML layer into
/// the typed [`AutoModeConfig`]. A malformed table is dropped to `None` (warned,
/// not silently swallowed, so a bad local `[auto_mode]` is visible in logs).
fn auto_mode_config_from_toml(
    v: Option<&TomlValue>,
) -> Option<crate::agent::config::AutoModeConfig> {
    let table = v?.get("auto_mode")?.clone();
    table
        .try_into()
        .map_err(|e| tracing::warn!(error = %e, "[auto_mode]: dropped malformed local table"))
        .ok()
}

/// Free-function form of [`resolve_auto_permission_mode_enabled`] for call
/// launch decision in `effective_auto_for_launch`, the agent's
/// `session_auto_mode` guard, and the pager mode cycle / settings. Reads env +
/// requirements + the effective `config.toml` from disk. Defaults `true` so
/// Auto is available unless pinned off.
pub fn auto_permission_mode_enabled_from_disk() -> bool {
    let requirements = crate::config::load_merged_requirements();
    let effective = crate::config::load_effective_config().ok();
    resolve_auto_permission_mode_layers(
        auto_permission_mode_from_toml(requirements.as_ref()),
        auto_permission_mode_from_toml(effective.as_ref()),
        None,
        None,
    )
    .value
}

/// Resolve the full Auto-mode config from the effective local configuration.
pub fn resolve_auto_mode_config_from_disk() -> crate::agent::config::AutoModeConfig {
    let effective = crate::config::load_effective_config().ok();
    auto_mode_config_from_toml(effective.as_ref()).unwrap_or_default()
}

/// Apply the built-in Auto-mode classifier defaults to a resolved config (these
/// take effect once auto mode is enabled): an unset `prompt_type` defaults to
/// `full` (v9-traffic eval: transcript context cuts the residual block rate
/// ~1/3 and lets explicit user authorization satisfy the prompt's
/// confirmation clause); an unset `reasoning_effort` defaults to `low` ONLY
/// when the effective model supports reasoning effort (else stays `None` —
/// provider default). Explicit local config values always win. Returns the
/// `(prompt_type, reasoning_effort)` the classifier wiring should use.
pub fn auto_mode_classifier_defaults(
    cfg: &crate::agent::config::AutoModeConfig,
    effective_supports_reasoning_effort: bool,
) -> (
    atelier_workspace::permission::ClassifierPromptType,
    Option<atelier_sampling_types::ReasoningEffort>,
) {
    let prompt_type = cfg
        .prompt_type
        .unwrap_or(atelier_workspace::permission::ClassifierPromptType::Full);
    let reasoning_effort = cfg.reasoning_effort.or_else(|| {
        effective_supports_reasoning_effort.then_some(atelier_sampling_types::ReasoningEffort::Low)
    });
    (prompt_type, reasoning_effort)
}

#[cfg(test)]
mod auto_permission_mode_gate_tests {
    use super::*;
    use crate::agent::config::ConfigSource;

    // `ATELIER_AUTO_PERMISSION_MODE` is process-global; serialize every test that
    // reads it (all of them, via `BoolFlag::env`) and force it unset at the top
    // of each so a developer's shell value can't make these flaky.
    fn guard() -> std::sync::MutexGuard<'static, ()> {
        let g = super::AUTO_PERMISSION_MODE_ENV_LOCK
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
        g
    }

    fn toml_features_auto(v: bool) -> TomlValue {
        toml::from_str(&format!("[auto_mode]\nenabled = {v}\n")).unwrap()
    }

    #[test]
    fn defaults_on_when_nothing_set() {
        let _g = guard();
        let r = resolve_auto_permission_mode_enabled(None, None, None);
        assert!(r.value, "gate must default ON");
        assert_eq!(r.source, ConfigSource::Default);
    }

    #[test]
    fn each_layer_can_turn_it_on() {
        let _g = guard();
        let on = toml_features_auto(true);
        // requirement
        let r = resolve_auto_permission_mode_enabled(Some(&on), None, None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Requirement);
        // config (user)
        let r = resolve_auto_permission_mode_enabled(None, Some(&on), None);
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::Config);
        // managed
        let r = resolve_auto_permission_mode_enabled(None, None, Some(&on));
        assert!(r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn precedence_config_beats_managed() {
        let _g = guard();
        let off = toml_features_auto(false);
        let on = toml_features_auto(true);
        // config(false) wins over managed(true).
        let r = resolve_auto_permission_mode_enabled(None, Some(&off), Some(&on));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::Config);
        let r = resolve_auto_permission_mode_enabled(None, None, Some(&off));
        assert!(!r.value);
        assert_eq!(r.source, ConfigSource::ManagedConfig);
    }

    #[test]
    fn env_overrides_config() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        let off = toml_features_auto(false);
        let r = resolve_auto_permission_mode_enabled(None, Some(&off), None);
        assert!(r.value, "env must override config");
        assert_eq!(r.source, ConfigSource::Env);
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
    }

    #[test]
    fn requirement_beats_env() {
        let _g = guard();
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        let off = toml_features_auto(false);
        let r = resolve_auto_permission_mode_enabled(Some(&off), None, None);
        assert!(!r.value, "requirement (managed/MDM floor) must beat env");
        assert_eq!(r.source, ConfigSource::Requirement);
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
    }

    #[test]
    fn disk_reader_honors_env() {
        let _g = guard();
        // The disk reader wires the env layer (highest deterministic source).
        unsafe { std::env::set_var(ENV_AUTO_PERMISSION_MODE, "1") };
        assert!(
            auto_permission_mode_enabled_from_disk(),
            "from_disk must honor the env layer"
        );
        unsafe { std::env::remove_var(ENV_AUTO_PERMISSION_MODE) };
    }

    #[test]
    fn auto_mode_classifier_defaults_apply_when_unset() {
        use crate::agent::config::AutoModeConfig;
        use atelier_sampling_types::ReasoningEffort;
        use atelier_workspace::permission::ClassifierPromptType;
        // Unset + RE-supporting effective model ⇒ full (transcript) + low.
        let (pt, eff) = auto_mode_classifier_defaults(&AutoModeConfig::default(), true);
        assert_eq!(pt, ClassifierPromptType::Full);
        assert_eq!(eff, Some(ReasoningEffort::Low));
        // Unset + non-RE model ⇒ full + None (no effort override).
        let (pt, eff) = auto_mode_classifier_defaults(&AutoModeConfig::default(), false);
        assert_eq!(pt, ClassifierPromptType::Full);
        assert_eq!(eff, None);
        // Explicit values win over the defaults, even on a RE-supporting model.
        let cfg = AutoModeConfig {
            prompt_type: Some(ClassifierPromptType::JustCommand),
            reasoning_effort: Some(ReasoningEffort::High),
            ..AutoModeConfig::default()
        };
        let (pt, eff) = auto_mode_classifier_defaults(&cfg, true);
        assert_eq!(pt, ClassifierPromptType::JustCommand);
        assert_eq!(eff, Some(ReasoningEffort::High));
    }

    #[test]
    fn auto_mode_config_from_toml_round_trips_and_warns_on_malformed() {
        use atelier_workspace::permission::ClassifierPromptType;
        // A real [auto_mode] table round-trips (not silently dropped).
        let toml: TomlValue = toml::from_str(
            "[auto_mode]\nenabled = true\nprompt_type = \"just_command\"\nclassifier_model = \"m\"\n",
        )
        .unwrap();
        let cfg = auto_mode_config_from_toml(Some(&toml)).expect("table parses");
        assert_eq!(cfg.enabled, Some(true));
        assert_eq!(cfg.prompt_type, Some(ClassifierPromptType::JustCommand));
        assert_eq!(cfg.classifier_model.as_deref(), Some("m"));
        // Absent [auto_mode] ⇒ None.
        let bare: TomlValue = toml::from_str("[features]\ngoal = true\n").unwrap();
        assert!(auto_mode_config_from_toml(Some(&bare)).is_none());
        // Malformed enum ⇒ dropped to None (warned), never a panic.
        let bad: TomlValue = toml::from_str("[auto_mode]\nprompt_type = \"bogus\"\n").unwrap();
        assert!(auto_mode_config_from_toml(Some(&bad)).is_none());
    }
}
