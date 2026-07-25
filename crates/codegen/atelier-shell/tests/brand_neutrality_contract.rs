fn production_part(source: &str) -> &str {
    source
        .split_once("#[cfg(test)]")
        .map_or(source, |(production, _)| production)
}

#[test]
fn goal_runtime_prompts_are_atelier_branded() {
    let prompts = [
        include_str!("../src/session/templates/goal_planner_prompt.md"),
        include_str!("../src/session/templates/goal_strategist_prompt.md"),
        include_str!("../src/session/templates/goal_summarizer_prompt.md"),
        include_str!("../src/session/templates/goal_verifier_prompt.md"),
        include_str!("../src/session/templates/goal_rules.md"),
        include_str!("../src/session/templates/goal_task_discipline.md"),
    ]
    .join("\n");

    for forbidden in ["Grok", "xAI", "SpaceXAI"] {
        assert!(
            !prompts.contains(forbidden),
            "Goal runtime prompt still contains vendor brand {forbidden:?}"
        );
    }
    assert!(
        prompts.contains("Atelier"),
        "Goal runtime prompts must identify the runtime as Atelier"
    );
}

#[test]
fn removed_vendor_surfaces_do_not_return() {
    let pager_status = include_str!("../../atelier-pager/src/app/dispatch/status.rs");
    assert!(
        !pager_status.contains("coding_data_sharing"),
        "dead coding-data-sharing implementation remains"
    );

    let marketplace_sources = [
        production_part(include_str!("../../atelier-plugin-marketplace/src/lib.rs")),
        production_part(include_str!(
            "../../atelier-plugin-marketplace/src/install_resolve.rs"
        )),
        production_part(include_str!("../src/plugin.rs")),
        production_part(include_str!("../src/extensions/marketplace.rs")),
        production_part(include_str!("../../atelier-pager/src/app/dispatch/cta.rs")),
        production_part(include_str!(
            "../../atelier-pager/src/app/agent_view/cta.rs"
        )),
    ]
    .join("\n");
    for forbidden in [
        "xAI Official",
        "xai-org/plugin-marketplace",
        "OFFICIAL_SOURCE_NAME",
        "OFFICIAL_SOURCE_GIT_URL",
        "is_official_source_url",
    ] {
        assert!(
            !marketplace_sources.contains(forbidden),
            "legacy xAI marketplace production logic remains: {forbidden}"
        );
    }
}

#[test]
fn cargo_descriptions_are_vendor_neutral() {
    let manifests = [
        include_str!("../../atelier-chat-state/Cargo.toml"),
        include_str!("../../atelier-workspace-types/Cargo.toml"),
        include_str!("../../atelier-sampling-types/Cargo.toml"),
        include_str!("../../atelier-sampler/Cargo.toml"),
    ];
    for manifest in manifests {
        for forbidden in ["xAI", "Grok", "SpaceXAI"] {
            assert!(
                !manifest.contains(forbidden),
                "Cargo description contains vendor brand {forbidden:?}"
            );
        }
    }
}

#[test]
fn internal_identifiers_are_vendor_neutral() {
    let sources = [
        include_str!("../../../build/atelier-proto-build/src/lib.rs"),
        include_str!("../../atelier-acp-runtime/src/common.rs"),
        include_str!("../../atelier-shared/src/placeholder_images.rs"),
        include_str!("../../atelier-tools/src/types/memory_backend.rs"),
        include_str!("../../atelier-telemetry/src/memory_log.rs"),
        include_str!("../src/session/helpers/memory_flush.rs"),
        include_str!("../src/session/acp_session.rs"),
        include_str!("../../atelier-gix-status/src/lib.rs"),
        include_str!("../../atelier-tty-utils/src/lib.rs"),
        include_str!("../src/extensions/notification.rs"),
        include_str!("../src/session/acp_session_impl/recap.rs"),
        include_str!("../src/session/acp_session_tests/recap_display_only_tests.rs"),
    ]
    .join("\n");
    for forbidden in [
        "XaiProtoBuilder",
        "xaiAcpChannelFailure",
        "xai.dev/imageDisplayNumber",
        "xai_memory",
        "xai_session",
        "XAI_GIX_STATUS",
        "__XAI_STDERR_REDIRECT_SUBPROCESS",
        "xai-recap",
    ] {
        assert!(
            !sources.contains(forbidden),
            "vendor-namespaced internal identifier remains: {forbidden}"
        );
    }
}
