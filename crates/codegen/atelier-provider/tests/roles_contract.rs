use atelier_provider::{ProviderRegistry, RoleConfig, RoleError, RoleId, RoleRegistry};
use serde_json::{Map, Value, json};
use tempfile::tempdir;

fn role_config(provider: &str, model: &str) -> RoleConfig {
    let mut config = RoleConfig::new(provider, model).unwrap();
    config.effort = Some("high".into());
    config
}

#[test]
fn default_registry_contains_no_placeholder_role_assignments() {
    let registry = RoleRegistry::default();

    assert!(registry.is_empty());
    for role_id in RoleId::ALL {
        assert!(registry.find(role_id).is_none());
    }
    assert!(registry.find_by_name("custom").is_none());
}

#[test]
fn fixed_roles_include_general_subagent_with_stable_parentage() {
    assert_eq!(RoleId::ALL.len(), 12);
    assert_eq!("general".parse::<RoleId>().unwrap(), RoleId::General);
    assert_eq!(RoleId::General.settings_parent(), Some(RoleId::Main));
    for role in [
        RoleId::Explore,
        RoleId::Implement,
        RoleId::Review,
        RoleId::Test,
    ] {
        assert_eq!(role.settings_parent(), Some(RoleId::General));
    }
    for role in [
        RoleId::Compact,
        RoleId::Summary,
        RoleId::Title,
        RoleId::Planner,
        RoleId::Strategist,
        RoleId::Skeptic,
    ] {
        assert_eq!(role.settings_parent(), Some(RoleId::Main));
    }
}

#[test]
fn context_ancestry_never_inherits_main_for_non_main_roles() {
    assert_eq!(RoleId::Main.context_ancestry(), &[RoleId::Main]);
    assert_eq!(RoleId::General.context_ancestry(), &[RoleId::General]);
    assert_eq!(
        RoleId::Review.context_ancestry(),
        &[RoleId::Review, RoleId::General]
    );
    assert_eq!(RoleId::Compact.context_ancestry(), &[RoleId::Compact]);
    for role in RoleId::ALL.into_iter().filter(|role| *role != RoleId::Main) {
        assert!(!role.context_ancestry().contains(&RoleId::Main));
    }
}

#[test]
fn goal_roles_have_stable_public_names() {
    for (name, role) in [
        ("planner", RoleId::Planner),
        ("strategist", RoleId::Strategist),
        ("skeptic", RoleId::Skeptic),
    ] {
        assert_eq!(name.parse::<RoleId>().unwrap(), role);
        assert_eq!(role.as_str(), name);
        assert_eq!(role.to_string(), name);
    }
}

#[test]
fn role_config_rejects_empty_provider_and_model() {
    let mut config = role_config("provider", "model");
    config.provider = "  ".into();
    assert_eq!(config.validate(), Err(RoleError::EmptyProvider));

    config.provider = "provider".into();
    config.model.clear();
    assert_eq!(config.validate(), Err(RoleError::EmptyModel));
}

#[test]
fn role_config_rejects_unknown_reasoning_effort() {
    let mut config = role_config("provider", "model");
    config.effort = Some("nonsense".into());

    assert_eq!(
        config.validate(),
        Err(RoleError::InvalidEffort("nonsense".into()))
    );
}

#[test]
fn role_config_accepts_existing_reasoning_effort_values() {
    for effort in [
        "none", "minimal", "low", "medium", "high", "xhigh", "max", "HIGH",
    ] {
        let mut config = role_config("provider", "model");
        config.effort = Some(effort.into());

        assert!(config.validate().is_ok(), "effort {effort} must be valid");
    }
}

#[test]
fn role_config_rejects_credential_like_payload_keys_recursively() {
    let mut config = role_config("provider", "model");
    config.payload = serde_json::from_value(serde_json::json!({
        "request": {
            "authorization": "Bearer secret"
        }
    }))
    .unwrap();

    assert_eq!(
        config.validate(),
        Err(RoleError::SensitivePayloadKey("authorization".into()))
    );
}

#[test]
fn role_config_allows_non_credential_token_parameters() {
    let mut config = role_config("provider", "model");
    config
        .payload
        .insert("max_output_tokens".into(), 32000.into());

    assert!(config.validate().is_ok());
}

#[test]
fn role_debug_redacts_payload_values() {
    let mut config = role_config("provider", "model");
    config
        .payload
        .insert("api_key".into(), Value::String("role-secret-value".into()));

    let debug = format!("{config:?}");

    assert!(!debug.contains("role-secret-value"));
    assert!(debug.contains("REDACTED"));
}

#[test]
fn sparse_role_settings_resolve_each_field_through_the_fixed_parent_chain() {
    let registry: RoleRegistry = toml::from_str(
        r#"
        [general]
        provider = "proxy"
        model = "general-model"
        effort = "low"
        fast_mode = true

        [explore]
        model = "explore-model"

        [review]
        effort = "high"
        fast_mode = false
        [review.payload]
        temperature = 0.1
        "#,
    )
    .unwrap();
    let main = RoleConfig::new("main-provider", "main-model").unwrap();

    let (_, explore) = registry.resolve_inherited(RoleId::Explore, &main);
    assert_eq!(explore.provider, "proxy");
    assert_eq!(explore.model, "explore-model");
    assert_eq!(explore.effort.as_deref(), Some("low"));
    assert!(explore.fast_mode);

    let (_, review) = registry.resolve_inherited(RoleId::Review, &main);
    assert_eq!(review.provider, "proxy");
    assert_eq!(review.model, "general-model");
    assert_eq!(review.effort.as_deref(), Some("high"));
    assert!(!review.fast_mode);
    assert_eq!(review.payload["temperature"], 0.1);
}

#[test]
fn sparse_role_resolution_reports_per_field_sources() {
    let main = RoleConfig::new("main-provider", "main-model").unwrap();
    let general: RoleConfig = toml::from_str(
        r#"
        provider = "general-provider"
        effort = "low"
        [payload]
        shared = "general"
        parent = true
        "#,
    )
    .unwrap();
    let review: RoleConfig = toml::from_str(
        r#"
        model = "review-model"
        fast_mode = true
        [payload]
        shared = "review"
        "#,
    )
    .unwrap();
    let mut registry = RoleRegistry::new();
    registry.update(RoleId::General, general).unwrap();
    registry.update(RoleId::Review, review).unwrap();

    let resolved = registry.resolve_inherited_details(RoleId::Review, &main);

    assert_eq!(resolved.source, RoleId::Review);
    assert_eq!(resolved.field_sources.provider, RoleId::General);
    assert_eq!(resolved.field_sources.model, RoleId::Review);
    assert_eq!(resolved.field_sources.effort, Some(RoleId::General));
    assert_eq!(resolved.field_sources.fast_mode, Some(RoleId::Review));
    assert_eq!(resolved.field_sources.payload["parent"], RoleId::General);
    assert_eq!(resolved.field_sources.payload["shared"], RoleId::Review);
}

#[test]
fn sparse_role_patch_preserves_unspecified_exact_overrides() {
    let mut existing = RoleConfig::new("provider", "model-a").unwrap();
    existing.effort = Some("low".into());
    existing.set_fast_mode(true);
    existing.payload.insert("kept".into(), json!(true));
    let patch: RoleConfig = toml::from_str("effort = \"high\"").unwrap();

    let merged = existing.patched_with(&patch, true);

    assert_eq!(merged.provider_override(), Some("provider"));
    assert_eq!(merged.model_override(), Some("model-a"));
    assert_eq!(merged.effort.as_deref(), Some("high"));
    assert_eq!(merged.fast_mode_override(), Some(true));
    assert_eq!(merged.payload["kept"], json!(true));
}

#[test]
fn sparse_role_patch_can_replace_the_exact_payload() {
    let mut existing = RoleConfig::new("provider", "model-a").unwrap();
    existing.payload.insert("old".into(), json!(true));
    let patch: RoleConfig = toml::from_str(
        r#"
        effort = "high"
        [payload]
        new = true
        "#,
    )
    .unwrap();

    let merged = existing.patched_with(&patch, false);

    assert!(!merged.payload.contains_key("old"));
    assert_eq!(merged.payload["new"], json!(true));
}

#[test]
fn sparse_role_serialization_preserves_omitted_fast_mode() {
    let registry: RoleRegistry = toml::from_str(
        r#"
        [explore]
        effort = "medium"
        "#,
    )
    .unwrap();

    let serialized = toml::to_string(&registry).unwrap();

    assert!(serialized.contains("effort = \"medium\""));
    assert!(!serialized.contains("fast_mode"));
    assert!(!serialized.contains("provider"));
    assert!(!serialized.contains("model"));
}

#[test]
fn main_role_cannot_be_persisted_in_role_registry() {
    let mut registry = RoleRegistry::default();
    assert_eq!(
        registry.update(RoleId::Main, role_config("provider", "model")),
        Err(RoleError::MainManagedByConfig)
    );
}

#[test]
fn role_registry_supports_update_delete_and_find() {
    let mut registry = RoleRegistry::default();
    let updated = role_config("proxy", "review-model");

    registry.update(RoleId::Review, updated.clone()).unwrap();
    assert_eq!(registry.find(RoleId::Review), Some(&updated));

    assert_eq!(registry.delete(RoleId::Review), Some(updated));
    assert!(registry.find(RoleId::Review).is_none());
}

#[test]
fn role_payload_overrides_provider_defaults_deterministically() {
    let mut provider_payload = Map::new();
    provider_payload.insert("zeta".into(), Value::String("provider".into()));
    provider_payload.insert("shared".into(), Value::String("provider".into()));

    let mut config = role_config("provider", "model");
    config.payload.insert("alpha".into(), Value::Bool(true));
    config
        .payload
        .insert("shared".into(), Value::String("role".into()));

    let merged = config.merged_payload(&provider_payload);

    assert_eq!(merged.get("zeta"), Some(&Value::String("provider".into())));
    assert_eq!(merged.get("shared"), Some(&Value::String("role".into())));
    assert_eq!(merged.get("alpha"), Some(&Value::Bool(true)));
    assert_eq!(
        serde_json::to_string(&merged).unwrap(),
        r#"{"alpha":true,"fast_mode":false,"shared":"role","zeta":"provider"}"#
    );
}

#[test]
fn fast_mode_is_encoded_in_the_effective_role_payload() {
    let mut config = role_config("provider", "model");
    config.fast_mode = true;
    config
        .payload
        .insert("temperature".into(), Value::from(0.2));

    let payload = config.effective_payload();

    assert_eq!(payload.get("fast_mode"), Some(&Value::Bool(true)));
    assert_eq!(payload.get("temperature"), Some(&Value::from(0.2)));
}

#[test]
fn merged_payload_includes_fast_mode() {
    let mut config = role_config("provider", "model");
    config.fast_mode = true;

    let merged = config.merged_payload(&Map::new());

    assert_eq!(merged.get("fast_mode"), Some(&Value::Bool(true)));
}

#[test]
fn role_fast_mode_false_overrides_provider_default_true() {
    let config = role_config("provider", "model");
    let provider_defaults = serde_json::from_value(serde_json::json!({
        "fast_mode": true,
        "temperature": 0.8,
    }))
    .unwrap();

    let merged = config.merged_payload(&provider_defaults);

    assert_eq!(merged.get("fast_mode"), Some(&Value::Bool(false)));
    assert_eq!(merged.get("temperature"), Some(&Value::from(0.8)));
}

#[test]
fn every_persisted_role_assignment_is_configured() {
    let mut registry = RoleRegistry::default();
    registry
        .update(RoleId::General, role_config("default", "default"))
        .unwrap();

    assert!(registry.find(RoleId::General).unwrap().is_configured());
    assert!(role_config("provider", "model").is_configured());
}

#[test]
fn role_config_and_registry_round_trip_through_toml() {
    let mut registry = RoleRegistry::default();
    registry
        .update(RoleId::Title, role_config("proxy", "title-model"))
        .unwrap();

    let encoded = toml::to_string(&registry).unwrap();
    let decoded: RoleRegistry = toml::from_str(&encoded).unwrap();

    assert_eq!(decoded, registry);
}

#[test]
fn missing_roles_file_stays_empty_until_a_role_is_configured() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    std::fs::write(&path, "schema_version = 3\n\n[providers]\n").unwrap();

    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    assert!(registry.roles().is_empty());
    registry
        .update_role(RoleId::General, role_config("proxy", "general-model"))
        .unwrap();
    registry.save().unwrap();

    let text = std::fs::read_to_string(directory.path().join("roles.toml")).unwrap();
    assert!(text.contains("[roles.general]"));
    assert!(!text.contains("[roles.main]"));
    let loaded = ProviderRegistry::load_or_create(&path).unwrap();
    assert_eq!(loaded.role(RoleId::General).unwrap().model, "general-model");
    assert_eq!(loaded.roles().len(), 1);
    assert!(!text.contains("provider = \"default\""));
    assert!(!text.contains("model = \"default\""));
}

#[test]
fn invalid_persisted_role_is_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    std::fs::write(&path, "schema_version = 3\n\n[providers]\n").unwrap();
    std::fs::write(
        directory.path().join("roles.toml"),
        "schema_version = 1\n\n[roles.general]\nprovider = \"\"\nmodel = \"model\"\n",
    )
    .unwrap();

    assert!(ProviderRegistry::load_or_create(&path).is_err());
}
