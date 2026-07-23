//! Vendor authentication extensions are intentionally unavailable in Atelier.

use agent_client_protocol as acp;

use super::ExtResult;
use crate::agent::MvpAgent;

#[tracing::instrument(skip_all, fields(method = %args.method))]
pub async fn handle(_agent: &MvpAgent, args: &acp::ExtRequest) -> ExtResult {
    tracing::warn!(
        method = %args.method,
        "rejected vendor authentication extension in provider-only runtime"
    );
    Err(acp::Error::method_not_found()
        .data("vendor authentication is unavailable; configure credentials on an Atelier Provider"))
}
