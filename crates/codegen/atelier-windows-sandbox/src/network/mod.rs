//! Windows child-network policy and persistent network identities.

mod wfp;

pub(crate) use wfp::disabled_network_filters_installed_for_sid;
pub(crate) use wfp::install_disabled_network_filters_for_account;
pub(crate) use wfp::remove_disabled_network_filters;

pub const SANDBOX_NETWORK_ALLOWED_USERNAME: &str = "AtelierSandbox";
pub const SANDBOX_NETWORK_DISABLED_USERNAME: &str = "AtelierSandboxNoNet";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NetworkPolicy {
    Disabled,
    AllowAll,
}

impl NetworkPolicy {
    pub const fn for_mode(mode: crate::SandboxMode) -> Self {
        match mode {
            crate::SandboxMode::ReadOnly => Self::Disabled,
            crate::SandboxMode::WorkspaceWrite => Self::AllowAll,
        }
    }
}

pub const fn network_identity_username(policy: NetworkPolicy) -> &'static str {
    match policy {
        NetworkPolicy::Disabled => SANDBOX_NETWORK_DISABLED_USERNAME,
        NetworkPolicy::AllowAll => SANDBOX_NETWORK_ALLOWED_USERNAME,
    }
}
