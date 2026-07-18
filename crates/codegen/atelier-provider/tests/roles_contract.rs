use atelier_provider::{ProviderRegistry, RoleConfig, RoleError, RoleId, RoleRegistry};
use serde_json::{Map, Value};
use tempfile::tempdir;

fn role_config(provider: &str, model: &str) -> RoleConfig {
    RoleConfig {
        provider: provider.into(),
        model: model.into(),
        effort: Some("high".into()),
        fast_mode: false,
        payload: Map::new(),
    }
}

#[test]
fn default_registry_contains_exactly_the_eight_fixed_roles() {
    let registry = RoleRegistry::default();

    assert_eq!(registry.len(), 8);
    for role_id in RoleId::ALL {
        let role = registry.find(role_id).expect("default role is present");
        role.validate().unwrap();
    }
    assert!(registry.find_by_name("custom").is_none());
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
        r#"{"alpha":true,"shared":"role","zeta":"provider"}"#
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
fn old_provider_registry_without_roles_gets_default_roles_and_persists_them() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    std::fs::write(&path, "schema_version = 1\n\n[providers]\n\n[models]\n").unwrap();

    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    assert_eq!(registry.roles().len(), 8);
    registry
        .update_role(RoleId::Main, role_config("proxy", "main-model"))
        .unwrap();
    registry.save().unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("[roles.main]"));
    let loaded = ProviderRegistry::load_or_create(&path).unwrap();
    assert_eq!(loaded.role(RoleId::Main).unwrap().model, "main-model");
    assert_eq!(loaded.roles().len(), 8);
}

#[test]
fn invalid_persisted_role_is_rejected() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    std::fs::write(
        &path,
        "schema_version = 1\n\n[roles.main]\nprovider = \"\"\nmodel = \"model\"\n",
    )
    .unwrap();

    assert!(ProviderRegistry::load_or_create(&path).is_err());
}
