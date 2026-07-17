//! Local sandbox diagnostics for TUI and future GUI clients.

use agent_client_protocol as acp;

use super::{ExtResult, to_raw_response};
use crate::agent::MvpAgent;

pub const SANDBOX_STATUS: &str = "_atelier/sandbox/status";
pub const SANDBOX_DOCTOR: &str = "_atelier/sandbox/doctor";

/// Return the locally observed sandbox state without probing or contacting a
/// remote service. The status is intentionally read-only; changing policy is a
/// process-start operation and must go through local configuration.
pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        SANDBOX_STATUS | SANDBOX_DOCTOR | "atelier/sandbox/status" | "atelier/sandbox/doctor" => {
            to_raw_response(&atelier_sandbox::diagnostics())
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_method_names_are_private_extensions() {
        assert!(SANDBOX_STATUS.starts_with("_atelier/"));
        assert!(SANDBOX_DOCTOR.starts_with("_atelier/"));
    }
}
