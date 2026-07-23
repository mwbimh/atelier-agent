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
fn explicit_user_network_capabilities_remain_available() {
    let cli = include_str!("../../atelier-pager/src/app/cli.rs");
    let commands = include_str!("../../atelier-pager/src/slash/commands/mod.rs");
    assert!(cli.contains("Mcp(crate::mcp_cmd::McpArgs)"));
    assert!(commands.contains("Arc::new(provider::ProviderCommand)"));
    assert!(commands.contains("Arc::new(mcps::McpsCommand)"));
    assert!(cli.contains("disable_web_search"));
}
