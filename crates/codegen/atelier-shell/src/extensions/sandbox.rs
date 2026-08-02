//! Local sandbox diagnostics and session-scoped override controls.

use agent_client_protocol as acp;
use serde::Deserialize;

use super::{ExtResult, parse_params, to_raw_response};
use crate::agent::MvpAgent;
use crate::session::SessionCommand;
use atelier_workspace::permission::ClientType;

pub const SANDBOX_STATUS: &str = "_atelier/sandbox/status";
pub const SANDBOX_DOCTOR: &str = "_atelier/sandbox/doctor";
pub const SANDBOX_OVERRIDE_AUTO_APPROVE: &str = "_atelier/sandbox/set_override_auto_approve";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetOverrideAutoApproveParams {
    session_id: String,
    enabled: bool,
}

/// Return sandbox diagnostics or update a session-scoped runtime control.
/// The base sandbox profile remains a process-start decision; the auto-approve
/// control only grants each requested host execution as a fresh one-command
/// AllowOnce decision.
pub async fn handle(agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    match args.method.as_ref() {
        SANDBOX_STATUS | SANDBOX_DOCTOR | "atelier/sandbox/status" | "atelier/sandbox/doctor" => {
            to_raw_response(&atelier_sandbox::diagnostics())
        }
        SANDBOX_OVERRIDE_AUTO_APPROVE | "atelier/sandbox/set_override_auto_approve" => {
            if !matches!(
                agent.client_type(),
                ClientType::AtelierTUI | ClientType::AtelierPager | ClientType::Desktop
            ) {
                return Err(acp::Error::invalid_params().data(
                    "sandbox override auto-approval requires an interactive Atelier client"
                        .to_owned(),
                ));
            }
            let params: SetOverrideAutoApproveParams = parse_params(args)?;
            if params.enabled
                && let Some(reason) =
                    atelier_workspace::permission::resolution::yolo_disabled_by_policy()
            {
                return Err(acp::Error::invalid_params().data(format!(
                    "sandbox override auto-approval is blocked by managed policy: {reason}"
                )));
            }
            let session_id = acp::SessionId::new(params.session_id.clone());
            let handle = agent.get_session_handle(&session_id).ok_or_else(|| {
                acp::Error::invalid_params()
                    .data(format!("unknown sessionId: {}", params.session_id))
            })?;
            let (respond_to, response_rx) = tokio::sync::oneshot::channel();
            handle
                .cmd_tx
                .send(SessionCommand::SetSandboxOverrideAutoApprove {
                    enabled: params.enabled,
                    respond_to,
                })
                .map_err(|_| {
                    acp::Error::internal_error()
                        .data("session permission manager is unavailable".to_owned())
                })?;
            let enabled = response_rx.await.map_err(|_| {
                acp::Error::internal_error()
                    .data("session permission manager did not apply the control".to_owned())
            })?;
            if params.enabled && !enabled {
                return Err(acp::Error::invalid_params().data(
                    "sandbox override auto-approval is blocked by managed policy".to_owned(),
                ));
            }
            to_raw_response(&serde_json::json!({
                "enabled": enabled,
                "message": if enabled {
                    "Sandbox override requests will be auto-approved once for this session"
                } else {
                    "Sandbox override requests require interactive approval"
                }
            }))
        }
        _ => Err(acp::Error::method_not_found()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_method_names_are_private_extensions() {
        assert!(SANDBOX_STATUS.starts_with("_atelier/"));
        assert!(SANDBOX_DOCTOR.starts_with("_atelier/"));
        assert!(SANDBOX_OVERRIDE_AUTO_APPROVE.starts_with("_atelier/"));
    }

    #[test]
    fn override_auto_approve_params_require_session_and_boolean() {
        let parsed: SetOverrideAutoApproveParams = serde_json::from_value(serde_json::json!({
            "sessionId": "session-1",
            "enabled": true
        }))
        .expect("valid params");
        assert_eq!(parsed.session_id, "session-1");
        assert!(parsed.enabled);
        assert!(
            serde_json::from_value::<SetOverrideAutoApproveParams>(serde_json::json!({
                "enabled": true
            }))
            .is_err()
        );
    }
}
