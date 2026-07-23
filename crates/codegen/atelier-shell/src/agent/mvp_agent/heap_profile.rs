//! Heap-profile monitor wiring for [`MvpAgent`].
//!
//! Full-reapply sites call [`MvpAgent::reconfigure_heap_profile_monitor`].
//! K12 scoped kill-switch reconfigures only jemalloc fields (no wholesale
//! `remote_settings` rewrite, no `re_resolve_runtime_fields` / telemetry re-init).

use super::*;
use crate::heap_profile::build_upload_handles;

impl MvpAgent {
    pub(super) fn reconfigure_heap_profile_monitor(&self) {
        let zdr = self.is_data_collection_disabled();
        let config = self.cfg.borrow().resolve_jemalloc_heap_profile(zdr);
        let handles = self.heap_profile_upload_handles();
        self.heap_profile_monitor
            .borrow_mut()
            .reconfigure(config, handles);
    }

    pub(super) fn heap_profile_set_session_id(&self, session_id: &str) {
        self.heap_profile_monitor
            .borrow_mut()
            .set_session_id(session_id.to_owned());
    }

    fn heap_profile_upload_handles(&self) -> Option<crate::heap_profile::HeapProfileUploadHandles> {
        let method = self.trace_upload_config_snapshot()?;
        let bucket_url = self
            .cfg
            .borrow()
            .endpoints
            .resolve_trace_bucket_url()
            .map(|r| r.value);
        // Only direct GCS uploads need a bucket.
        if bucket_url.is_none()
            && matches!(
                method,
                crate::session::repo_changes::UploadMethod::Direct { .. }
            )
        {
            tracing::debug!("no trace bucket configured; heap-profile uploads disabled");
            return None;
        }
        Some(build_upload_handles(
            Arc::clone(&self.auth_manager),
            bucket_url,
            method,
        ))
    }

    /// Background poll + scoped kill-switch (agent entrypoints only).
    /// Idempotent; skipped under `cfg!(test)`.
    pub(super) fn spawn_heap_profile_monitor(&self) {
        if cfg!(test) || self.heap_profile_started.replace(true) {
            return;
        }
        self.reconfigure_heap_profile_monitor();
        let agent_ref = LocalRef::new(self);
        tokio::task::spawn_local(async move {
            loop {
                let poll_interval = {
                    let mon = agent_ref.get().heap_profile_monitor.borrow();
                    if mon.config().enabled {
                        mon.config().poll_interval
                    } else {
                        std::time::Duration::from_secs(30)
                    }
                };
                tokio::time::sleep(poll_interval).await;

                let enabled = agent_ref
                    .get()
                    .heap_profile_monitor
                    .borrow()
                    .config()
                    .enabled;
                if !enabled {
                    continue;
                }

                let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                    agent_ref.get().heap_profile_poll_tick_once(),
                ))
                .await;
                if result.is_err() {
                    agent_ref
                        .get()
                        .heap_profile_monitor
                        .borrow_mut()
                        .clear_upload_in_flight();
                    tracing::error!("heap_profile: poll tick panicked; continuing");
                }
            }
        });
    }

    async fn heap_profile_poll_tick_once(&self) {
        let pending = {
            let mut mon = self.heap_profile_monitor.borrow_mut();
            mon.begin_tick()
        };
        let Some(pending) = pending else {
            return;
        };
        let threshold = pending.threshold;
        let outcome = pending.execute().await;
        self.heap_profile_monitor
            .borrow_mut()
            .finish_tick(threshold, outcome);
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with_remote(rs: crate::util::config::RemoteSettings) -> AgentConfig {
        AgentConfig {
            remote_settings: Some(rs),
            ..Default::default()
        }
    }

    #[test]
    fn scoped_resolve_does_not_mutate_stored_remote_settings() {
        let cfg = cfg_with_remote(crate::util::config::RemoteSettings {
            jemalloc_heap_profile_enabled: Some(true),
            jemalloc_heap_profile_thresholds_bytes: Some(vec![1_000_000]),
            jemalloc_heap_profile_poll_interval_secs: Some(30),
            trace_upload_enabled: Some(true),
            telemetry_mode: Some("all".into()),
            ..Default::default()
        });

        let resolved = cfg.resolve_jemalloc_heap_profile_from_partial(
            Some(false),
            Some(&[1_000_000]),
            Some(30),
            false,
        );
        assert!(!resolved.enabled);

        let stored = cfg.remote_settings.as_ref().unwrap();
        assert_eq!(stored.jemalloc_heap_profile_enabled, Some(true));
        assert_eq!(
            stored.jemalloc_heap_profile_thresholds_bytes.as_deref(),
            Some([1_000_000u64].as_slice())
        );
    }

    /// Kill-switch poll patches stored jemalloc knobs so full reapply (`/new`)
    /// cannot re-enable from a stale flag when wholesale refresh is skipped.
    #[test]
    fn kill_switch_patch_keeps_full_reapply_disabled() {
        let mut cfg = cfg_with_remote(crate::util::config::RemoteSettings {
            jemalloc_heap_profile_enabled: Some(true),
            jemalloc_heap_profile_thresholds_bytes: Some(vec![1_000_000]),
            jemalloc_heap_profile_poll_interval_secs: Some(30),
            trace_upload_enabled: Some(true),
            telemetry_mode: Some("all".into()),
            ..Default::default()
        });

        // Simulate live kill-switch fetch: remote disabled jemalloc profiling.
        if let Some(rs) = cfg.remote_settings.as_mut() {
            rs.jemalloc_heap_profile_enabled = Some(false);
            rs.jemalloc_heap_profile_thresholds_bytes = Some(vec![1_000_000]);
            rs.jemalloc_heap_profile_poll_interval_secs = Some(30);
        }

        // Full reapply path reads stored fields (hooks available for gate check).
        let free = crate::heap_profile::resolve_jemalloc_heap_profile(
            cfg.remote_settings
                .as_ref()
                .and_then(|s| s.jemalloc_heap_profile_enabled),
            cfg.remote_settings
                .as_ref()
                .and_then(|s| s.jemalloc_heap_profile_thresholds_bytes.as_deref()),
            cfg.remote_settings
                .as_ref()
                .and_then(|s| s.jemalloc_heap_profile_poll_interval_secs),
            false,
            true,
            true,
        );
        assert!(!free.enabled);
        assert_eq!(
            cfg.remote_settings
                .as_ref()
                .and_then(|s| s.jemalloc_heap_profile_enabled),
            Some(false)
        );
    }

    #[test]
    fn scoped_partial_gates_and_free_resolve_agree() {
        let thresholds = [2u64 * 1024 * 1024 * 1024];
        let free = crate::heap_profile::resolve_jemalloc_heap_profile(
            Some(true),
            Some(&thresholds),
            Some(15),
            false,
            true,
            true,
        );
        assert!(free.enabled);
        assert_eq!(free.poll_interval, std::time::Duration::from_secs(15));
        assert_eq!(free.thresholds, thresholds);

        let cfg = cfg_with_remote(crate::util::config::RemoteSettings {
            trace_upload_enabled: Some(true),
            telemetry_mode: Some("all".into()),
            jemalloc_heap_profile_enabled: Some(false),
            jemalloc_heap_profile_thresholds_bytes: Some(vec![100]),
            ..Default::default()
        });
        assert!(
            !cfg.resolve_jemalloc_heap_profile_from_partial(
                Some(false),
                Some(&[100]),
                Some(30),
                false
            )
            .enabled
        );
        assert!(
            !cfg.resolve_jemalloc_heap_profile_from_partial(Some(true), Some(&[]), Some(30), false)
                .enabled
        );
        assert!(
            !cfg.resolve_jemalloc_heap_profile_from_partial(
                Some(true),
                Some(&[100]),
                Some(30),
                true
            )
            .enabled
        );
    }

    #[test]
    fn full_reapply_reads_stored_remote_jemalloc_fields() {
        let thresholds = vec![100u64, 200];
        let cfg = cfg_with_remote(crate::util::config::RemoteSettings {
            jemalloc_heap_profile_enabled: Some(true),
            jemalloc_heap_profile_thresholds_bytes: Some(thresholds.clone()),
            jemalloc_heap_profile_poll_interval_secs: Some(45),
            trace_upload_enabled: Some(true),
            telemetry_mode: Some("all".into()),
            ..Default::default()
        });

        let full = cfg.resolve_jemalloc_heap_profile(false);
        // Without installed hooks, prof_available is false → disabled.
        assert!(!full.enabled);
        assert_eq!(full.poll_interval, std::time::Duration::from_secs(45));
        assert_eq!(full.thresholds, vec![100, 200]);

        assert!(
            !cfg.resolve_jemalloc_heap_profile_from_partial(
                Some(false),
                Some(&[100, 200]),
                Some(45),
                false
            )
            .enabled
        );
        assert!(
            !cfg.resolve_jemalloc_heap_profile_from_partial(
                Some(true),
                Some(&[100]),
                Some(45),
                true
            )
            .enabled
        );

        let free = crate::heap_profile::resolve_jemalloc_heap_profile(
            Some(true),
            Some(&thresholds),
            Some(45),
            false,
            true,
            true,
        );
        assert!(free.enabled);
        assert_eq!(free.thresholds, vec![100, 200]);
        assert_eq!(free.poll_interval, std::time::Duration::from_secs(45));
    }
}
