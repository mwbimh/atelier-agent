#[test]
fn composition_root_contains_no_vendor_remote_startup_or_shutdown_hooks() {
    let source = include_str!("../src/main.rs");
    for forbidden in [
        "start_early_prefetch",
        "join_early_prefetch",
        "atelier_update",
        "auto_update",
        "enforce_minimum_version_or_exit",
        "otel_layer",
        "external::init",
        "sentry::flush_on_shutdown",
        "run_setup_command",
        "run_update_command",
    ] {
        assert!(
            !source.contains(forbidden),
            "composition root still contains vendor remote hook `{forbidden}`"
        );
    }
}

#[test]
fn background_update_checker_is_physically_absent() {
    let entrypoint = include_str!("../src/main.rs");
    let leader_app = include_str!("../../../crates/codegen/atelier-shell/src/agent/app.rs");

    for forbidden in [
        "LeaderAutoUpdateConfig",
        "run_auto_update_checker",
        "auto_update_check",
        "stdio_direct_update_eligible",
    ] {
        assert!(
            !entrypoint.contains(forbidden),
            "composition root still references background update checker `{forbidden}`"
        );
        assert!(
            !leader_app.contains(forbidden),
            "leader runtime still contains background update checker `{forbidden}`"
        );
    }
}

#[test]
fn explicit_update_keeps_the_leader_relaunch_protocol() {
    let entrypoint = include_str!("../src/main.rs");
    let protocol = include_str!("../../../crates/codegen/atelier-shell/src/leader/protocol.rs");

    assert!(entrypoint.contains("signal_leaders_to_relaunch"));
    assert!(entrypoint.contains("ControlCommand::RelaunchForUpdate"));
    assert!(protocol.contains("RelaunchForUpdate"));
}

#[test]
fn explicit_user_network_capabilities_remain_available() {
    let cli = include_str!("../../../crates/codegen/atelier-pager/src/app/cli.rs");
    let commands = include_str!("../../../crates/codegen/atelier-pager/src/slash/commands/mod.rs");
    assert!(cli.contains("Mcp(crate::mcp_cmd::McpArgs)"));
    assert!(commands.contains("Arc::new(provider::ProviderCommand)"));
    assert!(commands.contains("Arc::new(mcps::McpsCommand)"));
    assert!(cli.contains("disable_web_search"));
}
