use atelier_provider::{
    CredentialRef, ProviderConfig, ProviderDiscovery, ProviderProtocol, ProviderRegistry,
    ProviderSnapshot,
};
use std::collections::BTreeMap;
use tempfile::tempdir;
use url::Url;

fn provider_with_headers() -> ProviderConfig {
    let mut extra_headers = BTreeMap::new();
    extra_headers.insert(
        "Authorization".to_owned(),
        "Bearer authorization-secret".to_owned(),
    );
    extra_headers.insert("Cookie".to_owned(), "session=cookie-secret".to_owned());
    extra_headers.insert("X-API-Key".to_owned(), "api-key-secret".to_owned());
    extra_headers.insert("X-Request-Tags".to_owned(), "private-tag-value".to_owned());

    ProviderConfig {
        id: "proxy".into(),
        display_name: "Proxy".into(),
        protocol: ProviderProtocol::OpenAiResponses,
        base_url: Url::parse("https://models.example.test/v1").unwrap(),
        credential: CredentialRef::None,
        discovery: ProviderDiscovery::Static,
        extra_headers,
        enabled: true,
    }
}

#[test]
fn provider_config_debug_redacts_all_extra_header_values() {
    let debug = format!("{:?}", provider_with_headers());

    for secret in [
        "Bearer authorization-secret",
        "session=cookie-secret",
        "api-key-secret",
        "private-tag-value",
    ] {
        assert!(!debug.contains(secret), "debug output leaked {secret:?}");
    }
    assert!(debug.contains("REDACTED"));
}

#[test]
fn provider_snapshot_serialization_redacts_all_extra_header_values() {
    let snapshot = ProviderSnapshot {
        providers: vec![provider_with_headers()],
        models: Vec::new(),
        default_model: None,
        model_provider_overrides: Default::default(),
    };
    let encoded = serde_json::to_string(&snapshot).unwrap();

    for secret in [
        "Bearer authorization-secret",
        "session=cookie-secret",
        "api-key-secret",
        "private-tag-value",
    ] {
        assert!(!encoded.contains(secret), "snapshot leaked {secret:?}");
    }
    assert!(encoded.contains("REDACTED"));
}

#[test]
fn provider_config_serialization_redacts_credentials_but_preserves_safe_headers() {
    let encoded = toml::to_string(&provider_with_headers()).unwrap();

    for secret in [
        "Bearer authorization-secret",
        "session=cookie-secret",
        "api-key-secret",
    ] {
        assert!(
            !encoded.contains(secret),
            "provider config leaked {secret:?}"
        );
    }
    assert!(encoded.contains("private-tag-value"));
    assert!(encoded.contains("REDACTED"));
}

#[test]
fn non_credential_extra_headers_still_round_trip_through_registry_persistence() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    let mut provider = provider_with_headers();
    provider
        .extra_headers
        .retain(|name, _| name == "X-Request-Tags");

    let mut registry = ProviderRegistry::load_or_create(&path).unwrap();
    registry.upsert_provider(provider).unwrap();
    registry.save().unwrap();

    let loaded = ProviderRegistry::load_or_create(&path).unwrap();
    assert_eq!(
        loaded
            .provider("proxy")
            .unwrap()
            .extra_headers
            .get("X-Request-Tags")
            .map(String::as_str),
        Some("private-tag-value")
    );
}

#[test]
fn provider_validation_rejects_credential_headers_with_a_clear_error() {
    for header_name in ["Authorization", "Cookie", "api-key", "X-API-Key"] {
        let mut provider = provider_with_headers();
        provider.extra_headers.clear();
        provider
            .extra_headers
            .insert(header_name.to_owned(), "header-secret".to_owned());

        let error = provider.validate().unwrap_err().to_string();
        assert!(
            error.contains(header_name),
            "error omitted header name: {error}"
        );
        assert!(
            error.to_ascii_lowercase().contains("credential"),
            "error did not explain the credential boundary: {error}"
        );
        assert!(!error.contains("header-secret"));
    }
}

#[test]
fn registry_rejects_persisted_credential_headers_before_loading_them() {
    let directory = tempdir().unwrap();
    let path = directory.path().join("providers.toml");
    std::fs::write(
        &path,
        r#"
schema_version = 1

[providers.proxy]
id = "proxy"
display_name = "Proxy"
protocol = "open_ai_responses"
base_url = "https://models.example.test/v1"
credential = { type = "none" }
discovery = { type = "static" }
extra_headers = { Authorization = "Bearer persisted-secret" }
enabled = true

[models]
"#,
    )
    .unwrap();

    let error = match ProviderRegistry::load_or_create(&path) {
        Ok(_) => panic!("persisted credential header should be rejected"),
        Err(error) => error.to_string(),
    };
    assert!(
        error.contains("Authorization"),
        "error omitted header name: {error}"
    );
    assert!(
        error.to_ascii_lowercase().contains("credential"),
        "error did not explain the credential boundary: {error}"
    );
    assert!(!error.contains("persisted-secret"));
}
