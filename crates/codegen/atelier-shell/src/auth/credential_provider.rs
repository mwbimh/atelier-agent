use crate::auth::AuthManager;
use crate::util::atelier_auth_credentials::AtelierAuthCredentials;
use atelier_auth::{AuthCredentialProvider, CredentialSnapshot, HttpAuth};
use reqwest::RequestBuilder;
use std::sync::Arc;
/// `api_key.id` for the active credential: hash the stable API key, never the
/// OIDC bearer (which rotates). `None` for non-API-key auth.
fn api_key_id_for(auth: Option<&crate::auth::AtelierAuth>) -> Option<String> {
    auth.filter(|a| matches!(a.auth_mode, crate::auth::AuthMode::ApiKey))
        .map(|a| atelier_telemetry::config::deployment_id_from_key(&a.key))
}
/// Production impl: wraps the live `AuthManager`. 401 recovery
/// delegates to `AuthManager::unauthorized_recovery`.
pub struct ShellAuthCredentialProvider {
    auth_manager: Arc<AuthManager>,
    static_credentials: AtelierAuthCredentials,
}
impl ShellAuthCredentialProvider {
    pub(crate) fn new(
        auth_manager: Arc<AuthManager>,
        deployment_key: Option<String>,
        alpha_test_key: Option<String>,
    ) -> Self {
        let mut static_credentials = AtelierAuthCredentials::new(None);
        static_credentials.deployment_key = deployment_key;
        static_credentials.alpha_test_key = alpha_test_key;
        Self {
            auth_manager,
            static_credentials,
        }
    }
}
impl std::fmt::Debug for ShellAuthCredentialProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellAuthCredentialProvider")
            .field("auth_manager", &"<configured>")
            .finish()
    }
}
impl HttpAuth for ShellAuthCredentialProvider {
    fn apply(&self, builder: RequestBuilder, base_url: &str) -> RequestBuilder {
        let mut creds = self.static_credentials.clone();
        if creds.deployment_key.is_none()
            && let Some(auth) = self.auth_manager.current_or_expired()
        {
            creds.user_token = Some(auth.key);
        }
        creds.apply(builder, base_url)
    }
}
#[async_trait::async_trait]
impl AuthCredentialProvider for ShellAuthCredentialProvider {
    fn snapshot(&self) -> CredentialSnapshot {
        if let Some(ref dk) = self.static_credentials.deployment_key {
            return CredentialSnapshot {
                token: Some(dk.clone()),
                deployment_id: crate::managed_config::resolve_deployment_id(Some(dk)),
                ..Default::default()
            };
        }
        let auth = self.auth_manager.current_or_expired();
        let user_id = auth.as_ref().map(|a| a.user_id.clone());
        let team_id = auth.as_ref().and_then(|a| a.team_id.clone());
        let organization_id = auth.as_ref().and_then(|a| a.organization_id.clone());
        let api_key_id = api_key_id_for(auth.as_ref());
        let token = auth.map(|a| a.key);
        CredentialSnapshot {
            token,
            user_id,
            team_id,
            deployment_id: None,
            api_key_id,
            organization_id,
        }
    }
    async fn refresh_after_unauthorized(&self) -> bool {
        if self.static_credentials.deployment_key.is_some() {
            return false;
        }
        self.auth_manager
            .try_recover_unauthorized(crate::auth::recovery::RecoverySource::Background)
            .await
    }
    fn needs_token_auth_header(&self) -> bool {
        self.static_credentials.deployment_key.is_none()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AtelierAuth;
    use crate::auth::AtelierComConfig;
    use crate::auth::manager::AuthManager;
    use atelier_auth::AuthCredentialProvider;
    use chrono::{Duration as ChronoDuration, Utc};
    use std::sync::Mutex;
    /// Serializes tests that pin `ATELIER_AUTH_EARLY_INVALIDATION_SECS`, since
    /// env vars are process-global and parallel tests would race.
    static EARLY_INVALIDATION_LOCK: Mutex<()> = Mutex::new(());
    /// RAII guard: pins `ATELIER_AUTH_EARLY_INVALIDATION_SECS` to the production
    /// default (300s) while held, restoring the previous value on drop.
    /// Acquires `EARLY_INVALIDATION_LOCK` so concurrent test runners can't
    /// observe a half-mutated env.
    struct EarlyInvalidationGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }
    impl EarlyInvalidationGuard {
        fn pin_to_default() -> Self {
            let lock = EARLY_INVALIDATION_LOCK
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let previous = std::env::var("ATELIER_AUTH_EARLY_INVALIDATION_SECS").ok();
            unsafe { std::env::set_var("ATELIER_AUTH_EARLY_INVALIDATION_SECS", "300") };
            Self {
                _lock: lock,
                previous,
            }
        }
    }
    impl Drop for EarlyInvalidationGuard {
        fn drop(&mut self) {
            unsafe {
                match self.previous.take() {
                    Some(prev) => std::env::set_var("ATELIER_AUTH_EARLY_INVALIDATION_SECS", prev),
                    None => std::env::remove_var("ATELIER_AUTH_EARLY_INVALIDATION_SECS"),
                }
            }
        }
    }
    fn make_auth(key: &str, expires_in: ChronoDuration) -> AtelierAuth {
        AtelierAuth {
            key: key.to_string(),
            user_id: "test-user".to_string(),
            create_time: Utc::now(),
            expires_at: Some(Utc::now() + expires_in),
            ..AtelierAuth::test_default()
        }
    }
    /// Build an `AuthManager` rooted at `dir`. Caller keeps `dir` alive for
    /// the duration of the test so the `TempDir` `Drop` actually cleans up.
    fn make_manager(dir: &tempfile::TempDir, initial: Option<AtelierAuth>) -> Arc<AuthManager> {
        let mgr = AuthManager::new(dir.path(), AtelierComConfig::default());
        if let Some(auth) = initial {
            mgr.hot_swap(auth);
        }
        Arc::new(mgr)
    }
    /// `apply()` and `snapshot()` agree (snapshot==wire invariant) when the
    /// in-memory token is fresh.
    #[test]
    fn apply_and_snapshot_agree_on_live_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("live-token"));
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }
    /// During the 5-minute pre-refresh buffer window, `auth_manager.current()`
    /// returns `None` (the token is treated as expired-soon for refresh
    /// scheduling), but the token is still valid at the proxy. The provider
    /// must fall back to `expired_auth()` so the in-memory token gets sent
    /// instead of nothing -- which is the fix for the bulk of the
    /// `POST /v1/storage` 401s observed in production.
    #[test]
    fn falls_back_to_expired_auth_during_buffer_window() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffer-token", ChronoDuration::minutes(4))),
        );
        assert!(mgr.current().is_none(), "buffer-window precondition");
        assert!(mgr.expired_auth().is_some(), "buffer-window precondition");
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("buffer-token"),
            "snapshot should fall back to expired_auth instead of None"
        );
        assert_eq!(snap.user_id.as_deref(), Some("test-user"));
    }
    /// When `auth_manager` has nothing at all (no in-memory auth, expired
    /// or otherwise), `snapshot()` returns `None` for the user-token branch.
    /// `apply()` would then send no Authorization header.
    #[test]
    fn no_token_when_auth_manager_is_empty() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = ShellAuthCredentialProvider::new(mgr, None, None);
        let snap = provider.snapshot();
        assert!(
            snap.token.is_none(),
            "snapshot should be None when manager has no auth"
        );
        assert!(snap.user_id.is_none());
    }
    /// 401 recovery routes through `unauthorized_recovery` (pre-fix
    /// it no-oped because the refresher arg was hardcoded `None`).
    #[tokio::test]
    async fn refresh_after_unauthorized_drives_recovery_state_machine() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = Arc::new(AuthManager::new(
            dir.path(),
            crate::auth::AtelierComConfig::default(),
        ));
        mgr.hot_swap(AtelierAuth {
            key: "stale".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..AtelierAuth::test_default()
        });
        struct OkRefresher {
            calls: Arc<std::sync::atomic::AtomicU32>,
        }
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                crate::auth::refresh::RefreshOutcome::Success(Box::new(AtelierAuth {
                    key: "fresh".into(),
                    auth_mode: crate::auth::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..AtelierAuth::test_default()
                }))
            }
        }
        let calls = Arc::new(std::sync::atomic::AtomicU32::new(0));
        mgr.set_refresher(Arc::new(OkRefresher {
            calls: calls.clone(),
        }));
        let provider = ShellAuthCredentialProvider::new(mgr.clone(), None, None);
        assert!(provider.refresh_after_unauthorized().await);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(mgr.current().unwrap().key, "fresh");
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("fresh"),
            "snapshot must reflect refreshed token for subsequent apply() calls"
        );
    }
    /// Deployment-key path has no recovery (operator owns the bearer).
    #[tokio::test]
    async fn refresh_after_unauthorized_is_noop_for_deployment_key() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key".to_string()), None);
        assert!(!provider.refresh_after_unauthorized().await);
    }
    #[test]
    fn snapshot_populates_tenant_id_per_auth_mode() {
        use atelier_telemetry::config::deployment_id_from_key;
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let dep = ShellAuthCredentialProvider::new(
            make_manager(&dir, None),
            Some("xai-token-EX".into()),
            None,
        )
        .snapshot();
        assert_eq!(
            dep.deployment_id.as_deref(),
            Some(deployment_id_from_key("xai-token-EX").as_str())
        );
        assert!(dep.api_key_id.is_none());
        let api_auth = AtelierAuth {
            key: "sk-apikey-xyz".into(),
            auth_mode: crate::auth::AuthMode::ApiKey,
            expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            ..AtelierAuth::test_default()
        };
        let api = ShellAuthCredentialProvider::new(make_manager(&dir, Some(api_auth)), None, None)
            .snapshot();
        assert_eq!(
            api.api_key_id.as_deref(),
            Some(deployment_id_from_key("sk-apikey-xyz").as_str())
        );
        assert!(api.deployment_id.is_none());
        let oidc = ShellAuthCredentialProvider::new(
            make_manager(
                &dir,
                Some(make_auth("oidc-token", ChronoDuration::hours(1))),
            ),
            None,
            None,
        )
        .snapshot();
        assert!(oidc.deployment_id.is_none() && oidc.api_key_id.is_none());
    }
    /// Bootstrap mode: `snapshot()` re-reads disk so sibling-rotated
    /// tokens are picked up without a live AuthManager.
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[test]
    fn otel_bootstrap_snapshot_picks_up_disk_writes() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let scope = crate::auth::AtelierComConfig::default().auth_scope();
        let auth_path = dir.path().join("auth.json");
        let mgr = make_manager(
            &dir,
            Some(make_auth("initial-token", ChronoDuration::hours(1))),
        );
        let mut store = crate::auth::read_auth_json(&auth_path).unwrap_or_default();
        store.insert(
            scope.clone(),
            make_auth("initial-token", ChronoDuration::hours(1)),
        );
        crate::auth::storage::write_auth_json(&auth_path, &store).unwrap();
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert_eq!(provider.snapshot().token.as_deref(), Some("initial-token"));
        let mut store = crate::auth::read_auth_json(&auth_path).unwrap();
        store.insert(scope, make_auth("rotated-token", ChronoDuration::hours(1)));
        crate::auth::storage::write_auth_json(&auth_path, &store).unwrap();
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-token"),
            "must pick up sibling-rotated tokens from disk"
        );
    }
    /// After `set_live()`, `snapshot()` reads from the live manager's
    /// in-memory cache (no disk re-read) and `refresh_after_unauthorized()`
    /// drives the recovery state machine.
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[tokio::test]
    async fn otel_live_mode_uses_shared_auth_manager() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);
        assert!(provider.snapshot().token.is_none());
        assert!(!provider.refresh_after_unauthorized().await);
        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = make_manager(
            &live_dir,
            Some(make_auth("live-token", ChronoDuration::hours(1))),
        );
        provider.set_live(live_mgr.clone());
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("live-token"),
            "must read from live AuthManager after set_live()"
        );
        live_mgr.hot_swap(make_auth("rotated-live", ChronoDuration::hours(1)));
        assert_eq!(
            provider.snapshot().token.as_deref(),
            Some("rotated-live"),
            "must see rotated token from live manager"
        );
    }
    /// `refresh_after_unauthorized` drives recovery when live.
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[tokio::test]
    async fn otel_live_refresh_after_unauthorized_drives_recovery() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let bootstrap_dir = tempfile::tempdir().unwrap();
        let bootstrap_mgr = make_manager(&bootstrap_dir, None);
        let provider = OtelAuthCredentialProvider::new(bootstrap_mgr);
        let live_dir = tempfile::tempdir().unwrap();
        let live_mgr = Arc::new(AuthManager::new(
            live_dir.path(),
            crate::auth::AtelierComConfig::default(),
        ));
        live_mgr.hot_swap(AtelierAuth {
            key: "stale".into(),
            auth_mode: crate::auth::AuthMode::Oidc,
            create_time: chrono::Utc::now() - ChronoDuration::hours(2),
            user_id: "u".into(),
            refresh_token: Some("rt-stale".into()),
            expires_at: Some(chrono::Utc::now() - ChronoDuration::hours(1)),
            ..AtelierAuth::test_default()
        });
        struct OkRefresher;
        #[async_trait::async_trait]
        impl crate::auth::refresh::TokenRefresher for OkRefresher {
            async fn refresh(
                &self,
                _r: crate::auth::manager::RefreshReason,
            ) -> crate::auth::refresh::RefreshOutcome {
                crate::auth::refresh::RefreshOutcome::Success(Box::new(AtelierAuth {
                    key: "refreshed".into(),
                    auth_mode: crate::auth::AuthMode::Oidc,
                    create_time: chrono::Utc::now(),
                    user_id: "u".into(),
                    refresh_token: Some("rt-new".into()),
                    expires_at: Some(chrono::Utc::now() + ChronoDuration::hours(1)),
                    ..AtelierAuth::test_default()
                }))
            }
        }
        live_mgr.set_refresher(Arc::new(OkRefresher));
        provider.set_live(live_mgr.clone());
        assert!(
            provider.refresh_after_unauthorized().await,
            "live mode must drive recovery"
        );
        assert_eq!(live_mgr.current().unwrap().key, "refreshed");
    }
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[test]
    fn otel_deployment_key_sent_when_no_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, None);
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("enterprise-key".to_string());
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("enterprise-key"),
            "deployment key must be sent when no OIDC token exists"
        );
    }
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[test]
    fn otel_deployment_key_wins_over_oidc_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("oidc-token", ChronoDuration::hours(1))),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        provider.set_deployment_key("deployment-key-123".to_string());
        let snap = provider.snapshot();
        assert_eq!(
            snap.token.as_deref(),
            Some("deployment-key-123"),
            "deployment key must win over OIDC token"
        );
        assert!(snap.user_id.is_none());
    }
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[test]
    fn has_usable_credential_reflects_auth_state() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "valid unexpired token is usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("stale", ChronoDuration::hours(-1))));
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            !provider.has_usable_credential(),
            "expired token is not usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        assert!(
            !provider.has_usable_credential(),
            "absent token is not usable"
        );
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(&dir, Some(make_auth("live", ChronoDuration::hours(1))));
        mgr.record_permanent_failure(
            "live".into(),
            crate::auth::error::RefreshTokenFailedReason::RefreshTokenRejected.into(),
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a wire-valid token stays usable despite a permanent refresh verdict"
        );
        let dir = tempfile::tempdir().unwrap();
        let provider = OtelAuthCredentialProvider::new(make_manager(&dir, None));
        provider.set_deployment_key("enterprise-key".to_string());
        assert!(
            provider.has_usable_credential(),
            "static deployment key is always usable"
        );
    }
    /// A token inside the early-invalidation buffer is still accepted by the
    /// proxy (the buffer is a client-side pre-refresh margin, not a wire
    /// expiry), and the sender puts it on the wire via `current_or_expired()`.
    /// The export gate must therefore keep it usable even though `current()`
    /// reports `None`. Regression for the buffer-window export drop.
    #[cfg(any())] // Remote OTLP exporter authentication was removed; observability is local-only.
    #[test]
    fn has_usable_credential_true_inside_early_invalidation_buffer() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("buffered", ChronoDuration::minutes(4))),
        );
        assert!(
            mgr.current().is_none(),
            "4-min token sits inside the pinned 5-min buffer, so current() is None"
        );
        let provider = OtelAuthCredentialProvider::new(mgr);
        assert!(
            provider.has_usable_credential(),
            "a buffer-window token is still wire-valid, so the gate keeps it usable"
        );
    }
    /// A configured `deployment_key` always wins over the AuthManager-resolved
    /// user token, matching the precedence in `AtelierAuthCredentials::apply`.
    /// The snapshot must report the deployment key (not the user token) so
    /// the 401-attribution prefix matches the wire bytes.
    #[test]
    fn deployment_key_wins_over_resolved_user_token() {
        let _guard = EarlyInvalidationGuard::pin_to_default();
        let dir = tempfile::tempdir().unwrap();
        let mgr = make_manager(
            &dir,
            Some(make_auth("user-token", ChronoDuration::hours(1))),
        );
        let provider =
            ShellAuthCredentialProvider::new(mgr, Some("deployment-key-12345".to_string()), None);
        let snap = provider.snapshot();
        assert_eq!(snap.token.as_deref(), Some("deployment-key-12345"));
        assert!(snap.user_id.is_none());
    }
}
