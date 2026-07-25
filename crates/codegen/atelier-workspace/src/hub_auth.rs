//! Explicit, vendor-neutral authorization for the optional standalone
//! workspace hub client.
//!
//! This module deliberately does not inspect `ATELIER_HOME`, `HOME`,
//! `auth.json`, browser-login state, or historical vendor OIDC entries. The
//! caller must provide `--auth-config PATH`; the file contains one explicit
//! bearer credential and optional session-owner identity.

use std::path::Path;
use std::sync::Arc;

use atelier_tool_hub_sdk::{AuthCredential, AuthIdentity, AuthProvider};
use url::Url;

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ExplicitHubAuth {
    bearer_token: String,
    #[serde(default)]
    user_id: Option<String>,
    #[serde(default)]
    principal_type: Option<String>,
    #[serde(default)]
    principal_id: Option<String>,
}

struct ExplicitBearerProvider {
    token: String,
    identity: Option<AuthIdentity>,
}

impl std::fmt::Debug for ExplicitBearerProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExplicitBearerProvider")
            .field("identity", &self.identity)
            .finish_non_exhaustive()
    }
}

impl AuthProvider for ExplicitBearerProvider {
    fn current(&self) -> AuthCredential {
        AuthCredential::bearer(self.token.clone())
    }

    fn identity(&self) -> Option<AuthIdentity> {
        self.identity.clone()
    }
}

fn read_explicit_auth(path: &Path) -> anyhow::Result<ExplicitHubAuth> {
    let content = std::fs::read_to_string(path)
        .map_err(|error| anyhow::anyhow!("failed to read {}: {error}", path.display()))?;
    let config: ExplicitHubAuth = serde_json::from_str(&content).map_err(|error| {
        anyhow::anyhow!(
            "failed to parse explicit workspace auth config {}: {error}",
            path.display()
        )
    })?;
    if config.bearer_token.trim().is_empty() {
        anyhow::bail!("explicit workspace auth bearer_token must not be empty");
    }
    Ok(config)
}

fn explicit_identity(config: &ExplicitHubAuth) -> Option<AuthIdentity> {
    if config.user_id.is_none() && config.principal_type.is_none() && config.principal_id.is_none()
    {
        return None;
    }
    Some(AuthIdentity {
        user_id: config.user_id.clone().unwrap_or_default(),
        principal_type: config.principal_type.clone(),
        principal_id: config.principal_id.clone(),
    })
}

/// Build the optional standalone hub's authorization provider.
///
/// `auth_config` is mandatory. No implicit credential location is consulted,
/// and no token refresh or credential migration is performed by the Workspace
/// runtime.
pub fn provider(
    _hub_url: &Url,
    auth_config: Option<&Path>,
) -> anyhow::Result<Arc<dyn AuthProvider>> {
    let path = auth_config.ok_or_else(|| {
        anyhow::anyhow!(
            "workspace hub authorization requires an explicit --auth-config PATH; implicit auth.json scanning is disabled"
        )
    })?;
    let config = read_explicit_auth(path)?;
    Ok(Arc::new(ExplicitBearerProvider {
        identity: explicit_identity(&config),
        token: config.bearer_token,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_auth_json(dir: &std::path::Path, json: &str) -> std::path::PathBuf {
        let path = dir.join("workspace-auth.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn provider_without_explicit_config_does_not_scan_legacy_auth_json() {
        let home = tempfile::tempdir().unwrap();
        let _env = crate::LockedTestEnv::lock().set("ATELIER_HOME", home.path());
        std::fs::write(
            home.path().join("auth.json"),
            r#"{
                "https://auth.x.ai": {
                    "key": "legacy-access-token",
                    "refresh_token": "legacy-refresh-token",
                    "oidc_issuer": "https://auth.x.ai"
                }
            }"#,
        )
        .unwrap();

        let url = Url::parse("ws://localhost:9988/v1/tools").unwrap();
        let error = provider(&url, None).expect_err("implicit auth scanning must be disabled");
        assert!(error.to_string().contains("explicit --auth-config"));
    }

    #[test]
    fn explicit_generic_bearer_and_session_identity_are_accepted() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
                "bearer_token": "provider-session-token",
                "user_id": "session-owner",
                "principal_type": "Team",
                "principal_id": "team-1"
            }"#,
        );
        let url = Url::parse("wss://hub.example.test/v1/tools").unwrap();
        let auth = provider(&url, Some(&path)).unwrap();

        match auth.current() {
            AuthCredential::Bearer { token } => assert_eq!(token, "provider-session-token"),
            other => panic!("expected explicit bearer, got {other:?}"),
        }
        let identity = auth.identity().expect("explicit identity");
        assert_eq!(identity.user_id, "session-owner");
        assert_eq!(identity.principal_type.as_deref(), Some("Team"));
        assert_eq!(identity.principal_id.as_deref(), Some("team-1"));
    }

    #[test]
    fn legacy_vendor_auth_map_is_rejected_instead_of_migrated() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth_json(
            dir.path(),
            r#"{
                "https://auth.x.ai": {
                    "key": "legacy-access-token",
                    "refresh_token": "legacy-refresh-token",
                    "oidc_issuer": "https://auth.x.ai",
                    "oidc_client_id": "legacy-client"
                }
            }"#,
        );
        let url = Url::parse("wss://hub.example.test/v1/tools").unwrap();

        let error = provider(&url, Some(&path)).expect_err("legacy auth must be rejected");
        assert!(error.to_string().contains("explicit workspace auth config"));
    }

    #[test]
    fn workspace_worker_source_has_no_credential_discovery_dependency() {
        let source = include_str!("worker.rs");
        for forbidden in ["hub_auth", "auth.json", "auth.x.ai", "oidc_issuer"] {
            assert!(
                !source.contains(forbidden),
                "Workspace Worker must not depend on legacy credential discovery: {forbidden}"
            );
        }
    }
}
