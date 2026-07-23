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
