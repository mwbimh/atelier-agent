use std::fs;
use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(3)
        .expect("atelier-pager must live under crates/codegen")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = workspace_root().join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

#[test]
fn vendor_hosted_media_and_feedback_have_no_runtime_entry_points() {
    let pager_manifest = read("crates/codegen/atelier-pager/Cargo.toml");
    assert!(
        !pager_manifest.contains("atelier-voice"),
        "the shipped pager must not link the vendor voice client"
    );

    let pager_lib = read("crates/codegen/atelier-pager/src/lib.rs");
    assert!(!pager_lib.contains("mod voice"));
    assert!(!pager_lib.contains("mod client_identity"));

    let slash_registry = read("crates/codegen/atelier-pager/src/slash/commands/mod.rs");
    for forbidden in [
        "feedback::FeedbackCommand",
        "voice::VoiceCommand",
        "imagine_video::ImagineVideoCommand",
    ] {
        assert!(
            !slash_registry.contains(forbidden),
            "vendor-hosted slash entry remains registered: {forbidden}"
        );
    }

    let tool_modules =
        read("crates/codegen/atelier-tools/src/implementations/atelier_build/mod.rs");
    for forbidden in [
        "pub mod image_edit",
        "ImageEditTool",
        "ImageToVideoTool",
        "ReferenceToVideoTool",
    ] {
        assert!(
            !tool_modules.contains(forbidden),
            "vendor-hosted media implementation remains exported: {forbidden}"
        );
    }

    let registry = read("crates/codegen/atelier-tools/src/registry/types.rs");
    let agent_builder = read("crates/codegen/atelier-agent/src/builder.rs");
    let agent_config = read("crates/codegen/atelier-agent/src/config.rs");
    for forbidden in ["ImageEditTool", "ImageToVideoTool", "ReferenceToVideoTool"] {
        assert!(
            !registry.contains(forbidden),
            "tool registry exposes {forbidden}"
        );
        assert!(
            !agent_builder.contains(forbidden),
            "agent builder can enable {forbidden}"
        );
        assert!(
            !agent_config.contains(forbidden),
            "built-in toolset contains {forbidden}"
        );
    }
}

#[test]
fn xai_hosted_tools_fail_closed_without_explicit_provider_capabilities() {
    let provider = read("crates/codegen/atelier-provider/src/lib.rs");
    let agent_builder = read("crates/codegen/atelier-agent/src/builder.rs");
    let agent_ops = read("crates/codegen/atelier-shell/src/agent/mvp_agent/agent_ops.rs");

    assert!(
        !provider.contains("pub x_search:"),
        "x_search must not be enabled until the Provider/model schema can express it explicitly"
    );
    assert!(
        !provider.contains("pub video_generation:"),
        "video generation must not be enabled until the Provider/model schema can express it explicitly"
    );
    assert!(
        !agent_builder.contains("HostedTool::XSearch"),
        "AgentBuilder must not inject xAI XSearch from the generic backend-search toggle"
    );
    assert!(
        agent_ops.contains("ImageGenConfig::Disabled"),
        "agent construction must keep Imagine disabled until the exact Provider/model route is resolved"
    );
    assert!(
        agent_ops.contains("VideoGenConfig::Disabled"),
        "video generation must remain fail-closed without an exact Provider/model capability"
    );
}

#[test]
fn retained_local_surfaces_do_not_contain_vendor_endpoints() {
    let owned_sources = [
        "crates/codegen/atelier-announcements/src/lib.rs",
        "crates/codegen/atelier-pager/src/client_identity.rs",
        "crates/codegen/atelier-pager/src/voice/auth.rs",
        "crates/codegen/atelier-pager/src/voice/handle.rs",
        "crates/codegen/atelier-pager/src/voice/mod.rs",
        "crates/codegen/atelier-tools/src/implementations/atelier_build/image_gen/mod.rs",
        "crates/codegen/atelier-tools/src/implementations/atelier_build/image_edit/mod.rs",
        "crates/codegen/atelier-tools/src/implementations/atelier_build/video_gen/mod.rs",
    ];

    for relative in owned_sources {
        let path = workspace_root().join(relative);
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        for forbidden in ["api.x.ai"] {
            assert!(
                !source.contains(forbidden),
                "{} still contains vendor endpoint/capability marker {forbidden:?}",
                path.display()
            );
        }
    }

    let announcements = read("crates/codegen/atelier-announcements/src/lib.rs");
    assert!(
        announcements.contains("ATELIER_ANNOUNCEMENTS_OVERRIDE"),
        "the explicit local announcement override must remain available"
    );
}

#[test]
fn vendor_billing_and_subscription_have_no_runtime_entry_points() {
    let slash_registry = read("crates/codegen/atelier-pager/src/slash/commands/mod.rs");
    assert!(
        !slash_registry.contains("usage::UsageCommand"),
        "the vendor billing /usage command must not be registered"
    );

    let app_mod = read("crates/codegen/atelier-pager/src/app/mod.rs");
    assert!(
        !app_mod.contains("mod subscription"),
        "the vendor subscription watcher must not be compiled"
    );

    for relative in [
        "crates/codegen/atelier-pager/src/app/event_loop.rs",
        "crates/codegen/atelier-pager/src/app/dispatch/session/lifecycle.rs",
        "crates/codegen/atelier-pager/src/app/dispatch/session/load.rs",
        "crates/codegen/atelier-pager/src/app/dispatch/prompt.rs",
        "crates/codegen/atelier-pager/src/app/effects/mod.rs",
    ] {
        let source = read(relative);
        for forbidden in [
            "Effect::FetchBilling",
            "Effect::FetchAppBilling",
            "fire_subscription_check",
            "atelier/billing",
        ] {
            assert!(
                !source.contains(forbidden),
                "{relative} still exposes vendor billing/subscription path {forbidden}"
            );
        }
    }
}
