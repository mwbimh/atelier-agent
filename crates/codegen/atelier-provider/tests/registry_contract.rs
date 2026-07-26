use atelier_provider::{
    CapabilityOverrides, CredentialRef, ModelCapabilities, ModelDescriptor, ModelKey, ModelSource,
    ProviderAuth, ProviderConfig, ProviderDiscovery, ProviderModelOverride, ProviderRegistry,
    SecretString, WireApi, WireApiSource, parse_custom_model_id, parse_openai_models_response,
};
#[cfg(not(windows))]
use atelier_provider::{CredentialError, CredentialErrorCode};
use std::collections::BTreeMap;
use tempfile::tempdir;
use url::Url;

fn provider(id: &str) -> ProviderConfig {
    ProviderConfig {
        id: id.into(),
        display_name: id.into(),
        auth: ProviderAuth::Bearer,
        base_url: Url::parse("https://models.example.test/v1").unwrap(),
        credential: CredentialRef::Environment {
            variable: "ATELIER_TEST_KEY".into(),
        },
        discovery: ProviderDiscovery::Static,
        extra_headers: BTreeMap::new(),
        enabled: true,
    }
}

fn model(key: ModelKey) -> ModelDescriptor {
    ModelDescriptor {
        key,
        display_name: "Shared model".into(),
        description: None,
        wire_api: None,
        context_window: None,
        capabilities: ModelCapabilities::default(),
        reasoning_efforts: Vec::new(),
        default_effort: None,
        fast_mode: false,
        source: ModelSource::Static,
        enabled: true,
    }
}

#[test]
fn model_keys_are_provider_scoped() {
    let left = ModelKey::new("left", "same-model").unwrap();
    let right = ModelKey::new("right", "same-model").unwrap();
    assert_ne!(left, right);
    assert_eq!(left.to_string(), "left/same-model");
    assert_eq!(right.to_string(), "right/same-model");
}

#[test]
fn absent_wire_api_fails_closed_independently_of_provider_auth() {
    for (provider_id, auth) in [
        ("bearer", ProviderAuth::Bearer),
        (
            "header",
            ProviderAuth::Header {
                name: "x-api-key".into(),
            },
        ),
    ] {
        let key = ModelKey::new(provider_id, "model").unwrap();
        let mut registry = ProviderRegistry::in_memory();
        let mut config = provider(provider_id);
        config.auth = auth;
        registry.upsert_provider(config).unwrap();
        registry.upsert_model(model(key.clone())).unwrap();

        let error = registry.resolve_wire_api(&key).unwrap_err();
        let snapshot_error = registry.snapshot().resolve_wire_api(&key).unwrap_err();

        assert!(error.to_string().contains("wire API is not configured"));
        assert_eq!(snapshot_error.to_string(), error.to_string());
    }
}

#[test]
fn discovery_refresh_removes_only_missing_remote_models() {
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    let remote = |id: &str| ModelDescriptor {
        source: ModelSource::Remote,
        ..model(ModelKey::new("proxy", id).unwrap())
    };
    registry
        .merge_discovered_models("proxy", vec![remote("a"), remote("b")])
        .unwrap();
    registry
        .upsert_model(model(ModelKey::new("proxy", "static").unwrap()))
        .unwrap();

    registry
        .merge_discovered_models("proxy", vec![remote("a")])
        .unwrap();

    assert!(
        registry
            .model(&ModelKey::new("proxy", "a").unwrap())
            .is_some()
    );
    assert!(
        registry
            .model(&ModelKey::new("proxy", "b").unwrap())
            .is_none()
    );
    assert!(
        registry
            .model(&ModelKey::new("proxy", "static").unwrap())
            .is_some()
    );
}

#[test]
fn unknown_model_capabilities_are_disabled() {
    let capabilities = ModelCapabilities::default();
    assert!(!capabilities.image_generation);
    assert!(!capabilities.web_search);
    assert!(!capabilities.tool_calls);
    assert!(!capabilities.image_input);
    assert!(!capabilities.reasoning_effort);
}

#[test]
fn credential_ref_resolves_environment_without_persisting_value() {
    let variable = if cfg!(windows) { "Path" } else { "PATH" };
    let expected = std::env::var(variable).expect("test environment should define PATH");
    let resolved = CredentialRef::Environment {
        variable: variable.into(),
    }
    .resolve()
    .unwrap()
    .unwrap();

    assert_eq!(resolved.expose_secret(), expected);
    assert_eq!(format!("{resolved:?}"), "SecretString(REDACTED)");
}

#[test]
fn credential_ref_without_secret_is_available_for_public_providers() {
    assert!(CredentialRef::None.is_available());
}

#[test]
fn credential_ref_resolves_explicit_command_output() {
    let reference = if cfg!(windows) {
        CredentialRef::Command {
            program: "cmd".into(),
            args: vec!["/C".into(), "echo command-secret".into()],
        }
    } else {
        CredentialRef::Command {
            program: "printf".into(),
            args: vec!["command-secret\n".into()],
        }
    };

    let resolved = reference.resolve().unwrap().unwrap();
    assert_eq!(resolved.expose_secret(), "command-secret");
}

#[cfg(not(windows))]
#[test]
fn credential_ref_secret_store_is_explicitly_unsupported_outside_windows() {
    let error = CredentialRef::SecretStore {
        service: "atelier-test".into(),
        account: "test-account".into(),
    }
    .resolve()
    .unwrap_err();

    assert_eq!(error.code(), CredentialErrorCode::SecretStoreUnsupported);
    assert!(matches!(
        error,
        CredentialError::SecretStoreUnsupported { service, account }
            if service == "atelier-test" && account == "test-account"
    ));

    let reference = CredentialRef::SecretStore {
        service: "atelier-test".into(),
        account: "test-account".into(),
    };
    assert_eq!(
        reference
            .set_secret(SecretString::from("secret-value"))
            .unwrap_err()
            .code(),
        CredentialErrorCode::SecretStoreUnsupported
    );
    assert_eq!(
        reference.delete_secret().unwrap_err().code(),
        CredentialErrorCode::SecretStoreUnsupported
    );
}

#[cfg(windows)]
#[test]
fn credential_ref_secret_store_round_trips_through_windows_credential_manager() {
    let reference = CredentialRef::SecretStore {
        service: format!("atelier-provider-test-{}", std::process::id()),
        account: "test-account".into(),
    };
    let _cleanup = CredentialCleanup(reference.clone());
    let secret = SecretString::from("windows-secret-value");

    let _ = reference.delete_secret();
    reference.set_secret(secret).unwrap();

    let resolved = reference.resolve().unwrap().unwrap();
    assert_eq!(resolved.expose_secret(), "windows-secret-value");
    assert_eq!(format!("{resolved:?}"), "SecretString(REDACTED)");

    reference.delete_secret().unwrap();
    assert!(reference.resolve().is_err());
}

#[cfg(windows)]
struct CredentialCleanup(CredentialRef);

#[cfg(windows)]
impl Drop for CredentialCleanup {
    fn drop(&mut self) {
        let _ = self.0.delete_secret();
    }
}

#[test]
fn model_key_parser_preserves_custom_model_id_slashes() {
    let key = ModelKey::parse("proxy/custom/model").unwrap();
    assert_eq!(key.provider_id, "proxy");
    assert_eq!(key.model_id, "custom/model");
    assert_eq!(parse_custom_model_id("proxy", "custom/model").unwrap(), key);
    assert!(ModelKey::parse("unscoped-model").is_err());
}

#[test]
fn openai_models_response_parser_is_pure_and_fail_closed() {
    let response = r#"
        {
          "object": "list",
          "data": [
            {
              "id": "custom/model",
              "name": "Custom Model",
              "description": "A configured model",
              "wire_api": "messages",
              "context_window": 131072,
              "capabilities": {
                "text_input": true,
                "tool_calls": true,
                "image_input": true
              }
            },
            {
              "id": "unknown-capabilities",
              "owned_by": "vendor"
            }
          ]
        }
    "#;

    let parsed = parse_openai_models_response("proxy", response).unwrap();
    assert_eq!(parsed.len(), 2);
    assert_eq!(
        parsed[0].key,
        ModelKey::new("proxy", "custom/model").unwrap()
    );
    assert_eq!(parsed[0].display_name, "Custom Model");
    assert_eq!(parsed[0].wire_api, Some(WireApi::Messages));
    assert_eq!(parsed[0].context_window, Some(131072));
    assert!(parsed[0].capabilities.tool_calls);
    assert!(parsed[0].capabilities.image_input);
    assert!(!parsed[0].capabilities.web_search);
    assert_eq!(parsed[0].source, ModelSource::Remote);

    assert_eq!(parsed[1].key.model_id, "unknown-capabilities");
    assert_eq!(parsed[1].capabilities, ModelCapabilities::default());

    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    assert!(registry.models().next().is_none());
    registry.merge_discovered_models("proxy", parsed).unwrap();
    assert!(
        registry
            .model(&ModelKey::new("proxy", "custom/model").unwrap())
            .is_some()
    );

    let error = parse_openai_models_response(
        "proxy",
        r#"{"data":[{"id":"bad-wire","wire_api":"guessed"}]}"#,
    )
    .expect_err("unknown discovery wire APIs must fail closed");
    assert!(error.to_string().contains("unknown wire API"), "{error}");
}

#[test]
fn user_capability_overrides_win_over_remote_metadata() {
    let key = ModelKey::new("proxy", "same-model").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry.upsert_model(model(key.clone())).unwrap();

    let remote = ModelDescriptor {
        key: key.clone(),
        capabilities: ModelCapabilities {
            tool_calls: true,
            image_generation: true,
            ..ModelCapabilities::default()
        },
        source: ModelSource::Remote,
        ..model(key.clone())
    };
    registry
        .merge_discovered_models("proxy", vec![remote])
        .unwrap();
    registry
        .set_capability_overrides(
            &key,
            CapabilityOverrides {
                image_generation: Some(false),
                web_search: Some(true),
                ..CapabilityOverrides::default()
            },
        )
        .unwrap();

    let refreshed = ModelDescriptor {
        key: key.clone(),
        capabilities: ModelCapabilities {
            tool_calls: true,
            image_generation: true,
            web_search: false,
            ..ModelCapabilities::default()
        },
        source: ModelSource::Remote,
        ..model(key.clone())
    };
    registry
        .merge_discovered_models("proxy", vec![refreshed])
        .unwrap();

    let selected = registry.model(&key).unwrap();
    assert!(selected.capabilities.tool_calls);
    assert!(!selected.capabilities.image_generation);
    assert!(selected.capabilities.web_search);
}

#[test]
fn clearing_capability_override_restores_latest_remote_metadata() {
    let key = ModelKey::new("proxy", "same-model").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry
        .merge_discovered_models(
            "proxy",
            vec![ModelDescriptor {
                key: key.clone(),
                display_name: "Shared model".into(),
                description: None,
                wire_api: None,
                context_window: None,
                capabilities: ModelCapabilities {
                    tool_calls: true,
                    ..ModelCapabilities::default()
                },
                reasoning_efforts: Vec::new(),
                default_effort: None,
                fast_mode: false,
                source: ModelSource::Remote,
                enabled: true,
            }],
        )
        .unwrap();
    registry
        .set_capability_overrides(
            &key,
            CapabilityOverrides {
                tool_calls: Some(false),
                ..CapabilityOverrides::default()
            },
        )
        .unwrap();
    assert!(!registry.model(&key).unwrap().capabilities.tool_calls);

    registry
        .set_capability_overrides(&key, CapabilityOverrides::default())
        .unwrap();
    assert!(registry.model(&key).unwrap().capabilities.tool_calls);
}

#[test]
fn invalid_discovered_batch_does_not_partially_update_registry() {
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    let before = registry.snapshot();

    let valid = model(ModelKey::new("proxy", "valid").unwrap());
    let wrong_provider = model(ModelKey::new("other", "invalid").unwrap());
    assert!(
        registry
            .merge_discovered_models("proxy", vec![valid, wrong_provider])
            .is_err()
    );
    assert_eq!(registry.snapshot(), before);
}

#[test]
fn provider_snapshot_has_no_default_model_field() {
    let mut registry = ProviderRegistry::in_memory();
    let first = ModelKey::new("first", "same-model").unwrap();
    registry.upsert_provider(provider("first")).unwrap();
    registry.upsert_model(model(first.clone())).unwrap();
    let value = serde_json::to_value(registry.snapshot()).unwrap();
    assert!(value.get("default_model").is_none(), "{value}");
    assert!(value.get("defaultModel").is_none(), "{value}");
}

#[test]
fn enabled_provider_models_exclude_disabled_providers() {
    let mut registry = ProviderRegistry::in_memory();
    let enabled_key = ModelKey::new("enabled", "model").unwrap();
    let disabled_key = ModelKey::new("disabled", "model").unwrap();
    registry.upsert_provider(provider("enabled")).unwrap();
    registry.upsert_provider(provider("disabled")).unwrap();
    registry.set_provider_enabled("disabled", false).unwrap();
    registry.upsert_model(model(enabled_key.clone())).unwrap();
    registry.upsert_model(model(disabled_key)).unwrap();

    let models = registry.enabled_provider_models();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].0.id, "enabled");
    assert_eq!(models[0].1.key, enabled_key);
}

#[test]
fn registry_reports_enabled_provider_without_models() {
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    assert!(registry.has_enabled_providers());
    assert!(registry.enabled_provider_models().is_empty());
}

#[test]
fn persistence_does_not_write_secret_values() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("providers.toml");
    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry.save().unwrap();

    let text = std::fs::read_to_string(path).unwrap();
    assert!(text.contains("ATELIER_TEST_KEY"));
    assert!(!text.contains("secret-value"));
    assert!(!text.contains("apiKey"));
}

#[test]
fn provider_validation_rejects_non_http_url_schemes() {
    let mut config = provider("local");
    config.base_url = Url::parse("file:///tmp/provider").unwrap();

    let error = config.validate().unwrap_err();

    assert!(error.to_string().contains("unsupported base URL scheme"));
}

#[test]
fn static_provider_can_register_models_without_discovery_endpoint() {
    let mut registry = ProviderRegistry::in_memory();
    let mut config = provider("static");
    config.discovery = ProviderDiscovery::Static;
    registry.upsert_provider(config).unwrap();

    let key = ModelKey::new("static", "local-coder").unwrap();
    registry.upsert_model(model(key.clone())).unwrap();

    assert_eq!(registry.model(&key).unwrap().key, key);
}

#[test]
fn save_replaces_existing_registry_as_one_complete_document() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("providers.toml");
    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry.upsert_provider(provider("first")).unwrap();
    registry.save().unwrap();

    registry.upsert_provider(provider("second")).unwrap();
    registry.save().unwrap();

    let loaded = ProviderRegistry::load_or_create(&path).unwrap();
    let ids: Vec<_> = loaded.providers().map(|value| value.id.as_str()).collect();
    assert_eq!(ids, vec!["first", "second"]);
    assert!(toml::from_str::<toml::Value>(&std::fs::read_to_string(path).unwrap()).is_ok());
}

#[test]
fn model_wire_api_and_provider_model_override_resolve_in_order() {
    let key = ModelKey::new("proxy", "gpt-5").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();

    let mut descriptor = model(key.clone());
    descriptor.wire_api = Some(WireApi::Responses);
    registry.upsert_model(descriptor).unwrap();
    let resolved = registry.resolve_wire_api(&key).unwrap();
    assert_eq!(resolved.wire_api, WireApi::Responses);
    assert_eq!(resolved.source, WireApiSource::ModelDefinition);

    registry
        .set_model_provider_override(
            &key,
            ProviderModelOverride {
                wire_api: Some(WireApi::ChatCompletions),
                payload: serde_json::json!({ "temperature": 0.2 })
                    .as_object()
                    .cloned()
                    .unwrap(),
            },
        )
        .unwrap();
    let resolved = registry.resolve_wire_api(&key).unwrap();
    assert_eq!(resolved.wire_api, WireApi::ChatCompletions);
    assert_eq!(resolved.source, WireApiSource::ProviderModelOverride);
}

#[test]
fn model_wire_api_settings_survive_registry_reload() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    let key = ModelKey::new("proxy", "gpt-5").unwrap();
    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry.upsert_model(model(key.clone())).unwrap();
    registry
        .set_model_wire_api(&key, Some(WireApi::Responses))
        .unwrap();
    registry
        .set_model_provider_override(
            &key,
            ProviderModelOverride {
                wire_api: Some(WireApi::ChatCompletions),
                payload: serde_json::json!({ "temperature": 0.2 })
                    .as_object()
                    .cloned()
                    .unwrap(),
            },
        )
        .unwrap();
    registry.save().unwrap();

    let reloaded = ProviderRegistry::load_or_create(&path).unwrap();
    let resolved = reloaded.resolve_wire_api(&key).unwrap();
    assert_eq!(resolved.wire_api, WireApi::ChatCompletions);
    assert_eq!(resolved.source, WireApiSource::ProviderModelOverride);
    assert_eq!(
        reloaded.model_provider_override(&key).unwrap().payload["temperature"],
        0.2
    );
}

#[test]
fn absent_model_wire_api_has_no_provider_fallback() {
    let key = ModelKey::new("responses-provider", "legacy-model").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry
        .upsert_provider(provider("responses-provider"))
        .unwrap();
    registry.upsert_model(model(key.clone())).unwrap();

    let error = registry.resolve_wire_api(&key).unwrap_err();
    assert!(error.to_string().contains("wire API is not configured"));
}

#[test]
fn model_wire_api_is_isolated_across_exact_provider_model_pairs() {
    let key_a = ModelKey::new("proxy", "responses-model").unwrap();
    let key_b = ModelKey::new("proxy", "messages-model").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry.upsert_model(model(key_a.clone())).unwrap();
    registry.upsert_model(model(key_b.clone())).unwrap();

    registry
        .set_model_wire_api(&key_a, Some(WireApi::Responses))
        .unwrap();
    registry
        .set_model_wire_api(&key_b, Some(WireApi::Messages))
        .unwrap();

    assert_eq!(
        registry.resolve_wire_api(&key_a).unwrap().wire_api,
        WireApi::Responses
    );
    assert_eq!(
        registry.resolve_wire_api(&key_b).unwrap().wire_api,
        WireApi::Messages
    );

    registry
        .set_model_provider_override(
            &key_a,
            ProviderModelOverride {
                wire_api: Some(WireApi::ChatCompletions),
                payload: Default::default(),
            },
        )
        .unwrap();
    assert_eq!(
        registry.resolve_wire_api(&key_a).unwrap().wire_api,
        WireApi::ChatCompletions
    );
    assert_eq!(
        registry.resolve_wire_api(&key_b).unwrap().wire_api,
        WireApi::Messages
    );
}

#[test]
fn model_override_rejects_credential_payload_keys() {
    let key = ModelKey::new("proxy", "gpt-5").unwrap();
    let mut registry = ProviderRegistry::in_memory();
    registry.upsert_provider(provider("proxy")).unwrap();
    registry.upsert_model(model(key.clone())).unwrap();
    let error = registry
        .set_model_provider_override(
            &key,
            ProviderModelOverride {
                wire_api: None,
                payload: serde_json::json!({ "api_key": "secret" })
                    .as_object()
                    .cloned()
                    .unwrap(),
            },
        )
        .expect_err("credential-like payload must be rejected");
    assert!(error.to_string().contains("credential-like"));
}
