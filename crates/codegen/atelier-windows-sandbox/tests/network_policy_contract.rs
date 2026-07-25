#![cfg(windows)]

use atelier_windows_sandbox::CommandRequest;
use atelier_windows_sandbox::NetworkPolicy;
use atelier_windows_sandbox::SANDBOX_NETWORK_ALLOWED_USERNAME;
use atelier_windows_sandbox::SANDBOX_NETWORK_DISABLED_USERNAME;
use atelier_windows_sandbox::SandboxMode;
use atelier_windows_sandbox::network_identity_username;
use std::path::PathBuf;

fn request(mode: SandboxMode) -> CommandRequest {
    CommandRequest::new(
        mode,
        vec![PathBuf::from(r"C:\workspace")],
        PathBuf::from(r"C:\workspace"),
        PathBuf::from(r"C:\Windows\System32\cmd.exe"),
        Vec::new(),
    )
}

#[test]
fn built_in_modes_have_explicit_network_defaults() {
    assert_eq!(
        request(SandboxMode::ReadOnly).network_policy,
        NetworkPolicy::Disabled
    );
    assert_eq!(
        request(SandboxMode::WorkspaceWrite).network_policy,
        NetworkPolicy::AllowAll
    );
}

#[test]
fn an_explicit_network_policy_overrides_the_mode_default() {
    let request = request(SandboxMode::WorkspaceWrite).with_network_policy(NetworkPolicy::Disabled);

    assert_eq!(request.network_policy, NetworkPolicy::Disabled);
}

#[test]
fn network_policy_selects_distinct_persistent_accounts() {
    assert_eq!(
        network_identity_username(NetworkPolicy::AllowAll),
        SANDBOX_NETWORK_ALLOWED_USERNAME
    );
    assert_eq!(
        network_identity_username(NetworkPolicy::Disabled),
        SANDBOX_NETWORK_DISABLED_USERNAME
    );
    assert_ne!(
        SANDBOX_NETWORK_ALLOWED_USERNAME,
        SANDBOX_NETWORK_DISABLED_USERNAME
    );
}

#[test]
fn persistent_filters_are_readable_for_unelevated_status_verification() {
    let source = include_str!("../src/network/wfp.rs");
    assert!(source.contains("FwpmFilterSetSecurityInfoByKey0"));
    assert!(source.contains("FWPM_ACTRL_READ"));
    assert!(source.contains("S-1-5-32-545"));
}
