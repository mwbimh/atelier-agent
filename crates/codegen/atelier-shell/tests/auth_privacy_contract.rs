const SOURCES: &[(&str, &str)] = &[
    ("auth/model.rs", include_str!("../src/auth/model.rs")),
    (
        "auth/attribution.rs",
        include_str!("../src/auth/attribution.rs"),
    ),
    ("auth/manager.rs", include_str!("../src/auth/manager.rs")),
    (
        "auth/manager/enrichment.rs",
        include_str!("../src/auth/manager/enrichment.rs"),
    ),
    ("auth/recovery.rs", include_str!("../src/auth/recovery.rs")),
    (
        "auth/refresh/mod.rs",
        include_str!("../src/auth/refresh/mod.rs"),
    ),
    (
        "auth/refresh/oidc_refresher.rs",
        include_str!("../src/auth/refresh/oidc_refresher.rs"),
    ),
    (
        "auth/oidc/protocol.rs",
        include_str!("../src/auth/oidc/protocol.rs"),
    ),
    (
        "auth/oidc/refresh.rs",
        include_str!("../src/auth/oidc/refresh.rs"),
    ),
    (
        "agent/mvp_agent/acp_agent.rs",
        include_str!("../src/agent/mvp_agent/acp_agent.rs"),
    ),
    (
        "session/acp_session_impl/sampler_turn.rs",
        include_str!("../src/session/acp_session_impl/sampler_turn.rs"),
    ),
];

#[test]
fn auth_diagnostics_do_not_compute_or_name_credential_fragments() {
    let forbidden = [
        "token_suffix(",
        "key_prefix",
        "key_suffix",
        "rt_prefix",
        "rt_suffix",
        "token_prefix",
        "token_suffix",
        "sent_key_prefix",
        "current_key_prefix",
    ];

    let mut violations = Vec::new();
    for (path, source) in SOURCES {
        for needle in forbidden {
            if source.contains(needle) {
                violations.push(format!("{path}: contains {needle:?}"));
            }
        }
    }

    assert!(
        violations.is_empty(),
        "credential-fragment diagnostics remain:\n{}",
        violations.join("\n")
    );
}

#[test]
fn auth_refresh_has_no_remote_diagnostic_upload_hook() {
    let sources = [
        include_str!("../src/auth/refresh/mod.rs"),
        include_str!("../src/auth/refresh/oidc_refresher.rs"),
        include_str!("../src/auth/manager.rs"),
    ]
    .join("\n");

    for forbidden in [
        "DiagnosticUploader",
        "diagnostic_uploader",
        "spawn_diagnostic_upload",
        "with_diagnostic_upload",
    ] {
        assert!(
            !sources.contains(forbidden),
            "remote diagnostic upload hook remains: {forbidden}"
        );
    }
}

fn production_part<'a>(source: &'a str, test_module_marker: &str) -> &'a str {
    source
        .split_once(test_module_marker)
        .map_or(source, |(production, _)| production)
}

#[test]
fn production_auth_and_endpoint_defaults_contain_no_vendor_hosts() {
    let sources = [
        (
            "auth/config.rs",
            production_part(
                include_str!("../src/auth/config.rs"),
                "#[cfg(test)]\nmod tests",
            ),
        ),
        (
            "auth/device_code.rs",
            production_part(
                include_str!("../src/auth/device_code.rs"),
                "#[cfg(test)]\npub(crate) mod tests",
            ),
        ),
        (
            "auth/oidc/protocol.rs",
            production_part(
                include_str!("../src/auth/oidc/protocol.rs"),
                "#[cfg(test)]\nmod tests",
            ),
        ),
        (
            "agent/config.rs",
            production_part(
                include_str!("../src/agent/config.rs"),
                "#[cfg(test)]\nmod tests",
            ),
        ),
    ];

    let forbidden = [
        "auth.x.ai",
        "api.x.ai",
        "accounts.x.ai",
        "XAI_OAUTH2_ISSUER",
    ];
    let mut violations = Vec::new();
    for (path, source) in sources {
        for host in forbidden {
            if source.contains(host) {
                violations.push(format!("{path}: contains {host:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "vendor network defaults remain:\n{}",
        violations.join("\n")
    );
}

#[test]
fn generic_runtime_contains_no_global_xai_api_key_fallback_or_auth_method() {
    let sources = [
        (
            "agent/auth_method.rs",
            include_str!("../src/agent/auth_method.rs"),
        ),
        ("agent/config.rs", include_str!("../src/agent/config.rs")),
        ("config/mod.rs", include_str!("../src/config/mod.rs")),
        ("cli_models.rs", include_str!("../src/cli_models.rs")),
        (
            "auth/test_support.rs",
            include_str!("../src/auth/test_support.rs"),
        ),
        ("auth/mod.rs", include_str!("../src/auth/mod.rs")),
        (
            "session/unified_list/mod.rs",
            include_str!("../src/session/unified_list/mod.rs"),
        ),
    ];
    let forbidden = [
        "XAI_API_KEY",
        "ATELIER_CODE_XAI_API_KEY",
        "XAI_API_KEY_METHOD_ID",
        "XaiApiKey",
        "read_xai_api_key_env",
        "has_xai_api_key_env",
        "should_advertise_xai_api_key",
        "xai_api_base_url",
        "ATELIER_XAI_API_BASE_URL",
        "XAI_API_BASE_URL_DEFAULT",
        "XAI_OAUTH2_ISSUER",
        "xai_oauth2_issuer",
        "is_xai_oauth2_issuer",
        "LEGACY_VENDOR_TEST_ISSUER",
        "xai_auth_manager",
        "https://auth.x.ai",
        "https://api.x.ai",
    ];
    let mut violations = Vec::new();
    for (path, source) in sources {
        for needle in forbidden {
            if source.contains(needle) {
                violations.push(format!("{path}: contains {needle:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "generic xAI API-key fallback remains:\n{}",
        violations.join("\n")
    );
}

#[test]
fn generic_provider_runtime_has_no_xai_auth_or_first_party_gate() {
    let sources = [
        ("auth/model.rs", include_str!("../src/auth/model.rs")),
        (
            "auth/external_auth.rs",
            include_str!("../src/auth/external_auth.rs"),
        ),
        ("auth/flow.rs", include_str!("../src/auth/flow.rs")),
        (
            "agent/auth_method.rs",
            include_str!("../src/agent/auth_method.rs"),
        ),
        (
            "agent/mvp_agent/agent_ops.rs",
            include_str!("../src/agent/mvp_agent/agent_ops.rs"),
        ),
        (
            "agent/mvp_agent/mod.rs",
            include_str!("../src/agent/mvp_agent/mod.rs"),
        ),
        (
            "extensions/mod.rs",
            include_str!("../src/extensions/mod.rs"),
        ),
        (
            "session/acp_session_impl/sampler_turn.rs",
            include_str!("../src/session/acp_session_impl/sampler_turn.rs"),
        ),
        (
            "atelier-shell-base/util/mod.rs",
            include_str!("../../atelier-shell-base/src/util/mod.rs"),
        ),
    ];
    let forbidden = [
        "is_xai_auth",
        "require_xai_auth",
        "first_party_xai_url",
        "endpoint_is_first_party",
        "XAI_API_KEY",
    ];
    let mut violations = Vec::new();
    for (path, source) in sources {
        for needle in forbidden {
            if source.contains(needle) {
                violations.push(format!("{path}: contains {needle:?}"));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "generic Provider runtime still contains a global xAI gate:\n{}",
        violations.join("\n")
    );
}

#[test]
fn grok_model_preset_remains_available_for_explicit_providers() {
    let defaults = include_str!("../../atelier-config/defaults/models/xai.toml");
    assert!(
        defaults.contains("[models.\"grok-4.3\"]")
            && defaults.contains("wire_api = \"chat_completions\"")
            && defaults.contains("[models.\"grok-4.5\"]"),
        "the explicit Grok model presets must remain available"
    );
}
